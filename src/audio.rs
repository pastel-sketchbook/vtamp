use anyhow::{Context, Result};
use rodio::{
    DeviceSinkBuilder, MixerDeviceSink, Player, Source,
    cpal::{
        self, DeviceId,
        traits::{DeviceTrait, HostTrait},
    },
};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub trait PlaybackBackend: Send {
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    fn seek(&mut self, position_ms: u64) -> Result<()>;
    fn volume(&mut self, value: u8);
    fn position(&self) -> u64;
    fn finished(&self) -> bool;
    /// Report and discard an unusable output, including a changed default device.
    /// The engine saves the position before calling this and reloads the same track.
    fn output_event(&mut self) -> Option<String> {
        None
    }
}

const DEVICE_CHECK_INTERVAL: Duration = Duration::from_millis(500);
const STALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub(crate) struct OutputUnavailable;
impl std::fmt::Display for OutputUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Audio output unavailable")
    }
}
impl std::error::Error for OutputUnavailable {}

#[derive(Default)]
pub struct RodioBackend {
    // Player must be dropped before its output stream.
    player: Option<Player>,
    device: Option<MixerDeviceSink>,
    device_id: Option<DeviceId>,
    last_device_check: Option<Instant>,
    progress: ProgressWatch,
    path: Option<PathBuf>,
    volume: u8,
    position_offset_ms: u64,
    error: Arc<Mutex<Option<String>>>,
}

#[derive(Default)]
struct ProgressWatch {
    last: Option<(u64, Instant)>,
}
impl ProgressWatch {
    fn stalled(&mut self, position: u64, playing: bool, now: Instant) -> bool {
        if !playing {
            self.last = None;
            return false;
        }
        if let Some((previous, since)) = self.last
            && previous == position
        {
            return now.duration_since(since) >= STALL_TIMEOUT;
        }
        self.last = Some((position, now));
        false
    }
}

impl RodioBackend {
    fn reset_output(&mut self) {
        self.stop();
        self.device = None;
        self.device_id = None;
        // Old stream callbacks cannot invalidate a replacement stream.
        self.error = Arc::default();
    }

    fn ensure_output(&mut self) -> Result<()> {
        let Some(device) = cpal::default_host().default_output_device() else {
            self.reset_output();
            anyhow::bail!("No output device available");
        };
        let id = device
            .id()
            .context("Cannot identify the default output device")?;
        let failed = self
            .error
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .is_some();
        if self.device_id.as_ref() != Some(&id) || failed {
            self.reset_output();
        }
        if self.device.is_none() {
            let error = self.error.clone();
            self.device = Some(
                DeviceSinkBuilder::from_device(device)
                    .context("Cannot configure the default output device")?
                    .with_error_callback(move |e| {
                        if let Ok(mut slot) = error.lock() {
                            *slot = Some(format!("Audio device error: {e}"));
                        }
                    })
                    .open_sink_or_fallback()
                    .context("Cannot open the default output device")?,
            );
            self.device_id = Some(id);
        }
        self.last_device_check = Some(Instant::now());
        Ok(())
    }
}

