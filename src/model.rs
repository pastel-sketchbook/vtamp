use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 3;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::youtube::Source>,
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
    pub queue_revision: u64,
    pub play_next: Vec<String>,
    pub scheduled_stop: Option<ScheduledStop>,
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
            queue_revision: 0,
            play_next: vec![],
            scheduled_stop: None,
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
    ImportPreview {
        request: crate::imports::ImportRequest,
    },
    ImportStart {
        request: crate::imports::ImportRequest,
    },
    Imports,
    ImportStatus {
        id: String,
        offset: usize,
        limit: usize,
    },
    ImportCancel {
        id: String,
    },
    ImportRetry {
        id: String,
    },
    ImportLookup {
        video_ids: Vec<String>,
    },
    LibraryEdit {
        id: String,
        title: Option<String>,
        artist: Option<String>,
    },
    LibraryRetag {
        id: String,
    },
    ImportCapabilities,
    ImportAvailable,
    Status,
    Now,
    QueuePage {
        offset: usize,
        limit: usize,
    },
    QueueEdit {
        edit: QueueEdit,
        dry_run: bool,
        if_queue_revision: Option<u64>,
        request_id: Option<String>,
    },
    LibrarySearch {
        filter: SearchFilter,
        offset: usize,
        limit: usize,
    },
    LibraryTrack {
        id: String,
    },
    ScanStatus {
        id: String,
    },
    StopAfterCurrent,
    SleepSet {
        milliseconds: u64,
    },
    SleepStatus,
    SleepCancel,
    Watch,
    SpectrumWatch,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
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
    Imports(Vec<crate::imports::ImportJob>),
    ImportProgress(crate::imports::ImportJob),
    State(State),
    Progress { position_ms: u64, revision: u64 },
    LibraryChanged,
    ScanCompleted(ScanJob),
    Shutdown,
}

pub fn display_time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduledStop {
    AfterCurrent { queue_item_id: String },
    Deadline { deadline_ms: u64 },
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchFilter {
    pub query: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub exclude: Vec<String>,
    pub exact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueEdit {
    pub operations: Vec<QueueOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueOperation {
    Add {
        track_ids: Vec<String>,
        #[serde(default)]
        after_current: bool,
        #[serde(default)]
        index: Option<usize>,
    },
    Remove {
        queue_item_ids: Vec<String>,
    },
    Move {
        queue_item_id: String,
        index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub warning_count: usize,
    pub warnings: Vec<ScanWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanJob {
    pub job_id: String,
    pub status: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub summary: Option<ScanSummary>,
    pub error: Option<String>,
}

impl State {
    pub fn now(&self) -> serde_json::Value {
        let duration_ms = self.current().map_or(0, |item| item.track.duration_ms);
        serde_json::json!({
            "current": self.current(), "status": self.status,
            "position_ms": self.position_ms, "duration_ms": duration_ms,
            "remaining_ms": duration_ms.saturating_sub(self.position_ms),
            "volume": self.volume, "shuffle": self.shuffle, "repeat": self.repeat,
            "queue_length": self.queue.len(), "revision": self.revision,
            "queue_revision": self.queue_revision, "scheduled_stop": self.scheduled_stop,
            "last_error": self.last_error
        })
    }
    pub fn queue_changed_since(&self, old: &Self) -> bool {
        self.current_id != old.current_id
            || self.play_next != old.play_next
            || self
                .queue
                .iter()
                .map(|i| &i.id)
                .ne(old.queue.iter().map(|i| &i.id))
    }
}
