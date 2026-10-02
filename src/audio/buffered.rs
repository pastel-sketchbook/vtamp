//! Bounded decode-ahead. Only prepared PCM is consumed by the output callback.
use anyhow::{Context, Result};
use crossbeam_queue::ArrayQueue;
use rodio::{ChannelCount, SampleRate, Source, source::UniformSourceIterator};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const BLOCK_SAMPLES: usize = 2048;
const MAX_BLOCKS: usize = 128;

struct Block {
    samples: [f32; BLOCK_SAMPLES],
    len: usize,
}
impl Default for Block {
    fn default() -> Self {
        Self {
            samples: [0.0; BLOCK_SAMPLES],
            len: 0,
        }
    }
}

struct Shared {
    blocks: ArrayQueue<Block>,
    cancelled: AtomicBool,
    eof: AtomicBool,
    consumed: AtomicU64,
    underruns: AtomicU64,
    missing: AtomicU64,
}

/// Owned by the control thread, so decoder disposal and joining never run in
/// the audio callback (including when rodio discards an exhausted source).
pub(super) struct DecoderWorker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    samples_per_second: u64,
    reported: (u64, u64),
    last_report: Instant,
    context: Option<(PathBuf, u64)>,
}

impl DecoderWorker {
    pub(super) fn start(
        source: Box<dyn Source + Send>,
        channels: ChannelCount,
        rate: SampleRate,
    ) -> Result<(Self, BufferedSource)> {
        let duration = source.total_duration();
        let block_len = BLOCK_SAMPLES / usize::from(channels.get()) * usize::from(channels.get());
        anyhow::ensure!(block_len > 0, "Too many output channels");
        let samples_per_second = u64::from(channels.get()) * u64::from(rate.get());
        // Half a second for ordinary outputs; memory stays below 1.1 MiB even
        // for unusually high sample rates/channel counts. Never buffer a song.
        let capacity = (samples_per_second / 2)
            .div_ceil(block_len as u64)
            .clamp(2, MAX_BLOCKS as u64) as usize;
        let shared = Arc::new(Shared {
            blocks: ArrayQueue::new(capacity),
            cancelled: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            consumed: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            missing: AtomicU64::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let sleep = Duration::from_secs_f64(
            (capacity / 4).max(1) as f64 * block_len as f64 / samples_per_second as f64,
        )
        .min(Duration::from_millis(50));
        let thread = thread::Builder::new()
            .name("vtamp-decoder".into())
            .spawn(move || {
                // Even converter initialization can read input. Keep it here.
                let mut source = UniformSourceIterator::new(source, channels, rate);
                let mut ready = Some(ready_tx);
                loop {
                    if worker_shared.cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    if worker_shared.blocks.is_full() {
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(());
                        }
                        // No wake, lock, allocation, or OS call is needed on the
                        // consumer. Pause/stop unpark us from the control thread.
                        thread::park_timeout(sleep);
                        continue;
                    }
                    let mut block = Block::default();
                    for sample in &mut block.samples[..block_len] {
                        match source.next() {
                            Some(value) => {
                                *sample = value;
                                block.len += 1;
                            }
                            None => break,
                        }
                    }
                    let eof = block.len < block_len;
                    if block.len > 0 {
                        // Single producer: the consumer can only make more room.
                        assert!(worker_shared.blocks.push(block).is_ok());
                    }
                    if eof {
                        worker_shared.eof.store(true, Ordering::Release);
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(());
                        }
                        break;
                    }
                }
            })
            .context("Cannot start audio decoder")?;
        let worker = Self {
            shared: shared.clone(),
            thread: Some(thread),
            samples_per_second,
            reported: (0, 0),
            last_report: Instant::now(),
            context: None,
        };
        // Fill before attaching to the output; short files finish preloading at EOF.
        ready_rx
            .recv()
            .context("Audio decoder stopped before buffering")?;
        let source = BufferedSource {
            shared,
            block: Block::default(),
            cursor: 0,
            block_len,
            channels,
            rate,
            duration,
            consumed: 0,
            missing: 0,
            silence: false,
            underrunning: false,
            ended: false,
        };
        Ok((worker, source))
    }

    pub(super) fn set_context(&mut self, path: &Path, offset_ms: u64) {
        self.context = Some((path.to_owned(), offset_ms));
    }

    pub(super) fn position_ms(&self) -> u64 {
        self.shared.consumed.load(Ordering::Relaxed) * 1000 / self.samples_per_second
    }

    pub(super) fn consumer_alive(&self) -> bool {
        Arc::strong_count(&self.shared) > 1
    }

