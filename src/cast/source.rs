//! Local playback of a cast. [`read_streams`] splits the bytes of a
//! `cast_watch` connection into one [`CastSource`] per logical stream; each
//! source decodes Opus packets into 48 kHz stereo samples for a local output.
use super::{Demuxer, Event, Tags, codec};
use anyhow::Result;
use rodio::{ChannelCount, SampleRate, Source};
use std::{io::Read, sync::mpsc, time::Duration};

/// Packets held for a source that has not consumed them yet: two seconds.
const CHANNEL_PACKETS: usize = 100;
/// Packets discarded after joining mid-stream, while the decoder converges.
const SETTLE_PACKETS: usize = 4;

/// Opus packets of one logical stream, decoded on demand. Reading blocks until
/// the next packet arrives, so it belongs on a decode-ahead thread, never in an
/// audio callback. The source ends when its stream ends or is superseded.
pub struct CastSource {
    packets: mpsc::Receiver<Vec<u8>>,
    decoder: codec::Decoder,
    pcm: Vec<f32>,
    cursor: usize,
    /// Interleaved samples still to discard: the pre-skip, plus the settling
    /// time after a late join.
    skip: usize,
    ended: bool,
}

impl CastSource {
    /// `skip_frames` are discarded first: the pre-skip for a stream heard from
    /// its start, or the settling time while the decoder converges after a
    /// late join.
    fn new(packets: mpsc::Receiver<Vec<u8>>, skip_frames: usize) -> Result<Self> {
        Ok(Self {
            packets,
            decoder: codec::Decoder::new()?,
            pcm: Vec::with_capacity(codec::FRAME_LEN),
            cursor: 0,
            skip: skip_frames * codec::CHANNELS,
            ended: false,
        })
    }
}

impl Iterator for CastSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        loop {
            if self.cursor < self.pcm.len() {
                let sample = self.pcm[self.cursor];
                self.cursor += 1;
                return Some(sample);
            }
            if self.ended {
                return None;
            }
            let Ok(packet) = self.packets.recv() else {
                self.ended = true;
                return None;
            };
            let decoded = match self.decoder.decode(&packet) {
                Ok(samples) => samples,
                Err(error) => {
                    tracing::warn!("Undecodable cast packet: {error:#}");
                    match self.decoder.conceal() {
                        Ok(samples) => samples,
                        Err(_) => continue,
                    }
                }
            };
            let dropped = self.skip.min(decoded.len());
            self.skip -= dropped;
            self.pcm.clear();
            self.pcm.extend_from_slice(&decoded[dropped..]);
            self.cursor = 0;
        }
    }
}

impl Source for CastSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> ChannelCount {
        ChannelCount::new(codec::CHANNELS as u16).unwrap()
    }
    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(codec::SAMPLE_RATE).unwrap()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// A logical stream that has started delivering audio.
pub struct StreamStart {
    pub serial: u32,
    pub tags: Tags,
    /// Stream time of the first sample the source yields: zero when the stream
    /// was heard from its start, later for a listener that joined mid-stream.
    pub offset_ms: u64,
    pub source: CastSource,
}

impl StreamStart {
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

struct Current {
    serial: u32,
    sender: Option<mpsc::SyncSender<Vec<u8>>>,
    pending: Option<Pending>,
    dropped: u64,
}

/// Headers seen; the first page's packets wait for that page's granule, which
/// tells where in the stream a late joiner came in.
struct Pending {
    tags: Tags,
    pre_skip: u16,
    late_join: bool,
    packets: Vec<Vec<u8>>,
}

type Started = (StreamStart, Vec<Vec<u8>>, mpsc::SyncSender<Vec<u8>>);

impl Pending {
    fn start(self, serial: u32, granule: u64) -> Result<Started> {
        let decoded: u64 = self
            .packets
            .iter()
            .map(|packet| codec::packet_frames(packet).unwrap_or(codec::FRAME_FRAMES) as u64)
            .sum();
        // Decoder output index of the first sample; audible time starts pre_skip later.
        let first = granule.saturating_sub(decoded);
        let skip_frames = if self.late_join {
            SETTLE_PACKETS * codec::FRAME_FRAMES
        } else {
            usize::from(self.pre_skip)
        };
        let offset_frames = (first + skip_frames as u64).saturating_sub(u64::from(self.pre_skip));
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_PACKETS);
        let source = CastSource::new(receiver, skip_frames)?;
        Ok((
            StreamStart {
                serial,
                tags: self.tags,
                offset_ms: offset_frames * 1000 / u64::from(codec::SAMPLE_RATE),
                source,
            },
            self.packets,
            sender,
        ))
    }
}

