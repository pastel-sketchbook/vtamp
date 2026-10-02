//! AAC decoding through the installed AudioToolbox framework, without bundling a codec.

use anyhow::{Context, Result, ensure};
use objc2_audio_toolbox::*;
use objc2_core_audio_types::*;
use objc2_core_foundation::CFURL;
use rodio::{ChannelCount, SampleRate, Source, source::SeekError};
use std::{
    io,
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::Arc,
    time::Duration,
};

const BUFFER_FRAMES: u32 = 4096;

struct AudioFile(NonNull<OpaqueExtAudioFile>);

// SAFETY: The handle is exclusively owned, never cloned or shared. AudioToolbox's
// sequential file API requires serialized access, not a particular thread. The
// source is moved to the decoder worker after initialization; reads/seeks require
// &mut self, and disposal stays on the worker or control thread.
// Deliberately do not implement Sync.
unsafe impl Send for AudioFile {}

impl Drop for AudioFile {
    fn drop(&mut self) {
        // SAFETY: This is the unique owner of a successfully opened file.
        unsafe {
            ExtAudioFileDispose(self.0.as_ptr());
        }
    }
}

fn check(status: i32, operation: &str) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation} failed (OSStatus {status})"
        )))
    }
}

pub(super) struct AacDecoder {
    file: AudioFile,
    path: PathBuf,
    channels: ChannelCount,
    rate: SampleRate,
    frames: u64,
    next_frame: u64,
    samples: Vec<f32>,
    cursor: usize,
    available: usize,
    ended: bool,
}

