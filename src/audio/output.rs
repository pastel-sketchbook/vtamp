//! One persistent CPAL stream, with bounded ownership transfers at buffer boundaries.
//! Sources are prepared and reclaimed by the control thread, never by render().
use anyhow::{Context, Result};
use crossbeam_queue::ArrayQueue;
use rodio::{
    ChannelCount, SampleRate, Source,
    cpal::{
        self,
        traits::{DeviceTrait, StreamTrait},
    },
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(super) struct VoiceState {
    pub cancelled: AtomicBool,
    pub finished: AtomicBool,
}

struct Voice {
    source: Box<dyn Source + Send>,
    state: Arc<VoiceState>,
}

struct Shared {
    // Only the control thread produces pending voices. It removes superseded
    // requests itself; the callback never needs to discard an unplayed Box.
    pending: ArrayQueue<Box<Voice>>,
    // Only the callback produces retired voices; only control reclaims them.
    retired: ArrayQueue<Box<Voice>>,
    gain: AtomicU32,
    timing: Timing,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            pending: ArrayQueue::new(1),
            retired: ArrayQueue::new(2),
            gain: AtomicU32::new(1.0f32.to_bits()),
            timing: Timing::default(),
        }
    }
}

#[derive(Default)]
struct Timing {
    callbacks: AtomicU64,
    min_frames: AtomicU64,
    max_frames: AtomicU64,
    max_gap_us: AtomicU64,
    max_excess_us: AtomicU64,
    max_render_us: AtomicU64,
    late: AtomicU64,
    over_budget: AtomicU64,
}

impl Timing {
    fn record(
        &self,
        frames: u64,
        budget: Duration,
        gap: Option<(Duration, Duration)>,
        elapsed: Duration,
    ) {
        // Encode min as an inverted value so zero is a valid initial state.
        self.min_frames
            .fetch_max(u64::MAX - frames, Ordering::Relaxed);
        self.max_frames.fetch_max(frames, Ordering::Relaxed);
        self.max_render_us
            .fetch_max(micros(elapsed), Ordering::Relaxed);
        if elapsed > budget {
            self.over_budget.fetch_add(1, Ordering::Relaxed);
        }
        if let Some((gap, previous_budget)) = gap {
            self.max_gap_us.fetch_max(micros(gap), Ordering::Relaxed);
            self.max_excess_us.fetch_max(
                micros(gap.saturating_sub(previous_budget)),
                Ordering::Relaxed,
            );
            // A conservative diagnostic, not a trigger for output recovery.
            if gap > previous_budget * 2 {
                self.late.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.callbacks.fetch_add(1, Ordering::Release);
    }
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

struct Renderer {
    shared: Arc<Shared>,
    current: Option<Box<Voice>>,
    channels: usize,
    rate: u32,
    previous: Option<(Instant, Duration)>,
}

impl Renderer {
    fn render(&mut self, data: &mut [f32]) {
        // Reserve retirement space before taking ownership of a replacement.
        // If control is delayed, keep the current voice (or silence if cancelled).
        // No wait and no allocation/deallocation, including at EOF and transitions.
        if !self.shared.retired.is_full()
            && let Some(next) = self.shared.pending.pop()
            && let Some(old) = self.current.replace(next)
        {
            // A single producer and a prior capacity check guarantee success.
            let result = self.shared.retired.push(old);
            debug_assert!(result.is_ok());
        }
        data.fill(0.0);
        let Some(voice) = self.current.as_mut() else {
            return;
        };
        if voice.state.cancelled.load(Ordering::Acquire)
            || voice.state.finished.load(Ordering::Acquire)
        {
            return;
        }
        let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        // CPAL delivers interleaved whole frames. Never consume a partial frame
        // if a backend unexpectedly supplies an incomplete output buffer.
        let len = data.len() / self.channels * self.channels;
        for sample in &mut data[..len] {
            match voice.source.next() {
                Some(value) => *sample = value * gain,
                None => {
                    voice.state.finished.store(true, Ordering::Release);
                    break;
                }
            }
        }
    }

    fn callback(&mut self, data: &mut [f32]) {
        let start = Instant::now();
        let frames = (data.len() / self.channels) as u64;
        let budget = Duration::from_secs_f64(frames as f64 / f64::from(self.rate));
        let gap = self
            .previous
            .map(|(last, budget)| (start.saturating_duration_since(last), budget));
        self.render(data);
        self.shared
            .timing
            .record(frames, budget, gap, start.elapsed());
        self.previous = Some((start, budget));
    }
}

pub(super) struct Output {
    // Drop the stream (and its closure) on control before the shared queues.
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    pub id: u64,
    pub channels: ChannelCount,
    pub rate: SampleRate,
    last_report: Instant,
    reported_callbacks: u64,
}

impl Output {
    pub fn open(device: &cpal::Device, error: Arc<AtomicBool>) -> Result<Self> {
        let default = device
            .default_output_config()
            .context("Cannot configure audio output")?;
        let mut config = default.config();
        config.buffer_size = cpal::BufferSize::Default;
        let channels = ChannelCount::new(config.channels).context("Output has no channels")?;
        let rate = SampleRate::new(config.sample_rate).context("Output has no sample rate")?;
        let shared = Arc::new(Shared::default());
        let mut renderer = Renderer {
            shared: shared.clone(),
            current: None,
            channels: usize::from(config.channels),
            rate: config.sample_rate,
            previous: None,
        };
        // CoreAudio accepts interleaved float client PCM, independently of the
        // hardware representation. Keep the device's current rate and channels;
        // do not negotiate a new device-wide rate or fixed buffer size.
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| renderer.callback(data),
                move |_| {
                    error.store(true, Ordering::Release);
                },
                None,
            )
            .context("Cannot open float PCM audio output")?;
        stream.play().context("Cannot start audio output")?;
        let id = NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed);
        tracing::info!(stream_id = id, device = ?device.description().ok(), ?config,
            client_format = "F32", hardware_format = ?default.sample_format(), "Audio output opened");
        Ok(Self {
            _stream: stream,
            id,
            shared,
            channels,
            rate,
            last_report: Instant::now(),
            reported_callbacks: 0,
        })
    }

    pub fn collect(&self) {
        self.shared.collect();
    }
    pub fn play(&self, source: Box<dyn Source + Send>, volume: u8) -> Arc<VoiceState> {
        self.shared.play(source, volume)
    }
    pub fn volume(&self, volume: u8) {
        self.shared.volume(volume);
    }

    pub fn report(&mut self, force: bool, path: Option<&Path>, position_ms: u64) {
        if !force && self.last_report.elapsed() < Duration::from_secs(5) {
            return;
        }
        let t = &self.shared.timing;
        let callbacks = t.callbacks.load(Ordering::Acquire);
        if callbacks != self.reported_callbacks {
            tracing::info!(
                stream_id = self.id,
                ?path,
                position_ms,
                callbacks,
                min_frames = u64::MAX - t.min_frames.load(Ordering::Relaxed),
                max_frames = t.max_frames.load(Ordering::Relaxed),
                max_gap_us = t.max_gap_us.load(Ordering::Relaxed),
                max_gap_excess_us = t.max_excess_us.load(Ordering::Relaxed),
                max_render_us = t.max_render_us.load(Ordering::Relaxed),
                late_callbacks = t.late.load(Ordering::Relaxed),
                over_budget = t.over_budget.load(Ordering::Relaxed),
                "Audio output timing"
            );
        }
        self.reported_callbacks = callbacks;
        self.last_report = Instant::now();
    }
}

impl Shared {
    pub fn collect(&self) {
        while self.retired.pop().is_some() {}
    }