impl PlaybackBackend for RodioBackend {
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()> {
        let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
        let mut source = rodio::Decoder::try_from(file).context("Unsupported or damaged audio")?;
        // Seek the decoder directly: Player::try_seek waits for the audio callback,
        // which may never arrive while a Bluetooth output is disappearing.
        if position_ms > 0 {
            source
                .try_seek(Duration::from_millis(position_ms))
                .context("This audio file cannot seek to the saved position")?;
        }
        self.ensure_output().context(OutputUnavailable)?;
        self.stop();
        let player = Player::connect_new(self.device.as_ref().unwrap().mixer());
        player.pause();
        player.set_volume(f32::from(volume) / 100.0);
        player.append(source);
        if !paused {
            player.play();
        }
        self.player = Some(player);
        self.path = Some(path.to_owned());
        self.volume = volume;
        self.position_offset_ms = position_ms;
        Ok(())
    }
    fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
        self.progress = ProgressWatch::default();
    }
    fn resume(&mut self) {
        if let Some(p) = &self.player {
            p.play();
        }
        self.progress = ProgressWatch::default();
    }
    fn stop(&mut self) {
        if let Some(p) = self.player.take() {
            p.stop();
        }
        self.path = None;
        self.position_offset_ms = 0;
        self.progress = ProgressWatch::default();
    }
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        let path = self.path.clone().context("No audio is loaded")?;
        let paused = self.player.as_ref().is_none_or(Player::is_paused);
        self.load(&path, position_ms, self.volume, paused)
    }
    fn volume(&mut self, value: u8) {
        self.volume = value;
        if let Some(p) = &self.player {
            p.set_volume(f32::from(value) / 100.0);
        }
    }
    fn position(&self) -> u64 {
        self.player.as_ref().map_or(0, |p| {
            self.position_offset_ms
                .saturating_add(p.get_pos().as_millis() as u64)
        })
    }
    fn finished(&self) -> bool {
        self.player.as_ref().is_some_and(Player::empty)
    }
    fn output_event(&mut self) -> Option<String> {
        let mut event = self.error.lock().ok()?.take();
        let now = Instant::now();
        if event.is_none() && self.device.is_some() {
            if self
                .last_device_check
                .is_none_or(|t| now.duration_since(t) >= DEVICE_CHECK_INTERVAL)
            {
                self.last_device_check = Some(now);
                let current = cpal::default_host()
                    .default_output_device()
                    .and_then(|d| d.id().ok());
                if current != self.device_id {
                    event = Some("Default audio output changed".into());
                }
            }
            let playing = self
                .player
                .as_ref()
                .is_some_and(|p| !p.is_paused() && !p.empty());
            if self.progress.stalled(self.position(), playing, now) {
                event = Some("Audio output stopped consuming samples".into());
            }
        }
        if event.is_some() {
            self.reset_output();
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_output_requires_sustained_lack_of_progress_and_ignores_pause() {
        let mut watch = ProgressWatch::default();
        let now = Instant::now();
        assert!(!watch.stalled(1000, true, now));
        assert!(!watch.stalled(1000, true, now + Duration::from_secs(2)));
        assert!(watch.stalled(1000, true, now + STALL_TIMEOUT));
        assert!(!watch.stalled(1200, true, now + STALL_TIMEOUT));
        assert!(!watch.stalled(1200, false, now + Duration::from_secs(10)));
        assert!(!watch.stalled(1200, true, now + Duration::from_secs(20)));
    }

    #[test]
    #[ignore = "Needs a real output device and VTAMP_TEST_AUDIO_FILE (at least 15 seconds); plays muted"]
    fn real_output_reopens_and_seeks_without_waiting_for_audio_callbacks() {
        let path = PathBuf::from(std::env::var("VTAMP_TEST_AUDIO_FILE").unwrap());
        let mut backend = RodioBackend::default();
        backend.load(&path, 5000, 0, false).unwrap();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            assert!(backend.output_event().is_none());
        }
        let position = backend.position();
        assert!((6000..8000).contains(&position), "{position}");

        // Exercise the default-device comparison against real CoreAudio, without
        // changing the user's system output or requiring physical headphones.
        backend.device_id = None;
        backend.last_device_check = None;
        assert_eq!(
            backend.output_event().as_deref(),
            Some("Default audio output changed")
        );
        backend.load(&path, position, 0, true).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(backend.position(), position);
        backend.seek(10000).unwrap();
        assert_eq!(backend.position(), 10000);
        backend.resume();
        std::thread::sleep(Duration::from_millis(300));
        assert!(backend.position() > 10000);

        // An error from the old stream must not affect its replacement.
        let old_error = backend.error.clone();
        *old_error.lock().unwrap() = Some("Simulated stream error".into());
        assert!(backend.output_event().is_some());
        backend.load(&path, 10000, 0, false).unwrap();
        *old_error.lock().unwrap() = Some("Late callback".into());
        assert!(backend.output_event().is_none());
        backend.stop();
    }
}
