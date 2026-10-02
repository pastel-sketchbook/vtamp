//! Ogg Opus audio for a headless server and remote listeners.
//!
//! A [`Muxer`] turns 48 kHz stereo PCM into a chained Ogg Opus byte stream:
//! every track start or seek begins a new logical stream with its own tags, so
//! a listener knows where to discard buffered audio. A [`Demuxer`] turns the
//! bytes back into stream boundaries and Opus packets for a [`codec::Decoder`].
//! Everything here is pure computation; transport and clocks live elsewhere.
use anyhow::{Result, bail, ensure};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

pub mod codec;
pub mod ogg;

pub const DEFAULT_BITRATE: u32 = 128_000;
const VENDOR: &str = concat!("vtamp ", env!("CARGO_PKG_VERSION"));
/// Audio packets per page: 100 ms of latency at the page level.
const PACKETS_PER_PAGE: usize = 5;
const OPUS_HEAD: &[u8; 8] = b"OpusHead";
const OPUS_TAGS: &[u8; 8] = b"OpusTags";

pub type Tags = Vec<(String, String)>;
/// Bytes of one or more whole Ogg pages.
pub type Chunk = Arc<[u8]>;
pub type Listener = broadcast::Receiver<Chunk>;

/// Buffered chunks a slow listener may fall behind by before it skips ahead.
const HUB_CAPACITY: usize = 256;

/// Fan-out point between the thread that produces the cast and its listeners.
///
/// Listeners that join mid-stream first receive the open logical stream's
/// header pages so they can decode from the next page.
pub struct Hub {
    inner: Mutex<HubInner>,
}

struct HubInner {
    headers: Option<Chunk>,
    sender: broadcast::Sender<Chunk>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HubInner {
                headers: None,
                sender: broadcast::channel(HUB_CAPACITY).0,
            }),
        }
    }
}

impl Hub {
    /// The current stream's header pages, if one is open, and every chunk
    /// published from now on.
    pub fn subscribe(&self) -> (Option<Chunk>, Listener) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.headers.clone(), inner.sender.subscribe())
    }

    /// Header pages of the open logical stream, if any.
    pub fn headers(&self) -> Option<Chunk> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .headers
            .clone()
    }

    pub fn listeners(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sender
            .receiver_count()
    }

    /// Publish audio pages of the open stream.
    pub fn publish(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = inner.sender.send(bytes.into());
    }

    /// Publish the header pages of a new logical stream and remember them for
    /// listeners who join later.
    pub fn begin(&self, headers: &[u8]) {
        let chunk: Chunk = headers.into();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.headers = Some(chunk.clone());
        let _ = inner.sender.send(chunk);
    }

    /// Publish the end-of-stream page; later joiners get no headers.
    pub fn end(&self, bytes: &[u8]) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.headers = None;
        if !bytes.is_empty() {
            let _ = inner.sender.send(bytes.into());
        }
    }
}

pub struct Muxer {
    encoder: codec::Encoder,
    writer: Option<ogg::PageWriter>,
    next_serial: u32,
    granule: u64,
    pending: usize,
}

impl Muxer {
    /// `serial` seeds the logical stream serial numbers; callers pass a random value.
    pub fn new(bitrate: u32, serial: u32) -> Result<Self> {
        Ok(Self {
            encoder: codec::Encoder::new(bitrate)?,
            writer: None,
            next_serial: serial,
            granule: 0,
            pending: 0,
        })
    }

    pub fn is_open(&self) -> bool {
        self.writer.is_some()
    }

    /// Samples encoded into the current logical stream, including the pre-skip.
    pub fn granule(&self) -> u64 {
        self.granule
    }

    /// Start a logical stream with the given comments, ending any open one.
    pub fn begin(&mut self, tags: &[(String, String)], out: &mut Vec<u8>) -> Result<()> {
        for (key, _) in tags {
            ensure!(
                !key.is_empty()
                    && key
                        .bytes()
                        .all(|byte| (0x20..=0x7D).contains(&byte) && byte != b'='),
                "Invalid tag key: {key:?}"
            );
        }
        self.end(out);
        self.encoder.reset()?;
        let mut writer = ogg::PageWriter::new(self.next_serial);
        self.next_serial = self.next_serial.wrapping_add(1);
        writer.packet(&opus_head(self.encoder.lookahead()), 0, out);
        writer.flush(false, out);
        writer.packet(&opus_tags(tags), 0, out);
        writer.flush(false, out);
        self.writer = Some(writer);
        self.granule = 0;
        self.pending = 0;
        Ok(())
    }

