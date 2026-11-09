//! Method names, their parameter/result types, and pushed events.
//!
//! Naming: `<area>.<verb>`. Every method takes a single JSON object (or no params).

use serde::{Deserialize, Serialize};

use crate::{GlobalStats, Job, JobId, JobOptions, JobState, Limits};

pub const HELLO: &str = "session.hello";
pub const SUBSCRIBE: &str = "session.subscribe";
pub const JOBS_ADD: &str = "jobs.add";
pub const JOBS_LIST: &str = "jobs.list";
pub const JOBS_GET: &str = "jobs.get";
pub const JOBS_DETAIL: &str = "jobs.detail";
pub const JOBS_PAUSE: &str = "jobs.pause";
pub const JOBS_RESUME: &str = "jobs.resume";
pub const JOBS_REMOVE: &str = "jobs.remove";
pub const JOBS_PURGE: &str = "jobs.purge";
pub const JOBS_REORDER: &str = "jobs.reorder";
pub const JOBS_SET_OPTIONS: &str = "jobs.set_options";
pub const JOBS_RENAME: &str = "jobs.rename";
pub const JOBS_PREVIEW_MAGNET: &str = "jobs.preview_magnet";
pub const QUEUE_PAUSE_ALL: &str = "queue.pause_all";
pub const QUEUE_RESUME_ALL: &str = "queue.resume_all";
pub const LIMITS_GET: &str = "limits.get";
pub const LIMITS_SET: &str = "limits.set";
pub const SCHEDULE_GET: &str = "schedule.get";
pub const STATS_GET: &str = "stats.get";
pub const STATS_HISTORY: &str = "stats.history";
pub const FS_LIST: &str = "fs.list";
pub const ENGINE_STATUS: &str = "engine.status";
pub const ENGINE_RESTART: &str = "engine.restart";
pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";

/// Name of the notification carrying [`Event`]s.
pub const EVENT: &str = "event";

/// First message on every connection. The WebSocket transport rejects any other
/// method until `session.hello` succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloParams {
    pub token: String,
    pub api_version: u32,
    /// Free text such as `dman-tui/0.1.0`, for logs.
    pub client: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    pub api_version: u32,
    pub daemon_version: String,
    pub profile: String,
}

/// `session.subscribe`: start receiving [`Event`]s, beginning with a snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubscribeParams {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListParams {
    /// Only jobs in these states; empty = all.
    pub states: Vec<JobState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdParams {
    pub id: JobId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdsParams {
    pub ids: Vec<JobId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveParams {
    pub ids: Vec<JobId>,
    /// Also delete downloaded files. Clients must confirm twice before sending this.
    #[serde(default)]
    pub delete_files: bool,
    /// Remove even if the engine does not answer a graceful stop.
    #[serde(default)]
    pub force: bool,
}

/// How to move a job within the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", content = "value", rename_all = "snake_case")]
pub enum Reorder {
    /// Absolute position (0 = first).
    To(u32),
    /// Relative move; negative = towards the front.
    By(i32),
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReorderParams {
    pub id: JobId,
    pub to: Reorder,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetOptionsParams {
    pub id: JobId,
    pub options: JobOptions,
}

/// `jobs.rename`. Waiting/paused jobs get a new output name (applied by the
/// engine when the download starts); completed jobs are moved on disk and the
/// rename is recorded. Active transfers are refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameParams {
    pub id: JobId,
    /// New file name (or, for completed multi-file torrents without
    /// `file_index`, new top-level directory name). A plain name: no `/`, no `..`.
    pub name: String,
    /// 1-based file to rename in a multi-file job; required while the job has
    /// not completed and has several files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<u32>,
}

/// `jobs.preview_magnet`: fetch a magnet's metadata (file list) without
/// starting a download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewMagnetParams {
    pub uri: String,
    /// Reserved; the metadata is always fetched into a dman-private directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Give up after this many seconds (default 120, at most 600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
}

/// Result of `jobs.preview_magnet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagnetPreview {
    /// Lower-case hex info hash.
    pub info_hash: String,
    pub name: String,
    pub total_bytes: u64,
    /// Files in torrent order; `index` is 1-based, all `selected`.
    pub files: Vec<crate::FileInfo>,
    /// The `.torrent` file, base64. Pass it to `jobs.add` as
    /// `AddSource::Torrent` together with `select_files`.
    pub torrent_base64: String,
}

/// Per-item outcome of batch operations such as pause/resume/remove.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResult {
    pub ok: Vec<JobId>,
    pub failed: Vec<BatchFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchFailure {
    pub id: JobId,
    pub error: crate::RpcError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct LimitsUpdate {
    pub download: Option<u64>,
    pub upload: Option<u64>,
    pub max_concurrent: Option<u32>,
}

/// One configured schedule window, as returned by `schedule.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub name: String,
    /// Weekdays the window starts on, expanded and in week order
    /// (`"mon"`..`"sun"`).
    pub days: Vec<String>,
    /// `HH:MM` local time, inclusive.
    pub start: String,
    /// `HH:MM` local time, exclusive; earlier than `start` = crosses midnight.
    pub end: String,
    /// `"run"` or `"pause"`.
    pub action: String,
    /// Download cap (bytes/s, 0 = unlimited) applied inside the window; `None` = base limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
}

/// Result of `schedule.get`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleStatus {
    /// Configured windows in priority order (first match wins).
    pub windows: Vec<WindowInfo>,
    /// Name of the window active now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Unix seconds of the next change of the active window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_change: Option<i64>,
    /// Name of the window after the next change (`None` = outside all windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_window: Option<String>,
    /// Local time zone used for the windows (IANA name, or `UTC` on fallback).
    #[serde(default)]
    pub time_zone: String,
    /// Downloads are currently held back by a pause window.
    #[serde(default)]
    pub paused: bool,
    /// A runtime `limits.set` override is in effect until the next transition.
    #[serde(default)]
    pub override_active: bool,
}

