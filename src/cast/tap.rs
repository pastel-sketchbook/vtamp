//! Cast what a device server plays. A [`Tap`] in the output path copies the
//! prepared PCM into a lock-free queue without blocking, decoding, or
//! allocating; an encoder thread resamples it to the cast format and produces
//! the same chained Ogg Opus stream a headless server would.
use super::{Hub, Muxer, Tags, codec};
use anyhow::{Context, Result};
use crossbeam_queue::ArrayQueue;
use rodio::{ChannelCount, SampleRate, Source};
use std::{
    sync::{Arc, mpsc},
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

const BLOCK_SAMPLES: usize = 2048;
/// About 1.4 s of 48 kHz stereo; the encoder drains far faster than that.
const QUEUE_BLOCKS: usize = 64;
/// Without samples for this long the open stream continues with silence.
const IDLE: Duration = Duration::from_millis(40);
const FRAME: Duration = Duration::from_millis(20);

struct Block {
    generation: u64,
    rate: u32,
    channels: u16,
    len: usize,
    pcm: [f32; BLOCK_SAMPLES],
}

enum Command {
    Begin { generation: u64, tags: Tags },
    End,
}

/// Owns the encoder thread. One instance serves a backend for its lifetime;
/// each voice the backend starts gets its own generation and logical stream.
pub struct CastEncoder {
    blocks: Arc<ArrayQueue<Block>>,
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    generation: u64,
}

impl CastEncoder {
    pub fn start(hub: Arc<Hub>, bitrate: u32) -> Result<Self> {
        let muxer = Muxer::new(bitrate, rand::RngExt::random(&mut rand::rng()))?;
        let blocks = Arc::new(ArrayQueue::new(QUEUE_BLOCKS));
        let (commands, receiver) = mpsc::channel();
        let thread = {
            let blocks = blocks.clone();
            thread::Builder::new()
                .name("vtamp-cast-encoder".into())
                .spawn(move || run(&blocks, &receiver, muxer, &hub))
                .context("Cannot start the cast encoder")?
        };
        Ok(Self {
            blocks,
            commands,
            thread: Some(thread),
            generation: 0,
        })
    }

    /// Begin the logical stream for the voice that the next [`Self::tap`] wraps.
    pub fn begin(&mut self, tags: Tags) -> u64 {
        self.generation += 1;
        let _ = self.commands.send(Command::Begin {
            generation: self.generation,
            tags,
        });
        self.generation
    }

    pub fn end(&self) {
        let _ = self.commands.send(Command::End);
    }

    pub fn tap(&self, source: Box<dyn Source + Send>, generation: u64) -> Tap {
        let channels = source.channels();
        let rate = source.sample_rate();
        let wake = self
            .thread
            .as_ref()
            .map(|thread| thread.thread().clone())
            .unwrap_or_else(thread::current);
        Tap {
            source,
            blocks: self.blocks.clone(),
            wake,
            generation,
            rate: rate.get(),
            channels: channels.get(),
            block_len: BLOCK_SAMPLES / usize::from(channels.get()) * usize::from(channels.get()),
            len: 0,
            pcm: [0.0; BLOCK_SAMPLES],
        }
    }
}

impl Drop for CastEncoder {
    fn drop(&mut self) {
        let (closed, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, closed));
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            if thread.join().is_err() {
                tracing::error!("Cast encoder thread panicked");
            }
        }
    }
}

/// Copies the samples of one voice into blocks for the encoder thread. Runs in
/// the output callback: no locks, no allocation, no I/O.
pub struct Tap {
    source: Box<dyn Source + Send>,
    blocks: Arc<ArrayQueue<Block>>,
    wake: Thread,
    generation: u64,
    rate: u32,
    channels: u16,
    block_len: usize,
    len: usize,
    pcm: [f32; BLOCK_SAMPLES],
}

impl Tap {
    fn flush(&mut self) {
        // A full queue drops the oldest block: the encoder is behind, never the output.
        self.blocks.force_push(Block {
            generation: self.generation,
            rate: self.rate,
            channels: self.channels,
            len: self.len,
            pcm: self.pcm,
        });
        self.len = 0;
        self.wake.unpark();
    }
}