    /// Encode one 20 ms frame of interleaved stereo samples.
    pub fn frame(&mut self, pcm: &[f32], out: &mut Vec<u8>) -> Result<()> {
        let Some(writer) = &mut self.writer else {
            bail!("No logical stream is open");
        };
        // A full page leaves when the next packet arrives, so the end-of-stream
        // page always carries audio instead of being empty.
        if self.pending == PACKETS_PER_PAGE {
            writer.flush(false, out);
            self.pending = 0;
        }
        let packet = self.encoder.encode(pcm)?;
        self.granule += codec::FRAME_FRAMES as u64;
        writer.packet(packet, self.granule, out);
        self.pending += 1;
        Ok(())
    }

    /// Finish the open logical stream, if any, with an end-of-stream page.
    pub fn end(&mut self, out: &mut Vec<u8>) {
        if let Some(mut writer) = self.writer.take() {
            writer.flush(true, out);
            self.pending = 0;
        }
    }
}

fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(OPUS_HEAD);
    head.push(1);
    head.push(codec::CHANNELS as u8);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&codec::SAMPLE_RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    head
}

fn opus_tags(tags: &[(String, String)]) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(OPUS_TAGS);
    packet.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    packet.extend_from_slice(VENDOR.as_bytes());
    packet.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (key, value) in tags {
        let comment = format!("{key}={value}");
        packet.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        packet.extend_from_slice(comment.as_bytes());
    }
    packet
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A logical stream begins; discard audio buffered from earlier streams.
    Start {
        serial: u32,
        /// Samples to discard from the start of the decoded stream.
        pre_skip: u16,
        tags: Tags,
    },
    Packet {
        serial: u32,
        data: Vec<u8>,
        /// Granule position of the page this packet completes, when it is the
        /// last packet to end on its page.
        granule: Option<u64>,
    },
    End {
        serial: u32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The identification header arrived; the comment header is next.
    Head,
    Audio,
    /// Not Opus, or damaged headers: dropped until its end-of-stream page.
    Ignored,
}

struct Stream {
    serial: u32,
    sequence: u32,
    phase: Phase,
    pre_skip: u16,
    partial: Vec<u8>,
}

#[derive(Default)]
pub struct Demuxer {
    reader: ogg::PageReader,
    stream: Option<Stream>,
    events: VecDeque<Event>,
    /// Pages dropped for belonging to no current logical stream.
    pub stray_pages: u64,
    /// Page sequence gaps, each of which may have lost a continued packet.
    pub gaps: u64,
}

