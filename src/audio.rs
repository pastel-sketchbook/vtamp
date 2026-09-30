use anyhow::{Context, Result};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
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
    fn take_error(&mut self) -> Option<String> {
        None
    }
}

#[derive(Default)]
pub struct RodioBackend {
    // Player must be dropped before its output stream.
    player: Option<Player>,
    device: Option<MixerDeviceSink>,
    error: Arc<Mutex<Option<String>>>,
}

impl PlaybackBackend for RodioBackend {
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()> {
        let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
        let source = rodio::Decoder::try_from(file).context("Unsupported or damaged audio")?;
        if self.device.is_none() {
            let error = self.error.clone();
            self.device = Some(
                DeviceSinkBuilder::from_default_device()
                    .context("No output device available")?
                    .with_error_callback(move |e| {
                        if let Ok(mut slot) = error.lock() {
                            *slot = Some(format!("Audio device error: {e}"));
                        }
                    })
                    .open_sink_or_fallback()
                    .context("Cannot open the default output device")?,
            );
        }
        self.stop();
        let player = Player::connect_new(self.device.as_ref().unwrap().mixer());
        player.pause();
        player.set_volume(f32::from(volume) / 100.0);
        player.append(source);
        if position_ms > 0 {
            player
                .try_seek(Duration::from_millis(position_ms))
                .context("This audio file cannot seek to the saved position")?;
        }
        if !paused {
            player.play();
        }
        self.player = Some(player);
        Ok(())
    }
    fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
    }
    fn resume(&mut self) {
        if let Some(p) = &self.player {
            p.play();
        }
    }
    fn stop(&mut self) {
        if let Some(p) = self.player.take() {
            p.stop();
        }
    }
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        self.player
            .as_ref()
            .context("No audio is loaded")?
            .try_seek(Duration::from_millis(position_ms))?;
        Ok(())
    }
    fn volume(&mut self, value: u8) {
        if let Some(p) = &self.player {
            p.set_volume(f32::from(value) / 100.0);
        }
    }
    fn position(&self) -> u64 {
        self.player
            .as_ref()
            .map_or(0, |p| p.get_pos().as_millis() as u64)
    }
    fn finished(&self) -> bool {
        self.player.as_ref().is_some_and(Player::empty)
    }
    fn take_error(&mut self) -> Option<String> {
        let error = self.error.lock().ok()?.take();
        if error.is_some() {
            self.stop();
            self.device = None;
        }
        error
    }
}