    pub fn play(&self, source: Box<dyn Source + Send>, volume: u8) -> Arc<VoiceState> {
        self.collect();
        // Superseded requests have never been observed by the callback.
        while self.pending.pop().is_some() {}
        self.volume(volume);
        let state = Arc::new(VoiceState::default());
        let voice = Box::new(Voice {
            source,
            state: state.clone(),
        });
        assert!(self.pending.push(voice).is_ok());
        state
    }

    pub fn volume(&self, volume: u8) {
        self.gain
            .store((f32::from(volume) / 100.0).to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::buffered::DecoderWorker;
    use rodio::buffer::SamplesBuffer;
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    // Thread-local accounting excludes concurrent decoders and unrelated tests.
    // Track frees as well: retaining PCM alone would miss Box/source destruction.
    struct Allocator;
    thread_local! {
        static TRACK: Cell<bool> = const { Cell::new(false) };
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
        static FREES: Cell<usize> = const { Cell::new(0) };
    }
    fn count(counter: &'static std::thread::LocalKey<Cell<usize>>) {
        if TRACK.try_with(Cell::get).unwrap_or(false) {
            let _ = counter.try_with(|n| n.set(n.get() + 1));
        }
    }
    // SAFETY: Delegates each operation unchanged to the system allocator.
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count(&ALLOCS);
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count(&ALLOCS);
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            count(&FREES);
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count(&ALLOCS);
            count(&FREES);
            unsafe { System.realloc(ptr, layout, size) }
        }
    }
    #[global_allocator]
    static ALLOCATOR: Allocator = Allocator;

