//! Optional, lossy analysis of the samples consumed by playback. Never an audio effect.
use crossbeam_queue::ArrayQueue;
use rodio::Source;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub const BANDS: usize = 32;
const FFT_SIZE: usize = 4096;
const BLOCK_SIZE: usize = 256;
// Display headroom makes ordinary music legible; this is not a calibrated meter.
const FLOOR_DB: f32 = -70.0;
const CEILING_DB: f32 = -10.0;
const INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpectrumFrame {
    pub generation: u64,
    pub current_id: Option<String>,
    pub active: bool,
    pub low_hz: f32,
    pub high_hz: f32,
    pub levels: [f32; BANDS],
}

struct Samples {
    generation: u64,
    epoch: u64,
    sequence: u64,
    rate: u32,
    pcm: [[f32; BLOCK_SIZE]; 2],
}

pub struct Spectrum {
    samples: ArrayQueue<Samples>,
    generation: AtomicU64,
    epoch: AtomicU64,
    subscribers: AtomicUsize,
    playing: AtomicBool,
    current_id: Mutex<Option<String>>,
    frames: watch::Sender<SpectrumFrame>,
}

impl Spectrum {
    pub fn start() -> std::io::Result<Arc<Self>> {
        let spectrum = Arc::new(Self::default());
        let weak = Arc::downgrade(&spectrum);
        std::thread::Builder::new()
            .name("vtamp-spectrum".into())
            .spawn(move || {
                let mut analyzer = Analyzer::new();
                loop {
                    {
                        let Some(spectrum) = weak.upgrade() else {
                            break;
                        };
                        analyzer.update(&spectrum);
                    }
                    std::thread::sleep(INTERVAL);
                }
            })?;
        Ok(spectrum)
    }

    pub fn context(&self, id: Option<String>) {
        *self.current_id.lock().unwrap() = id;
    }