impl Iterator for Tap {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        match self.source.next() {
            Some(sample) => {
                self.pcm[self.len] = if sample.is_finite() { sample } else { 0.0 };
                self.len += 1;
                if self.len == self.block_len {
                    self.flush();
                }
                Some(sample)
            }
            None => {
                if self.len > 0 {
                    self.flush();
                }
                None
            }
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.source.size_hint()
    }
}

impl Source for Tap {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> ChannelCount {
        self.source.channels()
    }
    fn sample_rate(&self) -> SampleRate {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
}

/// Linear interpolation from the device format to 48 kHz stereo, continuous
/// across blocks. The front pair is kept; mono is duplicated.
struct Resampler {
    step: f64,
    channels: usize,
    position: f64,
    previous: Option<[f32; 2]>,
}

impl Resampler {
    fn new(rate: u32, channels: u16) -> Self {
        Self {
            step: f64::from(rate) / f64::from(codec::SAMPLE_RATE),
            channels: usize::from(channels.max(1)),
            position: 0.0,
            previous: None,
        }
    }

    fn push(&mut self, pcm: &[f32], out: &mut Vec<f32>) {
        for frame in pcm.chunks_exact(self.channels) {
            let current = if self.channels == 1 {
                [frame[0], frame[0]]
            } else {
                [frame[0], frame[1]]
            };
            let Some(previous) = self.previous else {
                self.previous = Some(current);
                continue;
            };
            while self.position < 1.0 {
                let t = self.position as f32;
                out.push(previous[0] + (current[0] - previous[0]) * t);
                out.push(previous[1] + (current[1] - previous[1]) * t);
                self.position += self.step;
            }
            self.position -= 1.0;
            self.previous = Some(current);
        }
    }
}

struct Stream {
    generation: u64,
    resampler: Option<Resampler>,
    pcm: Vec<f32>,
}

impl Stream {
    fn encode_full_frames(&mut self, muxer: &mut Muxer, hub: &Hub, out: &mut Vec<u8>) {
        let mut offset = 0;
        while self.pcm.len() - offset >= codec::FRAME_LEN {
            out.clear();
            match muxer.frame(&self.pcm[offset..offset + codec::FRAME_LEN], out) {
                Ok(()) => hub.publish(out),
                Err(error) => tracing::error!("Cast encoding failed: {error:#}"),
            }
            offset += codec::FRAME_LEN;
        }
        self.pcm.drain(..offset);
    }

