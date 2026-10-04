//! File loudness measurements. No decoding or metering runs in the output callback.
use anyhow::{Context, Result, ensure};
use ebur128::{EbuR128, Mode};
use rodio::Source;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

pub const TARGET_LUFS: f64 = -18.0;
const PEAK_DBTP: f64 = -1.0;
const MAX_BOOST_DB: f64 = 12.0;
const ANALYSIS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    modified: u128,
    bytes: u64,
    version: u32,
}
impl Fingerprint {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = path.metadata()?;
        ensure!(metadata.is_file(), "Not a regular audio file");
        Ok(Self {
            modified: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
            bytes: metadata.len(),
            version: ANALYSIS_VERSION,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
}
impl Measurement {
    pub fn gain_db(&self) -> f64 {
        (TARGET_LUFS - self.integrated_lufs)
            .min(PEAK_DBTP - self.true_peak_dbtp)
            .min(MAX_BOOST_DB)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub fingerprint: Fingerprint,
    pub measurement: Option<Measurement>,
    pub error: Option<String>,
}
impl Analysis {
    pub fn matches(&self, path: &Path) -> bool {
        Fingerprint::read(path).is_ok_and(|f| f == self.fingerprint)
    }
    pub fn gain_db(&self) -> f64 {
        self.measurement.as_ref().map_or(0.0, Measurement::gain_db)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Status {
    pub enabled: bool,
    pub target_lufs: f64,
    pub ready: usize,
    pub pending: usize,
    pub failed: usize,
    pub unmeasurable: usize,
    pub applied_gain_db: Option<f64>,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            enabled: true,
            target_lufs: TARGET_LUFS,
            ready: 0,
            pending: 0,
            failed: 0,
            unmeasurable: 0,
            applied_gain_db: None,
        }
    }
}

/// Use exactly the platform's playback decoder, including AudioToolbox AAC.
/// Unknown multichannel layouts are left untouched rather than guessed.
pub fn analyze(path: &Path, cancelled: &AtomicBool) -> Result<Option<Measurement>> {
    measure(crate::audio::decode_file(path)?, cancelled)
}
fn measure(
    mut source: Box<dyn Source + Send>,
    cancelled: &AtomicBool,
) -> Result<Option<Measurement>> {
    let channels = usize::from(source.channels().get());
    if channels > 2 {
        return Ok(None);
    }
    // Histogram mode bounds memory even for very long files.
    let mut meter = EbuR128::new(
        channels as u32,
        source.sample_rate().get(),
        Mode::I | Mode::TRUE_PEAK | Mode::HISTOGRAM,
    )?;
    // A mono source is reproduced in both front channels by our playback converter.
    if channels == 1 {
        meter.set_channel(0, ebur128::Channel::DualMono)?;
    }
    let mut block = vec![0.0; 4096 * channels];
    loop {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Loudness analysis cancelled"
        );
        let mut len = 0;
        for value in &mut block {
            let Some(sample) = source.next() else {
                break;
            };
            ensure!(sample.is_finite(), "Non-finite audio sample");
            *value = sample;
            len += 1;
        }
        ensure!(len % channels == 0, "Incomplete audio frame");
        if len == 0 {
            break;
        }
        meter.add_frames_f32(&block[..len])?;
    }
    let integrated_lufs = meter.loudness_global()?;
    // Flush the interpolator's delayed tail without adding silence to loudness.
    // 64 frames exceed its longest (24-frame) per-phase history.
    block[..64 * channels].fill(0.0);
    meter.add_frames_f32(&block[..64 * channels])?;
    let mut peak = 0.0_f64;
    for channel in 0..channels {
        peak = peak.max(meter.true_peak(channel as u32)?);
    }
    let true_peak_dbtp = 20.0 * peak.log10();
    if !integrated_lufs.is_finite() || !true_peak_dbtp.is_finite() {
        return Ok(None);
    }
    Ok(Some(Measurement {
        integrated_lufs,
        true_peak_dbtp,
    }))
}

/// Results are committed only while they still describe the same file.
pub fn analyze_file(
    path: &Path,
    fingerprint: Fingerprint,
    cancelled: &AtomicBool,
) -> Result<Analysis> {
    let measured = analyze(path, cancelled);
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Loudness analysis cancelled"
    );
    ensure!(
        Fingerprint::read(path).context("Audio changed during analysis")? == fingerprint,
        "Audio changed during analysis"
    );
    Ok(match measured {
        Ok(measurement) => Analysis {
            fingerprint,
            measurement,
            error: None,
        },
        Err(error) => Analysis {
            fingerprint,
            measurement: None,
            error: Some(format!("{error:#}")),
        },
    })
}

/// A fixed file gain. The remote-cast playback path deliberately does not use this.
pub fn apply(source: Box<dyn Source + Send>, db: f64) -> Box<dyn Source + Send> {
    Box::new(source.amplify(10.0_f64.powf(db / 20.0) as f32))
}

pub type Cache = std::collections::HashMap<PathBuf, Analysis>;

#[cfg(test)]
pub(crate) fn write_tone(path: &Path, amplitude: f32, seconds: u32) {
    use std::io::Write;
    let mut file = std::fs::File::create(path).unwrap();
    let frames = 48000 * seconds;
    let bytes = frames * 2 * 2;
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    for value in [1u16, 2] {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
    for value in [48000u32, 192000] {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
    for value in [4u16, 16] {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
    file.write_all(b"data").unwrap();
    file.write_all(&bytes.to_le_bytes()).unwrap();
    let mut samples = Vec::with_capacity(bytes as usize);
    for i in 0..frames {
        let value = ((i as f32 * 1000.0 * std::f32::consts::TAU / 48000.0).sin()
            * amplitude
            * 32767.0) as i16;
        samples.extend_from_slice(&value.to_le_bytes());
        samples.extend_from_slice(&value.to_le_bytes());
    }
    file.write_all(&samples).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::{ChannelCount, SampleRate, buffer::SamplesBuffer};
    fn tone(amplitude: f32, channels: u16) -> Box<dyn Source + Send> {
        let samples: Vec<f32> = (0..144000)
            .flat_map(|i| {
                let value =
                    (i as f64 * 1000.0 * std::f64::consts::TAU / 48000.0).sin() as f32 * amplitude;
                std::iter::repeat_n(value, channels as usize)
            })
            .collect();
        Box::new(SamplesBuffer::new(
            ChannelCount::new(channels).unwrap(),
            SampleRate::new(48000).unwrap(),
            samples,
        ))
    }
    #[test]
    fn different_levels_converge_without_changing_the_waveform() {
        let cancel = AtomicBool::new(false);
        let mut corrected = vec![];
        for amplitude in [0.1, 0.5] {
            let before = measure(tone(amplitude, 2), &cancel).unwrap().unwrap();
            let gain = before.gain_db();
            let after = measure(apply(tone(amplitude, 2), gain), &cancel)
                .unwrap()
                .unwrap();
            assert!(
                (after.integrated_lufs - TARGET_LUFS).abs() < 0.15,
                "{after:?}"
            );
            assert!(after.true_peak_dbtp <= PEAK_DBTP + 0.1);
            let original: Vec<_> = tone(amplitude, 2).collect();
            let output: Vec<_> = apply(tone(amplitude, 2), gain).collect();
            let scale = 10.0_f64.powf(gain / 20.0) as f32;
            assert!(
                original
                    .iter()
                    .zip(output)
                    .all(|(a, b)| (a * scale - b).abs() < 1e-7)
            );
            corrected.push(after.integrated_lufs);
        }
        assert!((corrected[0] - corrected[1]).abs() < 0.5);
    }
    #[test]
    fn true_peak_includes_interpolation_after_the_last_frame() {
        let mut samples: Vec<_> = tone(0.02, 1).collect();
        let len = samples.len();
        samples[len - 2] = -0.8;
        samples[len - 1] = 0.8;
        let source = |values| {
            Box::new(SamplesBuffer::new(
                ChannelCount::new(1).unwrap(),
                SampleRate::new(48000).unwrap(),
                values,
            )) as Box<dyn Source + Send>
        };
        let end = measure(source(samples.clone()), &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        samples.extend_from_slice(&[0.0; 100]);
        let padded = measure(source(samples), &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert!((end.true_peak_dbtp - padded.true_peak_dbtp).abs() < 0.001);
    }

    #[test]
    fn peak_headroom_and_maximum_boost_take_priority_over_target() {
        assert!(
            (Measurement {
                integrated_lufs: -30.0,
                true_peak_dbtp: -0.2
            }
            .gain_db()
                + 0.8)
                .abs()
                < 1e-9
        );
        assert_eq!(
            Measurement {
                integrated_lufs: -60.0,
                true_peak_dbtp: -50.0
            }
            .gain_db(),
            12.0
        );
    }
    #[test]
    fn mono_matches_the_same_signal_played_in_both_stereo_channels() {
        let cancel = AtomicBool::new(false);
        let mono = measure(tone(0.1, 1), &cancel).unwrap().unwrap();
        let stereo = measure(tone(0.1, 2), &cancel).unwrap().unwrap();
        assert!((mono.integrated_lufs - stereo.integrated_lufs).abs() < 0.1);
        // A stereo 1 kHz sine with -20 dBFS peaks is about -20 LUFS (BS.1770 weighting).
        assert!((stereo.integrated_lufs + 20.0).abs() < 0.2, "{stereo:?}");
    }
    #[test]
    fn silence_short_audio_unknown_layout_and_invalid_samples_are_safe() {
        let cancel = AtomicBool::new(false);
        assert!(measure(tone(0.0, 2), &cancel).unwrap().is_none());
        assert!(measure(tone(0.1, 6), &cancel).unwrap().is_none());
        let source = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(48000).unwrap(),
            vec![0.1; 100],
        );
        assert!(measure(Box::new(source), &cancel).unwrap().is_none());
        let source = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(48000).unwrap(),
            vec![f32::NAN; 100],
        );
        assert!(measure(Box::new(source), &cancel).is_err());
        assert!(measure(tone(0.1, 2), &AtomicBool::new(true)).is_err());
    }
    #[test]
    fn file_results_are_reusable_but_changed_and_removed_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_tone(&path, 0.1, 2);
        let fingerprint = Fingerprint::read(&path).unwrap();
        let result = analyze_file(&path, fingerprint.clone(), &AtomicBool::new(false)).unwrap();
        assert!(result.measurement.is_some());
        assert!(result.matches(&path));
        write_tone(&path, 0.5, 3);
        assert!(!result.matches(&path));
        assert!(analyze_file(&path, fingerprint, &AtomicBool::new(false)).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(!result.matches(&path));
    }
}
