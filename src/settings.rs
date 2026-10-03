//! Client-only preferences; never touches the playback server or its database.
use crate::{platform, theme::Theme};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

/// Rendering style of the TUI spectrum. Colors always come from the active
/// theme's roles; a style chooses the geometry and how roles are mapped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpectrumStyle {
    #[default]
    Bars,
    Gradient,
    Mono,
    Mirror,
    Dots,
    Waterfall,
}

impl SpectrumStyle {
    pub const ALL: [Self; 6] = [
        Self::Bars,
        Self::Gradient,
        Self::Mono,
        Self::Mirror,
        Self::Dots,
        Self::Waterfall,
    ];

    /// Settings identifier; also the literal name shown in the TUI.
    pub fn id(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Gradient => "gradient",
            Self::Mono => "mono",
            Self::Mirror => "mirror",
            Self::Dots => "dots",
            Self::Waterfall => "waterfall",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "video_enabled")]
    pub video: bool,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub spectrum: bool,
    #[serde(default)]
    pub spectrum_style: SpectrumStyle,
}

fn video_enabled() -> bool {
    true
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            spectrum: false,
            spectrum_style: SpectrumStyle::default(),
            video: true,
        }
    }
}
impl Settings {
    pub fn set_video(path: &Path, enabled: bool) -> Result<()> {
        let mut settings = Self::load(path)?;
        settings.video = enabled;
        settings.save(path)
    }

    pub fn set_theme(path: &Path, theme: Theme) -> Result<()> {
        // Explicit theme saves retain the existing repair behavior for invalid files.
        let mut settings = Self::load(path).unwrap_or_default();
        settings.theme = theme;
        settings.save(path)
    }
    pub fn set_spectrum(path: &Path, enabled: bool) -> Result<()> {
        // A visualization toggle must not silently repair/replace a broken theme file.
        let mut settings = Self::load(path)?;
        settings.spectrum = enabled;
        settings.save(path)
    }
    pub fn set_spectrum_style(path: &Path, style: SpectrumStyle) -> Result<()> {
        // Same rule as the visibility toggle: never repair an invalid file here.
        let mut settings = Self::load(path)?;
        settings.spectrum_style = style;
        settings.save(path)
    }
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("Invalid UI settings in {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("Cannot read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("UI settings path has no parent")?;
        platform::private_dir(parent)?;
        let temporary = parent.join(format!(".ui-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.with_context(|| format!("Cannot save UI settings to {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spectrum_and_theme_updates_preserve_each_other_and_legacy_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        fs::write(&path, br#"{"theme":"nord"}"#).unwrap();
        assert!(!Settings::load(&path).unwrap().spectrum);
        assert_eq!(
            Settings::load(&path).unwrap().spectrum_style,
            SpectrumStyle::Bars
        );
        Settings::set_spectrum(&path, true).unwrap();
        assert_eq!(Settings::load(&path).unwrap().theme, Theme::Nord);
        Settings::set_theme(&path, Theme::RosePine).unwrap();
        assert!(Settings::load(&path).unwrap().spectrum);
        Settings::set_spectrum_style(&path, SpectrumStyle::Waterfall).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.theme, Theme::RosePine);
        assert!(loaded.spectrum);
        assert_eq!(loaded.spectrum_style, SpectrumStyle::Waterfall);
        Settings::set_theme(&path, Theme::Nord).unwrap();
        Settings::set_spectrum(&path, false).unwrap();
        assert_eq!(
            Settings::load(&path).unwrap().spectrum_style,
            SpectrumStyle::Waterfall
        );
        fs::write(&path, b"broken").unwrap();
        assert!(Settings::set_spectrum(&path, false).is_err());
        assert!(Settings::set_spectrum_style(&path, SpectrumStyle::Mono).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        fs::write(&path, br#"{"spectrum_style":"neon"}"#).unwrap();
        assert!(Settings::load(&path).is_err());
    }
    #[test]
    fn spectrum_style_ids_round_trip_and_cycle() {
        for style in SpectrumStyle::ALL {
            assert_eq!(serde_json::to_value(style).unwrap(), style.id());
            assert_eq!(
                serde_json::from_value::<SpectrumStyle>(style.id().into()).unwrap(),
                style
            );
        }
        let mut seen = vec![SpectrumStyle::default()];
        while seen.len() < SpectrumStyle::ALL.len() {
            let next = seen.last().unwrap().next();
            assert!(!seen.contains(&next));
            seen.push(next);
        }
        assert_eq!(seen.last().unwrap().next(), SpectrumStyle::Bars);
    }
    #[test]
    fn defaults_roundtrip_and_corruption_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        assert_eq!(Settings::load(&path).unwrap().theme, Theme::CatppuccinMocha);
        assert!(!path.exists());
        Settings {
            theme: Theme::RosePine,
            ..Settings::default()
        }
        .save(&path)
        .unwrap();
        assert_eq!(Settings::load(&path).unwrap().theme, Theme::RosePine);
        fs::write(&path, b"broken").unwrap();
        assert!(Settings::load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        fs::write(&path, br#"{"theme":"unknown"}"#).unwrap();
        assert!(Settings::load(&path).is_err());
    }
    #[test]
    fn failed_save_preserves_destination_and_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        fs::create_dir(&path).unwrap();
        assert!(Settings::default().save(&path).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