    fn silence(&mut self, muxer: &mut Muxer, hub: &Hub, out: &mut Vec<u8>) {
        self.pcm.resize(codec::FRAME_LEN, 0.0);
        self.encode_full_frames(muxer, hub, out);
    }
}

fn end_stream(muxer: &mut Muxer, hub: &Hub, out: &mut Vec<u8>) {
    if muxer.is_open() {
        out.clear();
        muxer.end(out);
        hub.end(out);
    }
}

fn run(
    blocks: &ArrayQueue<Block>,
    commands: &mpsc::Receiver<Command>,
    mut muxer: Muxer,
    hub: &Hub,
) {
    let mut stream: Option<Stream> = None;
    let mut out = Vec::new();
    let mut last_audio = Instant::now();
    let mut silence_due: Option<Instant> = None;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Begin { generation, tags }) => {
                    end_stream(&mut muxer, hub, &mut out);
                    out.clear();
                    match muxer.begin(&tags, &mut out) {
                        Ok(()) => hub.begin(&out),
                        Err(error) => tracing::error!("Cannot start a cast stream: {error:#}"),
                    }
                    stream = Some(Stream {
                        generation,
                        resampler: None,
                        pcm: Vec::with_capacity(codec::FRAME_LEN * 4),
                    });
                    last_audio = Instant::now();
                    silence_due = None;
                }
                Ok(Command::End) => {
                    end_stream(&mut muxer, hub, &mut out);
                    stream = None;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    end_stream(&mut muxer, hub, &mut out);
                    return;
                }
            }
        }
        let mut received = false;
        while let Some(block) = blocks.pop() {
            let Some(current) = &mut stream else { continue };
            if block.generation != current.generation {
                continue;
            }
            received = true;
            let resampler = current
                .resampler
                .get_or_insert_with(|| Resampler::new(block.rate, block.channels));
            resampler.push(&block.pcm[..block.len], &mut current.pcm);
            current.encode_full_frames(&mut muxer, hub, &mut out);
        }
        if received {
            last_audio = Instant::now();
            silence_due = None;
        } else if let Some(current) = &mut stream
            && last_audio.elapsed() >= IDLE
        {
            // Paused or finished voice: keep the stream alive at frame cadence.
            let due = silence_due.get_or_insert(last_audio + IDLE);
            while Instant::now() >= *due {
                current.silence(&mut muxer, hub, &mut out);
                *due += FRAME;
            }
        }
        thread::park_timeout(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast::{Demuxer, Event, codec::tests::sine};
    use rodio::buffer::SamplesBuffer;

    fn drain(receiver: &mut crate::cast::Chunks, demuxer: &mut Demuxer) -> Vec<Event> {
        while let Ok(chunk) = receiver.try_recv() {
            demuxer.push(&chunk);
        }
        std::iter::from_fn(|| demuxer.pop()).collect()
    }

    fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn resampler_keeps_duration_and_passes_48k_through() {
        let mut same = Resampler::new(48_000, 2);
        let mut out = vec![];
        same.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], &mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0]);
        let mut up = Resampler::new(44_100, 1);
        let mut out = vec![];
        for _ in 0..10 {
            up.push(&vec![0.5; 4410], &mut out);
        }
        // One second of mono at 44.1 kHz becomes a second of stereo at 48 kHz,
        // less the frame held back for interpolation.
        assert!((out.len() as i64 - 96_000).abs() <= 4, "{}", out.len());
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn taps_feed_tagged_streams_and_idle_voices_turn_into_silence() {
        let hub = Arc::new(Hub::default());
        let mut encoder = CastEncoder::start(hub.clone(), 96_000).unwrap();
        let (_, mut receiver) = hub.subscribe();
        let mut demuxer = Demuxer::default();
        let generation = encoder.begin(vec![("TITLE".into(), "Tapped".into())]);
        let audio = sine(44_100, 440.0);
        let source = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(44_100).unwrap(),
            audio,
        );
        let mut tap = encoder.tap(Box::new(source), generation);
        assert_eq!(tap.channels().get(), 2);
        // The output callback drains the voice; the copy must not alter it.
        let played: Vec<f32> = tap.by_ref().collect();
        assert_eq!(played.len(), 88_200);
        let mut events = vec![];
        wait_until("a second of resampled audio", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            events
                .iter()
                .filter(|e| matches!(e, Event::Packet { .. }))
                .count()
                >= 45
        });
        assert!(matches!(
            events.first(),
            Some(Event::Start { tags, .. }) if tags[0].1 == "Tapped"
        ));
        // Nothing more arrives from the finished voice, so silence keeps flowing.
        let before = events.len();
        wait_until("silence frames", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            events.len() >= before + 60
        });
        let mut decoder = codec::Decoder::new().unwrap();
        let tail: Vec<f32> = events
            .iter()
            .rev()
            .take(5)
            .filter_map(|e| match e {
                Event::Packet { data, .. } => Some(decoder.decode(data).unwrap().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(tail.iter().all(|s| s.abs() < 0.01), "silence after EOF");
        // A new voice begins a new stream; blocks of the old one are ignored.
        let next = encoder.begin(vec![]);
        assert_ne!(next, generation);
        let stale = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(48_000).unwrap(),
            vec![0.9; 9600],
        );
        encoder.tap(Box::new(stale), generation).for_each(drop);
        wait_until("the second stream", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            events
                .iter()
                .filter(|e| matches!(e, Event::Start { .. }))
                .count()
                == 2
        });
        encoder.end();
        wait_until("the end-of-stream page", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            matches!(events.last(), Some(Event::End { .. }))
        });
        let second_start = events
            .iter()
            .rposition(|e| matches!(e, Event::Start { .. }))
            .unwrap();
        let loud = events[second_start..]
            .iter()
            .filter_map(|e| match e {
                Event::Packet { data, .. } => Some(decoder.decode(data).unwrap().to_vec()),
                _ => None,
            })
            .flatten()
            .any(|s| s.abs() > 0.5);
        assert!(!loud, "stale blocks must not reach the new stream");
        drop(encoder);
    }
}