impl AacDecoder {
    /// None means the OS could not identify AAC; let the existing decoder probe it.
    /// Once AAC is identified, all setup/decode failures are errors, never fallback.
    pub(super) fn open(path: &Path) -> Result<Option<Self>> {
        let url = CFURL::from_file_path(path).context("Cannot create audio file URL")?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: The URL and writable output pointer live through the call.
        let status = unsafe { ExtAudioFileOpenURL(&url, NonNull::from(&mut raw)) };
        // Also adopt a non-null handle on an unsuccessful open, so cleanup is assured.
        let file = NonNull::new(raw).map(AudioFile);
        if status != 0 {
            return Ok(None);
        }
        let file = file.context("AudioToolbox returned no audio file")?;
        let mut format = AudioStreamBasicDescription {
            mSampleRate: 0.0,
            mFormatID: 0,
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: 0,
            mBytesPerFrame: 0,
            mChannelsPerFrame: 0,
            mBitsPerChannel: 0,
            mReserved: 0,
        };
        let mut size = size_of_val(&format) as u32;
        // SAFETY: Handle is live; property size and destination match its ASBD type.
        let status = unsafe {
            ExtAudioFileGetProperty(
                file.0.as_ptr(),
                kExtAudioFileProperty_FileDataFormat,
                NonNull::from(&mut size),
                NonNull::from(&mut format).cast(),
            )
        };
        if status != 0 {
            return Ok(None);
        }
        if ![
            kAudioFormatMPEG4AAC,
            kAudioFormatMPEG4AAC_HE,
            kAudioFormatMPEG4AAC_HE_V2,
            kAudioFormatMPEG4AAC_LD,
            kAudioFormatMPEG4AAC_ELD,
            kAudioFormatMPEG4AAC_ELD_SBR,
            kAudioFormatMPEG4AAC_ELD_V2,
            kAudioFormatMPEG4AAC_Spatial,
            kAudioFormatMPEGD_USAC,
        ]
        .contains(&format.mFormatID)
        {
            return Ok(None);
        }

        let channels = ChannelCount::new(u16::try_from(format.mChannelsPerFrame)?)
            .context("AAC has no channels")?;
        ensure!(
            format.mSampleRate.is_finite()
                && format.mSampleRate >= 1.0
                && format.mSampleRate <= u32::MAX as f64
                && format.mSampleRate.fract() == 0.0,
            "Invalid AAC sample rate"
        );
        let rate = SampleRate::new(format.mSampleRate as u32).context("AAC has no sample rate")?;
        let mut frames: i64 = 0;
        size = size_of_val(&frames) as u32;
        // SAFETY: FileLengthFrames is an i64 property, with matching writable storage.
        check(
            unsafe {
                ExtAudioFileGetProperty(
                    file.0.as_ptr(),
                    kExtAudioFileProperty_FileLengthFrames,
                    NonNull::from(&mut size),
                    NonNull::from(&mut frames).cast(),
                )
            },
            "Read AAC length",
        )?;
        let frames = u64::try_from(frames).context("Invalid AAC frame count")?;
        let mut pcm = AudioStreamBasicDescription {
            mSampleRate: format.mSampleRate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: u32::from(channels.get()) * 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: u32::from(channels.get()) * 4,
            mChannelsPerFrame: u32::from(channels.get()),
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        // SAFETY: The live file copies this correctly sized PCM format description.
        check(
            unsafe {
                ExtAudioFileSetProperty(
                    file.0.as_ptr(),
                    kExtAudioFileProperty_ClientDataFormat,
                    size_of_val(&pcm) as u32,
                    NonNull::from(&mut pcm).cast(),
                )
            },
            "Configure AAC PCM output",
        )?;
        let mut source = Self {
            file,
            path: path.to_owned(),
            channels,
            rate,
            frames,
            next_frame: 0,
            samples: vec![0.0; BUFFER_FRAMES as usize * usize::from(channels.get())],
            cursor: 0,
            available: 0,
            ended: false,
        };
        source.refill()?;
        Ok(Some(source))
    }

    fn refill(&mut self) -> io::Result<()> {
        self.cursor = 0;
        self.available = 0;
        let mut frames = self
            .frames
            .saturating_sub(self.next_frame)
            .min(u64::from(BUFFER_FRAMES)) as u32;
        if frames == 0 {
            self.ended = true;
            return Ok(());
        }
        let requested = frames;
        let mut buffers = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::from(self.channels.get()),
                mDataByteSize: (self.samples.len() * size_of::<f32>()) as u32,
                mData: self.samples.as_mut_ptr().cast(),
            }],
        };
        // SAFETY: An interleaved format requires exactly one buffer. Its allocation
        // holds BUFFER_FRAMES of all channels, and is borrowed only for this call.
        let status = unsafe {
            ExtAudioFileRead(
                self.file.0.as_ptr(),
                NonNull::from(&mut frames),
                NonNull::from(&mut buffers),
            )
        };
        check(status, "Read AAC PCM")?;
        if frames > requested {
            return Err(io::Error::other("AudioToolbox returned too many frames"));
        }
        self.next_frame += u64::from(frames);
        self.available = frames as usize * usize::from(self.channels.get());
        self.ended = frames == 0;
        Ok(())
    }

    fn duration(&self) -> Duration {
        let rate = u64::from(self.rate.get());
        Duration::new(
            self.frames / rate,
            (self.frames % rate * 1_000_000_000 / rate) as u32,
        )
    }

    fn seek(&mut self, position: Duration) -> io::Result<()> {
        // A duration rounded to nanoseconds may lie just before its final frame.
        // Seeking to the reported duration must nevertheless reach EOF.
        let frame = if position >= self.duration() {
            self.frames
        } else {
            (position.as_nanos() * u128::from(self.rate.get()) / 1_000_000_000) as u64
        };
        self.cursor = 0;
        self.available = 0;
        self.ended = true;
        if frame == self.frames {
            self.next_frame = frame;
            return Ok(());
        }
        // SAFETY: Exclusive access to a live read-only handle; frame <= i64::MAX
        // because the file length came from an i64 property.
        check(
            unsafe { ExtAudioFileSeek(self.file.0.as_ptr(), frame as i64) },
            "Seek AAC",
        )?;
        self.next_frame = frame;
        self.ended = false;
        self.refill()
    }
}

impl Iterator for AacDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.cursor == self.available {
            if self.ended {
                return None;
            }
            if let Err(error) = self.refill() {
                tracing::warn!(path = %self.path.display(), %error, "AudioToolbox AAC decoding failed");
                self.ended = true;
                return None;
            }
            if self.ended {
                return None;
            }
        }
        let sample = self.samples[self.cursor];
        self.cursor += 1;
        Some(sample)
    }
}

