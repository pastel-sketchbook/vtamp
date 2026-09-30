use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: u32,
    pub duration_ms: u64,
    pub cover: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueItem {
    pub id: String,
    pub track: Track,
}

impl QueueItem {
    pub fn new(track: Track) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            track,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    #[default]
    Off,
    One,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub queue: Vec<QueueItem>,
    pub current_id: Option<String>,
    pub status: PlaybackStatus,
    pub position_ms: u64,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub revision: u64,
    pub scanning: bool,
    pub last_error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            queue: vec![],
            current_id: None,
            status: PlaybackStatus::Stopped,
            position_ms: 0,
            volume: 70,
            shuffle: false,
            repeat: Repeat::Off,
            revision: 0,
            scanning: false,
            last_error: None,
        }
    }
}

impl State {
    pub fn current_index(&self) -> Option<usize> {
        self.queue
            .iter()
            .position(|q| Some(&q.id) == self.current_id.as_ref())
    }
    pub fn current(&self) -> Option<&QueueItem> {
        self.current_index().map(|i| &self.queue[i])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    Status,
    Watch,
    Shutdown,
    Play {
        paths: Vec<PathBuf>,
        track: Option<String>,
        queue_item: Option<String>,
    },
    Pause,
    Resume,
    Toggle,
    Stop,
    Next,
    Prev,
    Seek {
        milliseconds: i64,
        relative: bool,
    },
    Volume {
        value: Option<u8>,
    },
    Shuffle {
        enabled: bool,
    },
    Repeat {
        mode: Repeat,
    },
    QueueAdd {
        paths: Vec<PathBuf>,
        track: Option<String>,
    },
    QueueRemove {
        id: String,
    },
    QueueMove {
        id: String,
        index: usize,
    },
    QueueClear,
    LibraryAdd {
        path: PathBuf,
    },
    LibraryRemove {
        path: PathBuf,
    },
    LibraryScan,
    LibraryList {
        query: String,
        offset: usize,
        limit: usize,
    },
    LibraryRoots,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub request: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}
impl Reply {
    pub fn success(data: impl Serialize) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: true,
            data: Some(serde_json::to_value(data).expect("serializable response")),
            error: None,
        }
    }
    pub fn failure(error: ApiError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: false,
            data: None,
            error: Some(error),
        }
    }
    pub fn into_data(self) -> Result<serde_json::Value, ApiError> {
        if self.ok {
            Ok(self.data.unwrap_or(serde_json::Value::Null))
        } else {
            Err(self
                .error
                .unwrap_or_else(|| ApiError::new("protocol_error", "Missing error details")))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    State(State),
    Progress { position_ms: u64, revision: u64 },
    LibraryChanged,
    Shutdown,
}

pub fn display_time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}
