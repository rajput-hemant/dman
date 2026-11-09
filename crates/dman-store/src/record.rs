//! The persisted job record.

use dman_proto::{Engine, Job, JobId, JobOptions, JobState, SourceKind};
use serde::{Deserialize, Serialize};

/// What the user wants to happen with a job (as opposed to its observed state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    /// Should run whenever the queue allows.
    Queued,
    /// Should stay paused.
    Paused,
    /// Should not run (finished, failed or removed).
    Stopped,
}

/// Live figures reported by an engine for a running job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveFigures {
    /// Bytes per second.
    pub download_speed: u64,
    /// Bytes per second.
    pub upload_speed: u64,
    /// Number of connections / peers in use.
    pub connections: u32,
    /// Connected seeders (torrents only).
    pub seeders: Option<u32>,
}

/// dman's persistent record of one job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    /// Stable dman id.
    pub id: JobId,
    /// Engine that runs the job.
    pub engine: Engine,
    /// Kind of source.
    pub kind: SourceKind,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Target directory.
    #[serde(default)]
    pub dir: String,
    /// URI, magnet or file name the job was added from.
    #[serde(default)]
    pub source: String,
    /// Per-job options.
    #[serde(default)]
    pub options: JobOptions,
    /// What the user wants.
    pub desired: Desired,
    /// Last known state.
    pub state: JobState,
    /// Current aria2 gid (changes on re-add).
    #[serde(default)]
    pub gid: Option<String>,
    /// Torrent info hash, once known.
    #[serde(default)]
    pub info_hash: Option<String>,
    /// Total size in bytes (0 = unknown).
    #[serde(default)]
    pub total_bytes: u64,
    /// Bytes downloaded so far.
    #[serde(default)]
    pub completed_bytes: u64,
    /// Bytes uploaded in previous engine sessions.
    #[serde(default)]
    pub uploaded_base: u64,
    /// Bytes uploaded in the current engine session.
    #[serde(default)]
    pub uploaded_session: u64,
    /// Unix seconds when added.
    #[serde(default)]
    pub added_at: i64,
    /// Unix seconds when completed.
    #[serde(default)]
    pub completed_at: Option<i64>,
    /// Ordering key for the waiting queue (smaller = earlier).
    #[serde(default)]
    pub queue_key: u64,
    /// Last error message.
    #[serde(default)]
    pub error: Option<String>,
    /// Whether `.torrent` / `.metalink` bytes are stored in the blobs table.
    #[serde(default)]
    pub has_blob: bool,
    /// Unknown fields, kept for forward compatibility.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl JobRecord {
    /// Total uploaded bytes across all engine sessions.
    pub fn uploaded_total(&self) -> u64 {
        self.uploaded_base.saturating_add(self.uploaded_session)
    }

    /// Builds the client-facing [`Job`]. Speeds and connections are zero without
    /// `live` figures. `queue_position` is always `None` here: it depends on the other
    /// jobs, so the daemon fills it in for queued jobs.
    pub fn to_job(&self, live: Option<&LiveFigures>) -> Job {
        let live = live.copied().unwrap_or_default();
        Job {
            id: self.id,
            engine: self.engine,
            kind: self.kind,
            name: self.name.clone(),
            state: self.state,
            dir: self.dir.clone(),
            source: self.source.clone(),
            total_bytes: self.total_bytes,
            completed_bytes: self.completed_bytes,
            uploaded_bytes: self.uploaded_total(),
            download_speed: live.download_speed,
            upload_speed: live.upload_speed,
            connections: live.connections,
            seeders: live.seeders,
            info_hash: self.info_hash.clone(),
            added_at: self.added_at,
            completed_at: self.completed_at,
            queue_position: None,
            error: self.error.clone(),
        }
    }
}