/// `fs.list`: list a directory of the daemon host (for directory pickers).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsListParams {
    /// Directory to list; `None` = the profile's download directory (or its
    /// nearest existing ancestor). A leading `~` is expanded.
    pub path: Option<String>,
    /// Only return directories.
    pub dirs_only: bool,
}

/// One entry of an [`FsListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    /// Bytes of a file; `None` for directories or when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Result of `fs.list`: directories first, then files, each sorted by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListing {
    /// Canonical path of the listed directory.
    pub path: String,
    /// Canonical parent; `None` at the filesystem root.
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
    /// More entries existed than the daemon returns (cap 2000).
    #[serde(default)]
    pub truncated: bool,
}

/// State of an engine process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStatus {
    pub engine: crate::Engine,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Engine features (e.g. aria2 `enabledFeatures`).
    #[serde(default)]
    pub features: Vec<String>,
    /// Why it is unavailable, shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Severity of a status-line message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Pushed to subscribers as the params of the [`EVENT`] notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Complete state; always the first event after subscribing.
    Snapshot {
        jobs: Vec<Job>,
        stats: GlobalStats,
        limits: Limits,
    },
    /// Jobs added or changed since the last tick (full records).
    JobsUpdated {
        jobs: Vec<Job>,
    },
    /// Jobs gone from the live list (purged or removed).
    JobsRemoved {
        ids: Vec<JobId>,
    },
    Stats {
        stats: GlobalStats,
    },
    Limits {
        limits: Limits,
    },
    /// Human-readable result of an action or a background failure.
    Message {
        level: Level,
        text: String,
    },
    Engine {
        status: EngineStatus,
    },
    /// A job reached a notable end state; emitted once per transition.
    JobFinished {
        id: JobId,
        name: String,
        /// `dir/name`.
        path: String,
        outcome: Outcome,
    },
    /// A search made progress. Sent per provider status change, at most every
    /// 250 ms; `results` is the cumulative merged list (ranked, at most
    /// [`crate::search::MAX_RESULTS`]), so a client simply replaces its list.
    SearchUpdate {
        search_id: u64,
        provider: String,
        status: crate::search::ProviderStatus,
        results: Vec<crate::search::SearchResultDto>,
    },
    /// Every provider of the search finished (or it was cancelled); always the
    /// last event of a search.
    SearchFinished {
        search_id: u64,
    },
}

/// What happened to a job in [`Event::JobFinished`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// The download finished (seeding may follow).
    Completed,
    Failed {
        error: String,
    },
    /// Seeding ended.
    SeedCompleted,
}

impl Outcome {
    /// Name used in hooks, webhooks and the `[notify] on` filter.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed { .. } => "failed",
            Self::SeedCompleted => "seed_completed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_wire_format() {
        let v = serde_json::to_value(Reorder::By(-2)).unwrap();
        assert_eq!(v, serde_json::json!({"how":"by","value":-2}));
        let v = serde_json::to_value(Reorder::Top).unwrap();
        assert_eq!(v, serde_json::json!({"how":"top"}));
    }

    #[test]
    fn rename_and_preview_wire_format() {
        let r: RenameParams = serde_json::from_str(r#"{"id":3,"name":"x"}"#).unwrap();
        assert_eq!(r.file_index, None);
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("file_index").is_none());
        let p: PreviewMagnetParams = serde_json::from_str(r#"{"uri":"magnet:?xt=a"}"#).unwrap();
        assert_eq!((p.dir, p.timeout_secs), (None, None));
    }

    #[test]
    fn job_finished_wire_format() {
        let e = Event::JobFinished {
            id: JobId(1),
            name: "a".into(),
            path: "/d/a".into(),
            outcome: Outcome::Failed { error: "x".into() },
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "job_finished");
        assert_eq!(v["outcome"]["kind"], "failed");
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), e);
        assert_eq!(Outcome::SeedCompleted.name(), "seed_completed");
    }

    #[test]
    fn event_is_tagged() {
        let e = Event::JobsRemoved {
            ids: vec![JobId(4)],
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "jobs_removed");
    }
}