    fn realtime(f: impl FnOnce()) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                TRACK.set(false);
            }
        }
        ALLOCS.set(0);
        FREES.set(0);
        TRACK.set(true);
        let reset = Reset;
        f();
        drop(reset);
        assert_eq!(ALLOCS.get(), 0, "allocation in output callback");
        assert_eq!(FREES.get(), 0, "deallocation in output callback");
    }

    fn renderer() -> Renderer {
        Renderer {
            shared: Arc::new(Shared::default()),
            current: None,
            channels: 2,
            rate: 48_000,
            previous: None,
        }
    }

    fn samples(data: Vec<f32>) -> Box<dyn Source + Send> {
        Box::new(SamplesBuffer::new(
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
            data,
        ))
    }

    #[test]
    fn startup_volume_eof_and_replacement_never_allocate_or_free_in_callback() {
        let mut renderer = renderer();
        let shared = renderer.shared.clone();
        let mut out = [9.0; 4];
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.0; 4]);
        let first = shared.play(samples(vec![0.2, -0.4, 0.6, -0.8, 1.0, -1.0]), 50);
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.1, -0.2, 0.3, -0.4]);
        assert!(!first.finished.load(Ordering::Acquire));
        shared.volume(100);
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [1.0, -1.0, 0.0, 0.0]);
        assert!(first.finished.load(Ordering::Acquire));
        for _ in 0..3 {
            realtime(|| renderer.callback(&mut out));
        }
        assert_eq!(out, [0.0; 4]);
        let second = shared.play(samples(vec![0.5; 8]), 100);
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.5; 4]);
        assert_eq!(shared.retired.len(), 1);
        second.cancelled.store(true, Ordering::Release);
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.0; 4]);
        assert!(!second.finished.load(Ordering::Acquire));
        shared.collect(); // All old boxes, decoder buffers and handles die here.
        assert!(shared.retired.is_empty());
    }

    #[test]
    fn rapid_replacements_and_delayed_reclamation_are_bounded_and_do_not_mix_tracks() {
        let mut renderer = renderer();
        let shared = renderer.shared.clone();
        let first = shared.play(samples(vec![0.1; 64]), 100);
        let mut out = [0.0; 6];
        realtime(|| renderer.callback(&mut out));
        first.cancelled.store(true, Ordering::Release);
        for _ in 0..100 {
            shared.play(samples(vec![0.7; 64]), 100);
        }
        assert_eq!(shared.pending.len(), 1);
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.7; 6]);
        // Deliberately omit control-side collection to exhaust retirement slots.
        for value in [0.3, 0.9] {
            let voice = Box::new(Voice {
                source: samples(vec![value; 64]),
                state: Arc::default(),
            });
            assert!(shared.pending.push(voice).is_ok());
            realtime(|| renderer.callback(&mut out));
        }
        assert_eq!(shared.retired.len(), 2);
        assert_eq!(shared.pending.len(), 1);
        assert_eq!(out, [0.3; 6]);
        renderer
            .current
            .as_ref()
            .unwrap()
            .state
            .cancelled
            .store(true, Ordering::Release);
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.0; 6]);
        shared.collect();
        realtime(|| renderer.callback(&mut out));
        assert_eq!(out, [0.9; 6]);
    }

    #[test]
    fn prepared_stereo_and_spectrum_keep_samples_through_callback_boundaries() {
        let spectrum = Arc::new(crate::spectrum::Spectrum::default());
        let _subscription = spectrum.subscribe();
        spectrum.playing(true);
        for (input_rate, output_rate) in [(44_100, 48_000), (48_000, 44_100)] {
            let data: Vec<_> = (0..4410)
                .flat_map(|i| [i as f32 / 4410.0, -(i as f32) / 4410.0])
                .collect();
            let input =
                SamplesBuffer::new(2.try_into().unwrap(), input_rate.try_into().unwrap(), data);
            let expected: Vec<_> = rodio::source::UniformSourceIterator::new(
                input.clone(),
                2.try_into().unwrap(),
                output_rate.try_into().unwrap(),
            )
            .collect();
            let (worker, source) = DecoderWorker::start(
                Box::new(input),
                2.try_into().unwrap(),
                output_rate.try_into().unwrap(),
            )
            .unwrap();
            let mut renderer = renderer();
            renderer.rate = output_rate;
            let state = renderer
                .shared
                .play(Box::new(spectrum.tap(Box::new(source))), 100);
            let mut actual = Vec::new();
            let mut out = [0.0; 254];
            while !state.finished.load(Ordering::Acquire) {
                realtime(|| renderer.callback(&mut out));
                actual.extend_from_slice(&out);
            }
            assert_eq!(&actual[..expected.len()], expected.as_slice());
            assert!(actual[expected.len()..].iter().all(|x| *x == 0.0));
            assert_eq!(
                worker.position_ms(),
                expected.len() as u64 * 1000 / (u64::from(output_rate) * 2)
            );
        }
    }

    #[test]
    fn timing_separates_callback_delivery_delay_from_render_work_with_fake_durations() {
        let t = Timing::default();
        let ms = Duration::from_millis;
        t.record(480, ms(10), None, ms(1));
        t.record(960, ms(20), Some((ms(30), ms(10))), ms(2));
        t.record(480, ms(10), Some((ms(20), ms(20))), ms(11));
        assert_eq!(t.callbacks.load(Ordering::Acquire), 3);
        assert_eq!(u64::MAX - t.min_frames.load(Ordering::Relaxed), 480);
        assert_eq!(t.max_frames.load(Ordering::Relaxed), 960);
        assert_eq!(t.max_gap_us.load(Ordering::Relaxed), 30_000);
        assert_eq!(t.max_excess_us.load(Ordering::Relaxed), 20_000);
        assert_eq!(t.max_render_us.load(Ordering::Relaxed), 11_000);
        assert_eq!(t.late.load(Ordering::Relaxed), 1);
        assert_eq!(t.over_budget.load(Ordering::Relaxed), 1);
    }
}