    pub(super) fn cancel(&mut self) {
        self.shared.cancelled.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            if thread.join().is_err() {
                tracing::error!("Audio decoder worker panicked");
            }
        }
        self.report(true);
    }

    pub(super) fn report(&mut self, force: bool) {
        if !force && self.last_report.elapsed() < Duration::from_secs(5) {
            return;
        }
        let current = (
            self.shared.underruns.load(Ordering::Relaxed),
            self.shared.missing.load(Ordering::Relaxed),
        );
        if current != self.reported {
            tracing::warn!(
                path = ?self.context.as_ref().map(|(path, _)| path),
                position_ms = self.position_ms().saturating_add(self.context.as_ref().map_or(0, |(_, offset)| *offset)),
                underruns = current.0.saturating_sub(self.reported.0),
                silence_ms =
                    current.1.saturating_sub(self.reported.1) * 1000 / self.samples_per_second,
                "Audio decode buffer underrun"
            );
        }
        self.reported = current;
        self.last_report = Instant::now();
    }
}

impl Drop for DecoderWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(super) struct BufferedSource {
    shared: Arc<Shared>,
    block: Block,
    cursor: usize,
    block_len: usize,
    channels: ChannelCount,
    rate: SampleRate,
    duration: Option<Duration>,
    consumed: u64,
    missing: u64,
    silence: bool,
    underrunning: bool,
    ended: bool,
}

impl Iterator for BufferedSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.ended {
            return None;
        }
        if self.cursor == self.block.len {
            let mut next = self.shared.blocks.pop();
            if next.is_none() && self.shared.eof.load(Ordering::Acquire) {
                // EOF is published after the final block. Recheck after acquire
                // so a racing final push cannot truncate the track.
                next = self.shared.blocks.pop();
                if next.is_none() {
                    self.ended = true;
                    self.shared.consumed.store(self.consumed, Ordering::Relaxed);
                    return None;
                }
            }
            self.cursor = 0;
            if let Some(block) = next {
                self.block = block;
                self.silence = false;
                self.underrunning = false;
            } else {
                // Silence must cover whole frames; never end the track or
                // advance music position while the producer is late.
                self.block.len = self.block_len;
                self.silence = true;
                if !self.underrunning {
                    self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                    self.underrunning = true;
                }
            }
        }
        let sample = if self.silence {
            0.0
        } else {
            self.block.samples[self.cursor]
        };
        self.cursor += 1;
        if self.silence {
            self.missing += 1;
            if self.missing.is_multiple_of(64) || self.cursor == self.block.len {
                self.shared.missing.store(self.missing, Ordering::Relaxed);
            }
        } else {
            self.consumed += 1;
            // Publish sub-millisecond progress at common sample rates without
            // a shared atomic write for every sample.
            if self.consumed.is_multiple_of(64) || self.cursor == self.block.len {
                self.shared.consumed.store(self.consumed, Ordering::Relaxed);
            }
        }
        Some(sample)
    }
}

