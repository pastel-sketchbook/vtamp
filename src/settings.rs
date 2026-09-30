//! Client-only preferences; never touches the playback server or its database.
use crate::{platform, theme::Theme};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: Theme,
}

impl Settings {
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
    fn defaults_roundtrip_and_corruption_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        assert_eq!(Settings::load(&path).unwrap().theme, Theme::CatppuccinMocha);
        assert!(!path.exists());
        Settings {
            theme: Theme::RosePine,
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