impl Demuxer {
    pub fn push(&mut self, bytes: &[u8]) {
        self.reader.push(bytes);
        while let Some(page) = self.reader.next_page() {
            self.page(page);
        }
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Bytes skipped while resynchronizing on damaged input.
    pub fn skipped(&self) -> u64 {
        self.reader.skipped
    }

    fn page(&mut self, page: ogg::Page) {
        if page.bos() {
            if let Some(stream) = self.stream.take()
                && stream.phase == Phase::Audio
            {
                self.events.push_back(Event::End {
                    serial: stream.serial,
                });
            }
            let (phase, pre_skip) = match page.pieces().next() {
                Some((head, true)) if page.pieces().count() == 1 && opus_head_ok(head) => {
                    (Phase::Head, u16::from_le_bytes([head[10], head[11]]))
                }
                _ => (Phase::Ignored, 0),
            };
            self.stream = Some(Stream {
                serial: page.serial,
                sequence: page.sequence,
                phase,
                pre_skip,
                partial: vec![],
            });
            if page.eos() {
                self.finish();
            }
            return;
        }
        let Some(stream) = &mut self.stream else {
            self.stray_pages += 1;
            return;
        };
        if stream.serial != page.serial {
            self.stray_pages += 1;
            return;
        }
        if page.sequence != stream.sequence.wrapping_add(1) {
            self.gaps += 1;
            stream.partial.clear();
        }
        stream.sequence = page.sequence;
        if !page.continued() {
            stream.partial.clear();
        }
        let complete = page.pieces().filter(|(_, complete)| *complete).count();
        let mut index = 0;
        for (piece, is_complete) in page.pieces() {
            stream.partial.extend_from_slice(piece);
            if !is_complete {
                break;
            }
            index += 1;
            let packet = std::mem::take(&mut stream.partial);
            let granule =
                (index == complete && page.granule != ogg::NO_GRANULE).then_some(page.granule);
            match stream.phase {
                // A stream starts for listeners once both headers are valid.
                Phase::Head => {
                    stream.phase = match parse_tags(&packet) {
                        Some(tags) => {
                            self.events.push_back(Event::Start {
                                serial: stream.serial,
                                pre_skip: stream.pre_skip,
                                tags,
                            });
                            Phase::Audio
                        }
                        None => Phase::Ignored,
                    };
                }
                Phase::Audio => self.events.push_back(Event::Packet {
                    serial: stream.serial,
                    data: packet,
                    granule,
                }),
                Phase::Ignored => {}
            }
        }
        if page.eos() {
            self.finish();
        }
    }

    fn finish(&mut self) {
        if let Some(stream) = self.stream.take()
            && stream.phase == Phase::Audio
        {
            self.events.push_back(Event::End {
                serial: stream.serial,
            });
        }
    }
}

fn opus_head_ok(head: &[u8]) -> bool {
    head.len() >= 19
        && &head[..8] == OPUS_HEAD
        && head[8] >> 4 == 0
        && (1..=2).contains(&head[9])
        && head[18] == 0
}

fn parse_tags(packet: &[u8]) -> Option<Tags> {
    if packet.len() < 8 || &packet[..8] != OPUS_TAGS {
        return None;
    }
    let mut offset = 8;
    let mut field = |len: Option<usize>| -> Option<&[u8]> {
        let len = match len {
            Some(len) => len,
            None => {
                let raw = packet.get(offset..offset + 4)?;
                offset += 4;
                u32::from_le_bytes(raw.try_into().unwrap()) as usize
            }
        };
        let bytes = packet.get(offset..offset + len)?;
        offset += len;
        Some(bytes)
    };
    field(None)?;
    let count = u32::from_le_bytes(field(Some(4))?.try_into().unwrap());
    let mut tags = Vec::new();
    for _ in 0..count {
        let comment = std::str::from_utf8(field(None)?).ok()?;
        if let Some((key, value)) = comment.split_once('=') {
            tags.push((key.to_owned(), value.to_owned()));
        }
    }
    Some(tags)
}

#[cfg(test)]
mod tests {
    use super::{
        codec::tests::{sine, snr_db},
        *,
    };

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn drain(demuxer: &mut Demuxer) -> Vec<Event> {
        std::iter::from_fn(|| demuxer.pop()).collect()
    }

    fn demux(bytes: &[u8], chunk: usize) -> (Vec<Event>, Demuxer) {
        let mut demuxer = Demuxer::default();
        let mut events = vec![];
        for piece in bytes.chunks(chunk) {
            demuxer.push(piece);
            events.extend(drain(&mut demuxer));
        }
        (events, demuxer)
    }