/// Read a cast until EOF, handing each logical stream to `on_start` once its
/// first page is complete. Returning `false` from `on_start` stops reading.
/// Packets of a stream whose source was dropped are discarded; packets that
/// arrive faster than a source consumes them are dropped rather than blocking
/// the connection.
pub fn read_streams(
    mut reader: impl Read,
    mut on_start: impl FnMut(StreamStart) -> bool,
) -> std::io::Result<()> {
    let mut demuxer = Demuxer::default();
    let mut buffer = [0u8; 16 * 1024];
    let mut current: Option<Current> = None;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        demuxer.push(&buffer[..read]);
        while let Some(event) = demuxer.pop() {
            match event {
                Event::Start {
                    serial,
                    pre_skip,
                    tags,
                } => {
                    current = Some(Current {
                        serial,
                        sender: None,
                        pending: Some(Pending {
                            tags,
                            pre_skip,
                            late_join: false,
                            packets: vec![],
                        }),
                        dropped: 0,
                    });
                }
                Event::Gap { serial } => {
                    if let Some(stream) = &mut current
                        && stream.serial == serial
                        && let Some(pending) = &mut stream.pending
                        && pending.packets.is_empty()
                    {
                        pending.late_join = true;
                    }
                }
                Event::Packet {
                    serial,
                    data,
                    granule,
                } => {
                    let Some(stream) = &mut current else { continue };
                    if stream.serial != serial {
                        continue;
                    }
                    let deliver = if let Some(pending) = &mut stream.pending {
                        pending.packets.push(data);
                        let Some(granule) = granule else { continue };
                        let pending = stream.pending.take().unwrap();
                        match pending.start(serial, granule) {
                            Ok((start, packets, sender)) => {
                                stream.sender = Some(sender);
                                if !on_start(start) {
                                    return Ok(());
                                }
                                packets
                            }
                            Err(error) => {
                                tracing::error!("Cannot decode the cast: {error:#}");
                                continue;
                            }
                        }
                    } else {
                        vec![data]
                    };
                    let Some(sender) = &stream.sender else {
                        continue;
                    };
                    for packet in deliver {
                        match sender.try_send(packet) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(_)) => {
                                stream.dropped += 1;
                                if stream.dropped == 1 {
                                    tracing::warn!(
                                        "Local output is not consuming the cast; dropping packets"
                                    );
                                }
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                stream.sender = None;
                                break;
                            }
                        }
                    }
                }
                Event::End { serial } => {
                    if current
                        .as_ref()
                        .is_some_and(|stream| stream.serial == serial)
                    {
                        current = None;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast::{
        DEFAULT_BITRATE, Muxer,
        codec::tests::{sine, snr_db},
    };
    use std::io::Cursor;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn each_logical_stream_becomes_one_source_that_ends_when_superseded() {
        let mut muxer = Muxer::new(DEFAULT_BITRATE, 40).unwrap();
        let mut bytes = vec![];
        let first = sine(codec::FRAME_FRAMES * 30, 440.0);
        let second = sine(codec::FRAME_FRAMES * 15, 660.0);
        muxer
            .begin(&tags(&[("VTAMP_POSITION_MS", "1000")]), &mut bytes)
            .unwrap();
        for frame in first.chunks(codec::FRAME_LEN) {
            muxer.frame(frame, &mut bytes).unwrap();
        }
        muxer
            .begin(&tags(&[("VTAMP_POSITION_MS", "2000")]), &mut bytes)
            .unwrap();
        for frame in second.chunks(codec::FRAME_LEN) {
            muxer.frame(frame, &mut bytes).unwrap();
        }
        muxer.end(&mut bytes);

        let mut starts = vec![];
        read_streams(Cursor::new(&bytes), |start| {
            starts.push(start);
            true
        })
        .unwrap();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].tag("VTAMP_POSITION_MS"), Some("1000"));
        assert_eq!(starts[1].tag("VTAMP_POSITION_MS"), Some("2000"));
        assert_eq!((starts[0].serial, starts[1].serial), (40, 41));
        assert_eq!((starts[0].offset_ms, starts[1].offset_ms), (0, 0));
        let decoded: Vec<Vec<f32>> = starts
            .into_iter()
            .map(|start| start.source.collect())
            .collect();
        // The pre-skip is dropped and the encoder's tail is never flushed.
        let pre_skip = usize::from(codec::Encoder::new(DEFAULT_BITRATE).unwrap().lookahead());
        assert_eq!(decoded[0].len(), first.len() - pre_skip * codec::CHANNELS);
        assert_eq!(decoded[1].len(), second.len() - pre_skip * codec::CHANNELS);
        let settle = codec::FRAME_LEN * 5;
        let snr = snr_db(&first[settle..], &decoded[0][settle..]);
        assert!(snr > 18.0, "SNR {snr} dB");
        let snr = snr_db(&second[settle..], &decoded[1][settle..]);
        assert!(snr > 18.0, "SNR {snr} dB");
    }

    #[test]
    fn late_joiners_settle_and_a_declined_start_stops_reading() {
        let mut muxer = Muxer::new(DEFAULT_BITRATE, 7).unwrap();
        let mut headers = vec![];
        muxer.begin(&tags(&[("TITLE", "x")]), &mut headers).unwrap();
        let audio = sine(codec::FRAME_FRAMES * 40, 300.0);
        let mut pages = vec![];
        let mut page_starts = vec![];
        for (i, frame) in audio.chunks(codec::FRAME_LEN).enumerate() {
            if i % 5 == 0 {
                page_starts.push(pages.len());
            }
            muxer.frame(frame, &mut pages).unwrap();
        }
        muxer.end(&mut pages);
        // Join at the third audio page (packets 11-15), as the hub replays headers.
        // Pages leave one packet late, so page_starts[3] is where page 3 begins.
        let mut late = headers.clone();
        late.extend_from_slice(&pages[page_starts[3]..]);
        let mut starts = vec![];
        read_streams(Cursor::new(&late), |start| {
            starts.push(start);
            true
        })
        .unwrap();
        assert_eq!(starts.len(), 1);
        let start = starts.pop().unwrap();
        let pre_skip = usize::from(codec::Encoder::new(DEFAULT_BITRATE).unwrap().lookahead());
        // Packets 11..=40 arrived. The first audible sample is the start of packet
        // 15, after the settling time: stream frame (10 + 4) * 960 minus the pre-skip.
        let first_frame = (10 + SETTLE_PACKETS) * codec::FRAME_FRAMES - pre_skip;
        assert_eq!(
            start.offset_ms,
            (first_frame * 1000 / codec::SAMPLE_RATE as usize) as u64
        );
        let decoded: Vec<f32> = start.source.collect();
        assert_eq!(
            decoded.len(),
            30 * codec::FRAME_LEN - SETTLE_PACKETS * codec::FRAME_LEN
        );
        // Audio after the settling time matches the original at that offset.
        let settle = codec::FRAME_LEN * 5;
        let from = first_frame * codec::CHANNELS;
        let snr = snr_db(&audio[from + settle..], &decoded[settle..]);
        assert!(snr > 18.0, "SNR {snr} dB");

        let mut whole = headers;
        whole.extend_from_slice(&pages);
        let mut seen = 0;
        read_streams(Cursor::new(&whole), |_| {
            seen += 1;
            false
        })
        .unwrap();
        assert_eq!(seen, 1);
    }
}
