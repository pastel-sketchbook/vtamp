//! Client-owned extensions. The plugin protocol is independent of the server wire protocol.
mod runtime;
pub use runtime::{Session, Update};

use crate::{
    model::{PlaybackStatus, State, Track},
    platform::{self, Paths},
};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub const API_VERSION: u32 = 1;
pub const MAX_MESSAGE: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Playing,
    Selected,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub id: String,
    pub title: String,
    pub target: Target,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub exec: Vec<String>,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default)]
    pub plugins: BTreeMap<String, PathBuf>,
    /// Single printable keys, outside any prompt or modal. Built-ins are reserved.
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

#[derive(Default)]
pub struct Catalog {
    pub plugins: Vec<Plugin>,
    pub bindings: BTreeMap<char, String>,
    pub warnings: Vec<String>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn text(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

pub fn read_manifest(path: &Path) -> Result<Plugin> {
    let path = fs::canonicalize(path).context("Cannot locate plugin manifest")?;
    let bytes = read_bounded(&path, 64 * 1024)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).context("Invalid plugin manifest")?;
    if manifest.api_version != API_VERSION {
        bail!("Unsupported plugin API version {}", manifest.api_version);
    }
    if !identifier(&manifest.id) || !text(&manifest.name) {
        bail!("Invalid plugin ID or name");
    }
    if manifest.exec.is_empty()
        || manifest.exec[0].is_empty()
        || manifest.exec.iter().any(|s| s.contains('\0'))
    {
        bail!("Plugin exec must contain a program and optional arguments");
    }
    let mut ids = HashSet::new();
    if manifest.commands.is_empty() || manifest.commands.len() > 64 {
        bail!("A plugin needs 1–64 commands");
    }
    for command in &manifest.commands {
        if !identifier(&command.id) || !text(&command.title) || !ids.insert(&command.id) {
            bail!("Invalid or duplicate plugin command");
        }
    }
    Ok(Plugin { manifest, path })
}

pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("{} exceeds {} bytes", path.display(), limit);
    }
    Ok(bytes)
}

impl Registry {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.data.join("plugins.json");
        match read_bounded(&path, MAX_MESSAGE) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("Invalid plugins.json")?),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(Self::default())
            }
            Err(error) => Err(error),
        }
    }
    fn save(&self, paths: &Paths) -> Result<()> {
        platform::private_dir(&paths.data)?;
        platform::atomic_json(&paths.data.join("plugins.json"), self)
    }
    pub fn add(paths: &Paths, path: &Path) -> Result<String> {
        let plugin = read_manifest(path)?;
        let mut registry = Self::load(paths)?;
        if let Some(old) = registry.plugins.get(&plugin.manifest.id)
            && old != &plugin.path
        {
            bail!("Plugin ID already registered; remove it before replacing it");
        }
        let id = plugin.manifest.id;
        registry.plugins.insert(id.clone(), plugin.path);
        registry.save(paths)?;
        Ok(id)
    }
    pub fn remove(paths: &Paths, id: &str) -> Result<()> {
        let mut registry = Self::load(paths)?;
        if registry.plugins.remove(id).is_none() {
            bail!("Unknown plugin: {id}");
        }
        registry
            .bindings
            .retain(|_, command| !command.starts_with(&format!("{id}:")));
        registry.save(paths)
    }
}

// Includes optional commands and multi-key prefixes even when currently unavailable.
pub fn available_key(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let c = chars.next()?;
    (chars.next().is_none()
        && c.is_ascii_graphic()
        && !"q?tvVwgGzainbrsmoORFAexdjkJKX/[]:+=> <~-".contains(c))
    .then_some(c)
}

impl Catalog {
    pub fn load(paths: &Paths) -> Self {
        let mut catalog = Self::default();
        let registry = match Registry::load(paths) {
            Ok(registry) => registry,
            Err(error) => {
                catalog.warnings.push(format!("{error:#}"));
                return catalog;
            }
        };
        for (id, path) in registry.plugins {
            match read_manifest(&path) {
                Ok(plugin) if plugin.manifest.id == id => catalog.plugins.push(plugin),
                Ok(_) => catalog
                    .warnings
                    .push(format!("{}: registered ID changed", path.display())),
                Err(error) => catalog
                    .warnings
                    .push(format!("{}: {error:#}", path.display())),
            }
        }
        for (key, command) in registry.bindings {
            match available_key(&key) {
                Some(c) if catalog.resolve(&command).is_ok() => {
                    catalog.bindings.insert(c, command);
                }
                _ => catalog
                    .warnings
                    .push(format!("Invalid or conflicting binding: {key} → {command}")),
            }
        }
        catalog
    }
    pub fn resolve(&self, name: &str) -> Result<(Plugin, Command)> {
        let (id, command) = name.split_once(':').context("Use PLUGIN:COMMAND")?;
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .context("Plugin is unavailable")?;
        let command = plugin
            .manifest
            .commands
            .iter()
            .find(|c| c.id == command)
            .context("Unknown plugin command")?;
        Ok((plugin.clone(), command.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Context {
    pub generation: u64,
    pub connected: bool,
    pub track: Option<Track>,
    pub playback_id: Option<String>,
    pub status: PlaybackStatus,
    pub position_ms: u64,
}

impl Context {
    pub fn from_state(
        state: &State,
        connected: bool,
        target: Target,
        selected: Option<&Track>,
    ) -> Self {
        let current = state.current();
        let track = match target {
            Target::Playing => current.map(|item| item.track.clone()),
            Target::Selected => selected.cloned(),
            Target::None => None,
        };
        let playing = track
            .as_ref()
            .is_some_and(|track| current.is_some_and(|item| item.track.id == track.id));
        Self {
            generation: 1,
            connected,
            track,
            playback_id: playing.then(|| current.unwrap().id.clone()),
            status: if playing {
                state.status
            } else {
                PlaybackStatus::Stopped
            },
            position_ms: if playing { state.position_ms } else { 0 },
        }
    }
    pub fn advance(&mut self, mut next: Self) -> bool {
        next.generation = self.generation
            + u64::from(self.track != next.track || self.playback_id != next.playback_id);
        if *self == next {
            return false;
        }
        *self = next;
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    pub items: Vec<Item>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

impl View {
    pub fn validate(&self) -> Result<()> {
        if !text(&self.title) || self.items.len() > 10_000 || self.actions.len() > 64 {
            bail!("Invalid plugin view");
        }
        let clean = |s: &str| !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t');
        if !clean(&self.subtitle) {
            bail!("Control characters in plugin view");
        }
        let mut actions = HashSet::new();
        for action in &self.actions {
            if !identifier(&action.id) || !text(&action.title) || !actions.insert(&action.id) {
                bail!("Invalid view action");
            }
        }
        for item in &self.items {
            if !clean(&item.text)
                || item.action.as_ref().is_some_and(|id| !identifier(id))
                || item
                    .end_ms
                    .is_some_and(|end| item.start_ms.is_none_or(|start| end <= start))
            {
                bail!("Invalid plugin item");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Init {
        api_version: u32,
        data_dir: PathBuf,
        cache_dir: PathBuf,
        vtamp: PathBuf,
    },
    Invoke {
        command: String,
        context: Context,
    },
    Context {
        context: Context,
    },
    Action {
        id: String,
        generation: u64,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    Ready { api_version: u32 },
    View { generation: u64, view: View },
    Notice { generation: u64, message: String },
    Done { generation: u64 },
}