impl Source for AacDecoder {
    fn current_span_len(&self) -> Option<usize> {
        if self.ended { Some(0) } else { None }
    }
    fn channels(&self) -> ChannelCount {
        self.channels
    }
    fn sample_rate(&self) -> SampleRate {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration())
    }
    fn try_seek(&mut self, pos: Duration) -> std::result::Result<(), SeekError> {
        self.seek(pos).map_err(|error| {
            SeekError::Other(Arc::new(io::Error::other(format!(
                "AudioToolbox AAC decoder: {}: {error}",
                self.path.display()
            ))))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn aac_streams_to_eof_and_seeks_across_buffers() {
        for name in ["extended-mdat.m4a", "stereo-aac.m4a"] {
            let mut source = AacDecoder::open(&fixture(name)).unwrap().unwrap();
            let channels = usize::from(source.channels().get());
            let rate = source.sample_rate().get();
            let duration = source.total_duration().unwrap();
            let expected_frames = source.frames as usize;
            // A bound makes a regression in EOF handling fail rather than hang.
            let samples: Vec<_> = source
                .by_ref()
                .take(expected_frames * channels + 1)
                .collect();
            assert_eq!(samples.len(), expected_frames * channels);
            assert!(samples.iter().all(|s| s.is_finite()));
            assert!(samples.iter().any(|s| s.abs() > 0.01));
            assert_eq!(source.next(), None);
            assert_eq!(source.current_span_len(), Some(0));

            for position in [
                Duration::from_millis(100),
                Duration::ZERO,
                Duration::from_millis(150),
            ] {
                source.try_seek(position).unwrap();
                let first_frame = (position.as_nanos() * u128::from(rate) / 1_000_000_000) as usize;
                let tail: Vec<_> = source
                    .by_ref()
                    .take(expected_frames * channels + 1)
                    .collect();
                let expected = &samples[first_frame * channels..];
                assert_eq!(tail.len(), expected.len(), "{name}: {position:?}");
                // AAC's synthesized noise can differ after a decoder reset. The
                // tolerance is below one frame's phase shift for these test tones;
                // frame counts must still match exactly, including stereo alignment.
                assert!(
                    tail.iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 0.002),
                    "{name}: {position:?}"
                );
            }
            for end in [duration, Duration::MAX] {
                source.try_seek(end).unwrap();
                assert_eq!(source.next(), None);
            }
            // Seeking after EOF must make the source usable again.
            source.try_seek(Duration::ZERO).unwrap();
            assert!(source.take(1000).any(|s| s.abs() > 0.01));
        }
    }

    #[test]
    fn codec_selection_uses_content_and_preserves_other_decoders() {
        let dir = tempfile::tempdir().unwrap();
        let renamed = dir.path().join("음악 with spaces.wav");
        std::fs::copy(fixture("stereo-aac.m4a"), &renamed).unwrap();
        assert!(AacDecoder::open(&renamed).unwrap().is_some());
        assert!(
            super::super::decode_file(&renamed)
                .unwrap()
                .take(1000)
                .any(|s| s.abs() > 0.01)
        );
        for name in ["stereo-alac.m4a", "stereo.wav"] {
            assert!(AacDecoder::open(&fixture(name)).unwrap().is_none());
            std::fs::copy(fixture(name), &renamed).unwrap();
            assert!(AacDecoder::open(&renamed).unwrap().is_none());
        }
    }

    #[test]
    fn damaged_aac_reports_native_error_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("damaged.m4a");
        let mut bytes = std::fs::read(fixture("extended-mdat.m4a")).unwrap();
        let mdat = bytes.windows(4).position(|b| b == b"mdat").unwrap();
        let size = u64::from_be_bytes(bytes[mdat + 4..mdat + 12].try_into().unwrap()) as usize;
        bytes[mdat + 12..mdat - 4 + size].fill(0xff);
        std::fs::write(&path, bytes).unwrap();
        let error = super::super::decode_file(&path)
            .err()
            .expect("damaged AAC must fail");
        let error = format!("{error:#}");
        assert!(error.contains("AudioToolbox AAC decoder"), "{error}");
        assert!(error.contains("damaged.m4a"), "{error}");
    }

    #[test]
    fn initialized_decoder_can_move_to_consumer_thread() {
        let source = AacDecoder::open(&fixture("stereo-aac.m4a"))
            .unwrap()
            .unwrap();
        let count = std::thread::spawn(move || source.take(14400 * 2 + 1).count())
            .join()
            .unwrap();
        assert_eq!(count, 14400 * 2);
    }
}