impl Source for BufferedSource {
    fn current_span_len(&self) -> Option<usize> {
        self.ended.then_some(0)
    }
    fn channels(&self) -> ChannelCount {
        self.channels
    }
    fn sample_rate(&self) -> SampleRate {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
    // Seek is deliberately performed by reopening the decoder on the control
    // thread, never by waiting for a command to run in the output callback.
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready() {
            assert!(Instant::now() < deadline, "decoder did not make progress");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn short_files_preserve_every_sample_and_eof_at_different_output_formats() {
        for (channels, rate, len) in [(1, 44_100, 101), (2, 48_000, 4096), (6, 96_000, 9996)] {
            let data: Vec<_> = (0..len).map(|n| n as f32 / len as f32).collect();
            let source = SamplesBuffer::new(
                channels.try_into().unwrap(),
                rate.try_into().unwrap(),
                data.clone(),
            );
            let (worker, mut output) = DecoderWorker::start(
                Box::new(source),
                channels.try_into().unwrap(),
                rate.try_into().unwrap(),
            )
            .unwrap();
            assert_eq!(output.by_ref().collect::<Vec<_>>(), data);
            assert_eq!(output.next(), None);
            assert_eq!(worker.shared.consumed.load(Ordering::Relaxed), len as u64);
            assert_eq!(worker.shared.underruns.load(Ordering::Relaxed), 0);
        }
        let source = SamplesBuffer::new(
            1.try_into().unwrap(),
            24_000.try_into().unwrap(),
            vec![0.5; 2400],
        );
        let expected: Vec<_> = UniformSourceIterator::new(
            source.clone(),
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
        )
        .collect();
        let (_, output) = DecoderWorker::start(
            Box::new(source),
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
        )
        .unwrap();
        assert_eq!(output.collect::<Vec<_>>(), expected);
    }

    struct Probe {
        next: usize,
        end: usize,
        gate: Option<(usize, mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
        dropped: mpsc::Sender<thread::ThreadId>,
    }
    impl Iterator for Probe {
        type Item = f32;
        fn next(&mut self) -> Option<f32> {
            assert_eq!(thread::current().name(), Some("vtamp-decoder"));
            if self
                .gate
                .as_ref()
                .is_some_and(|(at, _, _)| self.next == *at)
            {
                let (_, entered, release) = self.gate.take().unwrap();
                entered.send(()).unwrap();
                // Bound even test failures so cleanup cannot hang the suite.
                let _ = release.recv_timeout(Duration::from_secs(5));
            }
            if self.next == self.end {
                return None;
            }
            self.next += 1;
            Some(self.next as f32)
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.dropped.send(thread::current().id());
        }
    }
    impl Source for Probe {
        fn current_span_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> ChannelCount {
            2.try_into().unwrap()
        }
        fn sample_rate(&self) -> SampleRate {
            48_000.try_into().unwrap()
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn blocked_decoder_does_not_block_output_or_skip_music_and_silence_does_not_advance_position() {
        let (entered_tx, entered) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::channel();
        let (dropped_tx, dropped) = mpsc::channel();
        let probe = Probe {
            next: 0,
            end: 60_010,
            gate: Some((60_000, entered_tx, release_rx)),
            dropped: dropped_tx,
        };
        let (worker, mut output) = DecoderWorker::start(
            Box::new(probe),
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
        )
        .unwrap();
        // Consume the prefill so the producer reaches the deliberately blocked read.
        let prefetched = worker.shared.blocks.len() * BLOCK_SAMPLES;
        for n in 1..=prefetched {
            assert_eq!(output.next(), Some(n as f32));
        }
        entered.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut consumed = prefetched;
        // Samples prepared before the stalled read remain playable.
        while !worker.shared.blocks.is_empty() || output.cursor < output.block.len {
            consumed += 1;
            assert_eq!(output.next(), Some(consumed as f32));
        }
        let position = worker.position_ms();
        for _ in 0..BLOCK_SAMPLES * 2 {
            assert_eq!(output.next(), Some(0.0));
        }
        assert_eq!(worker.position_ms(), position);
        assert_eq!(worker.shared.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(
            worker.shared.missing.load(Ordering::Relaxed),
            (BLOCK_SAMPLES * 2) as u64
        );
        release.send(()).unwrap();
        wait_until(|| worker.shared.eof.load(Ordering::Acquire));
        for n in consumed + 1..=60_010 {
            assert_eq!(output.next(), Some(n as f32));
        }
        assert_eq!(output.next(), None);
        assert_eq!(worker.shared.consumed.load(Ordering::Relaxed), 60_010);
        assert_ne!(
            dropped.recv_timeout(Duration::from_secs(3)).unwrap(),
            thread::current().id()
        );
    }

    #[test]
    fn cancellation_releases_decoder_on_worker_with_bounded_read_ahead() {
        let (tx, rx) = mpsc::channel();
        let probe = Probe {
            next: 0,
            end: usize::MAX,
            gate: None,
            dropped: tx,
        };
        let (worker, output) = DecoderWorker::start(
            Box::new(probe),
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
        )
        .unwrap();
        assert!(worker.shared.blocks.len() <= 24);
        assert!(worker.shared.blocks.is_full());
        // Output can outlive the owner (as it does inside rodio); stopping must
        // still dispose native/file resources without requiring another callback.
        drop(worker);
        assert_ne!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            thread::current().id()
        );
        drop(output);
    }

    #[test]
    fn native_aac_and_lossless_files_keep_pcm_and_seek_positions_through_buffering() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for name in ["extended-mdat.m4a", "stereo-alac.m4a", "stereo.wav"] {
            for position in [Duration::ZERO, Duration::from_millis(100)] {
                let mut source = super::super::decode_file(&root.join(name)).unwrap();
                source.try_seek(position).unwrap();
                let channels = source.channels();
                let rate = source.sample_rate();
                let expected: Vec<_> = source.collect();
                let mut source = super::super::decode_file(&root.join(name)).unwrap();
                source.try_seek(position).unwrap();
                let (worker, output) = DecoderWorker::start(source, channels, rate).unwrap();
                let actual: Vec<_> = output.collect();
                assert_eq!(actual.len(), expected.len(), "{name}");
                assert!(
                    actual
                        .iter()
                        .zip(&expected)
                        .all(|(a, b)| (a - b).abs() < 0.00001),
                    "{name}"
                );
                assert_eq!(worker.shared.underruns.load(Ordering::Relaxed), 0);
            }
        }
    }
}