    #[test]
    fn chained_streams_round_trip_with_tags_granules_and_pre_skip() {
        let mut muxer = Muxer::new(DEFAULT_BITRATE, 7).unwrap();
        let mut bytes = vec![];
        let first = sine(codec::FRAME_FRAMES * 50, 440.0);
        let second = sine(codec::FRAME_FRAMES * 12, 660.0);
        assert!(muxer.frame(&first[..codec::FRAME_LEN], &mut bytes).is_err());
        let first_tags = tags(&[("TITLE", "First"), ("VTAMP_POSITION_MS", "12000")]);
        muxer.begin(&first_tags, &mut bytes).unwrap();
        for frame in first.chunks(codec::FRAME_LEN) {
            muxer.frame(frame, &mut bytes).unwrap();
        }
        assert_eq!(muxer.granule(), 50 * 960);
        let second_tags = tags(&[("TITLE", "Second: 한글=값")]);
        muxer.begin(&second_tags, &mut bytes).unwrap();
        for frame in second.chunks(codec::FRAME_LEN) {
            muxer.frame(frame, &mut bytes).unwrap();
        }
        muxer.end(&mut bytes);
        assert!(!muxer.is_open());
        muxer.end(&mut bytes);

        let (events, demuxer) = demux(&bytes, usize::MAX);
        assert_eq!(demuxer.skipped(), 0);
        assert_eq!((demuxer.stray_pages, demuxer.gaps), (0, 0));
        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Start {
                    serial,
                    pre_skip,
                    tags,
                } => Some((*serial, *pre_skip, tags.clone())),
                _ => None,
            })
            .collect();
        let pre_skip = codec::Encoder::new(DEFAULT_BITRATE).unwrap().lookahead();
        assert_eq!(
            starts,
            vec![
                (7, pre_skip, first_tags.clone()),
                (8, pre_skip, second_tags.clone())
            ]
        );
        let ends: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::End { serial } => Some(*serial),
                _ => None,
            })
            .collect();
        assert_eq!(ends, vec![7, 8]);
        let packets = |serial| {
            events
                .iter()
                .filter_map(|e| match e {
                    Event::Packet {
                        serial: s,
                        data,
                        granule,
                    } if *s == serial => Some((data.clone(), *granule)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let (first_packets, second_packets) = (packets(7), packets(8));
        assert_eq!(first_packets.len(), 50);
        assert_eq!(second_packets.len(), 12);
        // Every fifth packet closes a page carrying the cumulative granule, and
        // the end-of-stream page carries the final granule whether it is full
        // (first stream) or partial (second stream).
        for (i, (_, granule)) in first_packets.iter().enumerate() {
            let expected = ((i + 1) % PACKETS_PER_PAGE == 0).then_some((i as u64 + 1) * 960);
            assert_eq!(*granule, expected, "{i}");
        }
        assert_eq!(second_packets.last().unwrap().1, Some(12 * 960));
        assert_eq!(
            bytes.windows(4).filter(|w| w == b"OggS").count(),
            4 + 10 + 3
        );
        // Order: Start, packets, End, Start, packets, End.
        let kinds: Vec<_> = events
            .iter()
            .map(|e| match e {
                Event::Start { .. } => 'S',
                Event::Packet { .. } => 'P',
                Event::End { .. } => 'E',
            })
            .collect();
        let expected: String = format!("S{}ES{}E", "P".repeat(50), "P".repeat(12));
        assert_eq!(kinds.iter().collect::<String>(), expected);

        let mut decoder = codec::Decoder::new().unwrap();
        let mut decoded = vec![];
        for (data, _) in &second_packets {
            decoded.extend_from_slice(decoder.decode(data).unwrap());
        }
        let aligned = &decoded[usize::from(pre_skip) * codec::CHANNELS..];
        let settle = codec::FRAME_LEN * 5;
        let snr = snr_db(&second[settle..], &aligned[settle..]);
        assert!(snr > 18.0, "SNR {snr} dB");

        for chunk in [1, 13, 700] {
            let (again, _) = demux(&bytes, chunk);
            assert_eq!(again, events, "{chunk}");
        }
    }

    #[test]
    fn damaged_pages_are_dropped_and_the_stream_continues() {
        let mut muxer = Muxer::new(96_000, 1).unwrap();
        let mut bytes = vec![];
        muxer.begin(&tags(&[("TITLE", "x")]), &mut bytes).unwrap();
        let audio = sine(codec::FRAME_FRAMES * 20, 300.0);
        let mut page_starts = vec![];
        for (i, frame) in audio.chunks(codec::FRAME_LEN).enumerate() {
            if i % PACKETS_PER_PAGE == 0 {
                page_starts.push(bytes.len());
            }
            muxer.frame(frame, &mut bytes).unwrap();
        }
        muxer.end(&mut bytes);
        // Corrupt the second audio page (the third page after the two headers).
        let target = page_starts[1] + 40;
        bytes[target] ^= 0x55;
        let (events, demuxer) = demux(&bytes, 97);
        assert!(demuxer.skipped() > 0);
        assert_eq!(demuxer.gaps, 1);
        let packets = events
            .iter()
            .filter(|e| matches!(e, Event::Packet { .. }))
            .count();
        assert_eq!(packets, 15);
        assert!(matches!(events.first(), Some(Event::Start { .. })));
        assert!(matches!(events.last(), Some(Event::End { serial: 1 })));
    }

    #[test]
    fn foreign_and_truncated_streams_are_ignored_without_events() {
        let mut writer = ogg::PageWriter::new(5);
        let mut bytes = vec![];
        writer.packet(b"\x01vorbis-not-opus", 0, &mut bytes);
        writer.flush(false, &mut bytes);
        writer.packet(&[1, 2, 3], 960, &mut bytes);
        writer.flush(true, &mut bytes);
        // An Opus stream that ends before its comment header.
        let mut writer = ogg::PageWriter::new(6);
        writer.packet(&opus_head(312), 0, &mut bytes);
        writer.flush(true, &mut bytes);
        // A stray page from an unknown serial: its first page never arrived.
        let mut writer = ogg::PageWriter::new(9);
        let mut lost = vec![];
        writer.packet(&[0], 0, &mut lost);
        writer.flush(false, &mut lost);
        writer.packet(&[1], 960, &mut bytes);
        writer.flush(false, &mut bytes);
        let (events, demuxer) = demux(&bytes, usize::MAX);
        assert!(events.is_empty(), "{events:?}");
        assert_eq!(demuxer.stray_pages, 1);
    }

    #[test]
    fn tag_keys_are_validated_and_comments_without_equals_are_skipped() {
        let mut muxer = Muxer::new(DEFAULT_BITRATE, 1).unwrap();
        let mut bytes = vec![];
        assert!(muxer.begin(&tags(&[("BAD=KEY", "v")]), &mut bytes).is_err());
        assert!(muxer.begin(&tags(&[("", "v")]), &mut bytes).is_err());
        assert!(muxer.begin(&tags(&[("한글", "v")]), &mut bytes).is_err());
        assert!(bytes.is_empty());

        let mut packet = OPUS_TAGS.to_vec();
        packet.extend_from_slice(&0u32.to_le_bytes());
        packet.extend_from_slice(&2u32.to_le_bytes());
        for comment in ["novalue", "K=v=w"] {
            packet.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            packet.extend_from_slice(comment.as_bytes());
        }
        assert_eq!(parse_tags(&packet), Some(tags(&[("K", "v=w")])));
        packet.truncate(packet.len() - 2);
        assert_eq!(parse_tags(&packet), None);
        assert_eq!(parse_tags(b"OpusTags"), None);
    }

    #[test]
    #[ignore = "Needs ffmpeg on PATH; checks interoperability of the Ogg Opus output"]
    fn ffmpeg_decodes_every_sample_of_a_chained_live_stream() {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let mut muxer = Muxer::new(DEFAULT_BITRATE, 0x1234).unwrap();
        let mut bytes = vec![];
        let pre_skip = codec::Encoder::new(DEFAULT_BITRATE).unwrap().lookahead();
        let frames_per_chain = 100;
        for (i, hz) in [440.0, 660.0, 880.0].into_iter().enumerate() {
            muxer
                .begin(&tags(&[("TITLE", &format!("Tone {i}"))]), &mut bytes)
                .unwrap();
            for frame in sine(codec::FRAME_FRAMES * frames_per_chain, hz).chunks(codec::FRAME_LEN) {
                muxer.frame(frame, &mut bytes).unwrap();
            }
        }
        muxer.end(&mut bytes);
        // A pipe is what listeners see: no seeking to the end for a duration.
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "ogg", "-i", "pipe:0", "-f", "s16le", "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || stdin.write_all(&bytes));
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        assert!(output.status.success());
        // ffmpeg restarts its Opus timeline at each chain minus the pre-skip and
        // reports the resulting step once per boundary. Nothing else may appear.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let boundary_steps = stderr
            .lines()
            .filter(|line| line.contains("non monotonically increasing dts"))
            .count();
        assert_eq!(boundary_steps, 2, "{stderr}");
        assert_eq!(stderr.lines().count(), boundary_steps, "{stderr}");
        // Every chain is trimmed by its own pre-skip and otherwise fully decoded.
        let frames = 3 * (frames_per_chain * codec::FRAME_FRAMES - usize::from(pre_skip));
        assert_eq!(output.stdout.len(), frames * codec::CHANNELS * 2);
    }
}