    pub(crate) fn playing(&self, playing: bool) {
        if self.playing.swap(playing, Ordering::AcqRel) != playing {
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn reset(&self) {
        self.playing(false);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn subscribe(self: &Arc<Self>) -> Subscription {
        if self.subscribers.fetch_add(1, Ordering::AcqRel) == 0 {
            self.epoch.fetch_add(1, Ordering::AcqRel);
            self.frames.send_replace(SpectrumFrame::default());
        }
        Subscription {
            spectrum: self.clone(),
            frames: self.frames.subscribe(),
        }
    }

    fn enabled(&self) -> bool {
        self.subscribers.load(Ordering::Acquire) > 0 && self.playing.load(Ordering::Acquire)
    }

    pub(crate) fn tap(self: &Arc<Self>, source: Box<dyn Source + Send>) -> Tap {
        Tap {
            source,
            spectrum: self.clone(),
            generation: self.generation.load(Ordering::Acquire),
            epoch: self.epoch.load(Ordering::Acquire),
            pcm: [[0.0; BLOCK_SIZE]; 2],
            filled: 0,
            channel: 0,
            sequence: 0,
        }
    }
}

impl Default for Spectrum {
    fn default() -> Self {
        Self {
            samples: ArrayQueue::new(32),
            generation: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            subscribers: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            current_id: Mutex::new(None),
            frames: watch::channel(SpectrumFrame::default()).0,
        }
    }
}

pub struct Subscription {
    spectrum: Arc<Spectrum>,
    pub frames: watch::Receiver<SpectrumFrame>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.spectrum.subscribers.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct Tap {
    source: Box<dyn Source + Send>,
    spectrum: Arc<Spectrum>,
    generation: u64,
    epoch: u64,
    pcm: [[f32; BLOCK_SIZE]; 2],
    filled: usize,
    channel: usize,
    sequence: u64,
}
impl Iterator for Tap {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let sample = self.source.next()?;
        let channels = self.source.channels().get() as usize;
        let epoch = self.spectrum.epoch.load(Ordering::Acquire);
        if epoch != self.epoch {
            self.epoch = epoch;
            self.filled = 0;
        }
        if self.spectrum.enabled()
            && self.generation == self.spectrum.generation.load(Ordering::Acquire)
        {
            // Analyze mono or the front stereo pair; never sum opposing phases.
            if self.channel < 2 {
                self.pcm[self.channel][self.filled] = if sample.is_finite() { sample } else { 0.0 };
            }
            if self.channel + 1 == channels {
                if channels == 1 {
                    self.pcm[1][self.filled] = self.pcm[0][self.filled];
                }
                self.filled += 1;
                if self.filled == BLOCK_SIZE {
                    self.spectrum.samples.force_push(Samples {
                        generation: self.generation,
                        epoch,
                        sequence: self.sequence,
                        rate: self.source.sample_rate().get(),
                        pcm: self.pcm,
                    });
                    self.sequence += 1;
                    self.filled = 0;
                }
            }
        } else {
            self.filled = 0;
        }
        self.channel = (self.channel + 1) % channels;
        Some(sample)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.source.size_hint()
    }
}
impl Source for Tap {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.source.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.filled = 0;
        self.channel = 0;
        self.sequence += 1;
        self.source.try_seek(pos)
    }
}

struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    input: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    window: Vec<f32>,
    pcm: [[f32; FFT_SIZE]; 2],
    cursor: usize,
    filled: usize,
    key: (u64, u64, u32),
    sequence: Option<u64>,
    last_sample: Instant,
}
impl Analyzer {
    fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            fft,
            scratch,
            input: vec![Complex::default(); FFT_SIZE],
            window: (0..FFT_SIZE)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos())
                .collect(),
            pcm: [[0.0; FFT_SIZE]; 2],
            cursor: 0,
            filled: 0,
            key: (0, 0, 0),
            sequence: None,
            last_sample: Instant::now(),
        }
    }
    fn clear(&mut self) {
        self.cursor = 0;
        self.filled = 0;
        self.sequence = None;
    }
    fn push(&mut self, samples: Samples) {
        let key = (samples.generation, samples.epoch, samples.rate);
        if self.key != key || self.sequence.is_some_and(|n| samples.sequence != n + 1) {
            self.clear();
        }
        self.key = key;
        self.sequence = Some(samples.sequence);
        for i in 0..BLOCK_SIZE {
            for ch in 0..2 {
                self.pcm[ch][self.cursor] = samples.pcm[ch][i];
            }
            self.cursor = (self.cursor + 1) % FFT_SIZE;
        }
        self.filled = (self.filled + BLOCK_SIZE).min(FFT_SIZE);
        self.last_sample = Instant::now();
    }
    fn levels(&mut self, rate: u32) -> [f32; BANDS] {
        let mut power = [0.0_f32; FFT_SIZE / 2 + 1];
        for ch in 0..2 {
            for i in 0..FFT_SIZE {
                self.input[i] = Complex::new(
                    self.pcm[ch][(self.cursor + i) % FFT_SIZE] * self.window[i],
                    0.0,
                );
            }
            self.fft
                .process_with_scratch(&mut self.input, &mut self.scratch);
            for (p, bin) in power.iter_mut().zip(&self.input) {
                *p += bin.norm_sqr() * 0.5;
            }
        }
        let high = (rate as f32 / 2.0).min(16_000.0);
        if high <= 40.0 {
            return [0.0; BANDS];
        }
        std::array::from_fn(|band| {
            let low = 40.0 * (high / 40.0).powf(band as f32 / BANDS as f32);
            let upper = 40.0 * (high / 40.0).powf((band + 1) as f32 / BANDS as f32);
            let start =
                ((low * FFT_SIZE as f32 / rate as f32).round() as usize).clamp(1, FFT_SIZE / 2);
            let end = ((upper * FFT_SIZE as f32 / rate as f32).round() as usize)
                .clamp(start, FFT_SIZE / 2);
            // Peak magnitude per band keeps a narrow tone legible without inflating wide bands.
            let peak = power[start..=end].iter().copied().fold(0.0, f32::max);
            let amplitude = peak.sqrt() * 4.0 / FFT_SIZE as f32;
            ((20.0 * amplitude.max(1e-10).log10() - FLOOR_DB) / (CEILING_DB - FLOOR_DB))
                .clamp(0.0, 1.0)
        })
    }
    fn update(&mut self, spectrum: &Spectrum) {
        let generation = spectrum.generation.load(Ordering::Acquire);
        let epoch = spectrum.epoch.load(Ordering::Acquire);
        let enabled = spectrum.enabled();
        // A bounded drain also bounds work under a very fast producer.
        for _ in 0..32 {
            let Some(samples) = spectrum.samples.pop() else {
                break;
            };
            if enabled && samples.generation == generation && samples.epoch == epoch {
                self.push(samples);
            }
        }
        if !enabled || self.key.0 != generation || self.key.1 != epoch {
            self.clear();
        }
        if spectrum.subscribers.load(Ordering::Acquire) == 0 {
            return;
        }
        let active = enabled
            && self.filled == FFT_SIZE
            && self.last_sample.elapsed() < Duration::from_millis(250);
        let levels = if active {
            self.levels(self.key.2)
        } else {
            [0.0; BANDS]
        };
        if spectrum.generation.load(Ordering::Acquire) != generation
            || spectrum.epoch.load(Ordering::Acquire) != epoch
        {
            return;
        }
        spectrum.frames.send_replace(SpectrumFrame {
            generation,
            current_id: spectrum.current_id.lock().unwrap().clone(),
            active,
            low_hz: 40.0,
            high_hz: (self.key.2 as f32 / 2.0).min(16_000.0),
            levels,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, frequency: f32, amplitude: f32, phase: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .flat_map(|i| {
                let sample =
                    (std::f32::consts::TAU * frequency * i as f32 / rate as f32).sin() * amplitude;
                [sample, sample * phase]
            })
            .collect()
    }
    fn analyze(data: Vec<f32>, rate: u32) -> [f32; BANDS] {
        let spectrum = Arc::new(Spectrum::default());
        let _subscription = spectrum.subscribe();
        spectrum.playing(true);
        let source = rodio::buffer::SamplesBuffer::new(
            2.try_into().unwrap(),
            rate.try_into().unwrap(),
            data,
        );
        spectrum.tap(Box::new(source)).for_each(drop);
        let mut analyzer = Analyzer::new();
        analyzer.update(&spectrum);
        let frame = spectrum.frames.borrow().clone();
        assert!(frame.active);
        frame.levels
    }
    #[test]
    fn fft_locates_tones_and_preserves_opposing_stereo_energy() {
        for rate in [44_100, 48_000, 96_000] {
            for frequency in [100.0, 1000.0, 8000.0] {
                // Stay below the display ceiling so a clipped plateau cannot move the peak.
                let positive = analyze(tone(rate, frequency, 0.2, 1.0), rate);
                let opposing = analyze(tone(rate, frequency, 0.2, -1.0), rate);
                assert_eq!(positive, opposing);
                let peak = positive
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap();
                let expected = ((frequency / 40.0_f32).ln() / (16_000.0_f32 / 40.0).ln()
                    * BANDS as f32) as usize;
                assert!(
                    peak.0.abs_diff(expected) <= 1,
                    "rate={rate}, frequency={frequency}, peak={peak:?}"
                );
                assert!(*peak.1 > 0.8);
            }
        }
        assert_eq!(analyze(vec![0.0; FFT_SIZE * 2], 48_000), [0.0; BANDS]);
        let loud = analyze(tone(48_000, 1000.0, 0.5, 1.0), 48_000);
        let quiet = analyze(tone(48_000, 1000.0, 0.005, 1.0), 48_000);
        assert!(
            loud.iter().copied().fold(0.0, f32::max)
                > quiet.iter().copied().fold(0.0, f32::max) + 0.5
        );
    }
    #[test]
    fn tap_preserves_samples_under_overflow_and_does_nothing_without_demand() {
        let spectrum = Arc::new(Spectrum::default());
        spectrum.playing(true);
        let pcm = tone(48_000, 500.0, 0.8, -1.0).repeat(8);
        let source = || {
            Box::new(rodio::buffer::SamplesBuffer::new(
                2.try_into().unwrap(),
                48_000.try_into().unwrap(),
                pcm.clone(),
            )) as Box<dyn Source + Send>
        };
        assert_eq!(spectrum.tap(source()).collect::<Vec<_>>(), pcm);
        assert!(spectrum.samples.is_empty());
        let subscription = spectrum.subscribe();
        let tap = spectrum.tap(source());
        assert_eq!(tap.channels().get(), 2);
        assert_eq!(tap.sample_rate().get(), 48_000);
        assert_eq!(tap.total_duration(), source().total_duration());
        assert_eq!(tap.collect::<Vec<_>>(), pcm);
        assert_eq!(spectrum.samples.len(), 32);
        drop(subscription);
        assert_eq!(spectrum.subscribers.load(Ordering::Acquire), 0);
    }
    #[test]
    fn pause_seek_and_demand_epochs_discard_old_analysis() {
        let spectrum = Arc::new(Spectrum::default());
        let subscription = spectrum.subscribe();
        spectrum.playing(true);
        let source = || {
            Box::new(rodio::buffer::SamplesBuffer::new(
                2.try_into().unwrap(),
                48_000.try_into().unwrap(),
                tone(48_000, 1000.0, 0.5, 1.0),
            )) as Box<dyn Source + Send>
        };
        spectrum.tap(source()).for_each(drop);
        let mut analyzer = Analyzer::new();
        analyzer.update(&spectrum);
        assert!(spectrum.frames.borrow().active);
        spectrum.playing(false);
        analyzer.update(&spectrum);
        assert!(!spectrum.frames.borrow().active);
        assert_eq!(spectrum.frames.borrow().levels, [0.0; BANDS]);
        spectrum.playing(true);
        let old = spectrum.tap(source());
        spectrum.reset();
        spectrum.playing(true);
        old.for_each(drop);
        assert!(spectrum.samples.is_empty());
        spectrum.tap(source()).for_each(drop);
        drop(subscription);
        let _new_subscription = spectrum.subscribe();
        analyzer.update(&spectrum);
        assert!(!spectrum.frames.borrow().active);
        spectrum.tap(source()).for_each(drop);
        analyzer.update(&spectrum);
        assert!(spectrum.frames.borrow().active);
        assert_eq!(spectrum.frames.borrow().generation, 1);
    }
}
