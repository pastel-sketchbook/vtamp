use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::Serialize;
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub runtime: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        if let Some(home) = std::env::var_os("VTAMP_HOME") {
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                bail!("VTAMP_HOME must be an absolute path");
            }
            return Ok(Self {
                runtime: home.join("run"),
                cache: home.join("covers"),
                data: home,
            });
        }
        let dirs =
            ProjectDirs::from("", "", "vtamp").context("Cannot locate your home directory")?;
        // Short enough for macOS's 104-byte Unix socket path limit.
        let runtime = PathBuf::from(format!("/tmp/vtamp-{}", unsafe { libc::geteuid() }));
        Ok(Self {
            data: dirs.data_local_dir().into(),
            runtime,
            cache: dirs.cache_dir().join("covers"),
        })
    }
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("control.sock")
    }
    pub fn database(&self) -> PathBuf {
        self.data.join("state.db")
    }
    pub fn ui_settings(&self) -> PathBuf {
        self.data.join("ui.json")
    }
    pub fn themes(&self) -> PathBuf {
        self.data.join("themes")
    }
    pub fn log(&self) -> PathBuf {
        self.data.join("server.log")
    }
    pub fn prepare(&self) -> Result<()> {
        for dir in [&self.data, &self.runtime, &self.cache] {
            private_dir(dir)?;
        }
        if self.socket().as_os_str().as_encoded_bytes().len() >= 104 {
            bail!("Socket path is too long; use a shorter VTAMP_HOME");
        }
        Ok(())
    }
}

pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "Refusing directory not owned by this user: {}",
            dir.display()
        );
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if let Ok(rest) = path.strip_prefix("~") {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(rest)
    } else {
        path.to_path_buf()
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_file_name(format!(".vtamp-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut f, value)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
