//! Jobs (downloads), their options, and global statistics.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable dman identifier of a job. Unlike aria2 GIDs it never changes, also not
/// when a magnet turns into a torrent or a job is re-added after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for JobId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(JobId)
    }
}

/// Which engine runs a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Aria2,
    YtDlp,
}

/// What kind of source a job was created from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Magnet,
    Torrent,
    Http,
    Ftp,
    Sftp,
    Metalink,
    Video,
}

/// Lifecycle state of a job as seen by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// In dman's queue, waiting for a slot, schedule window or disk space.
    Waiting,
    /// Transferring data.
    Active,
    /// Torrent download finished, still uploading.
    Seeding,
    /// Paused by the user.
    Paused,
    /// Finished successfully (and done seeding).
    Complete,
    /// Failed; see [`Job::error`].
    Error,
    /// Removed by the user; kept only in history.
    Removed,
}

impl JobState {
    /// States in which a transfer is not running and will not start by itself.
    pub const fn is_stopped(self) -> bool {
        matches!(
            self,
            JobState::Complete | JobState::Error | JobState::Removed
        )
    }

    /// States in which the job may be renamed by changing its output name.
    pub const fn is_renamable_in_place(self) -> bool {
        matches!(self, JobState::Waiting | JobState::Paused)
    }
}

/// A download as presented to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub engine: Engine,
    pub kind: SourceKind,
    pub name: String,
    pub state: JobState,
    /// Target directory (on the daemon's filesystem).
    pub dir: String,
    /// The URI, magnet or file name the job was added from.
    pub source: String,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    /// Total uploaded bytes, persisted by dman across restarts.
    pub uploaded_bytes: u64,
    /// Bytes per second.
    pub download_speed: u64,
    pub upload_speed: u64,
    pub connections: u32,
    /// Torrents only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    /// Unix seconds.
    pub added_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    /// Position in dman's queue (0 = next to start); `None` when not queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Job {
    /// Progress in the range `0.0..=1.0`; `0.0` while the size is unknown.
    pub fn progress(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.completed_bytes as f64 / self.total_bytes as f64
        }
    }

    /// Upload / download ratio (torrents); `0.0` while nothing is downloaded.
    pub fn ratio(&self) -> f64 {
        if self.completed_bytes == 0 {
            0.0
        } else {
            self.uploaded_bytes as f64 / self.completed_bytes as f64
        }
    }

    /// Seconds until completion at the current speed, if computable.
    pub fn eta_secs(&self) -> Option<u64> {
        let remaining = self.total_bytes.checked_sub(self.completed_bytes)?;
        if self.download_speed == 0 || self.total_bytes == 0 {
            return None;
        }
        Some(remaining.div_ceil(self.download_speed))
    }
}

/// Per-job options. Every field is optional; unset fields inherit the defaults of
/// the profile. `engine_options` passes engine-specific options through verbatim
/// (validated by the engine adapter) so every engine feature stays reachable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Output file name (single-file downloads).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// Bytes per second; `0` = unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_download_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_upload_limit: Option<u64>,
    /// 1-based file indexes to download (torrents / metalinks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_files: Option<Vec<u32>>,
    /// Per-file output names keyed by 1-based index.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub file_names: BTreeMap<u32, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_ratio: Option<f64>,
    /// Minutes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequential: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// `<algo>=<hex digest>`, e.g. `sha-256=...`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connections_per_server: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<u32>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub engine_options: BTreeMap<String, String>,
}

/// One thing to add.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AddSource {
    /// One download from one or more mirror URIs (HTTP/FTP/SFTP/magnet).
    Uris { uris: Vec<String> },
    /// Contents of a `.torrent` file, base64-encoded.
    Torrent { name: String, data_base64: String },
    /// Contents of a `.metalink`/`.meta4` file, base64-encoded.
    Metalink { name: String, data_base64: String },
}

/// Parameters of `jobs.add`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddParams {
    pub sources: Vec<AddSource>,
    #[serde(default)]
    pub options: JobOptions,
    /// Add without starting; the job stays paused until resumed.
    #[serde(default)]
    pub paused: bool,
}

/// Result of `jobs.add`: one entry per source, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AddOutcome {
    Added { id: JobId },
    Failed { error: crate::RpcError },
}

/// Aggregate figures shown in status lines and `dman stats`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalStats {
    pub download_speed: u64,
    pub upload_speed: u64,
    pub num_active: u32,
    pub num_waiting: u32,
    pub num_stopped: u32,
    /// Free bytes on the default download volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free: Option<u64>,
    /// Cumulative bytes over the lifetime of this profile.
    pub total_downloaded: u64,
    pub total_uploaded: u64,
}

/// Global limits (bytes per second; 0 = unlimited).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub download: u64,
    pub upload: u64,
    pub max_concurrent: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job {
            id: JobId(1),
            engine: Engine::Aria2,
            kind: SourceKind::Http,
            name: "a".into(),
            state: JobState::Active,
            dir: "/tmp".into(),
            source: "http://x/a".into(),
            total_bytes: 1000,
            completed_bytes: 250,
            uploaded_bytes: 500,
            download_speed: 100,
            upload_speed: 0,
            connections: 1,
            seeders: None,
            info_hash: None,
            added_at: 0,
            completed_at: None,
            queue_position: None,
            error: None,
        }
    }

    #[test]
    fn derived_figures() {
        let j = job();
        assert_eq!(j.progress(), 0.25);
        assert_eq!(j.ratio(), 2.0);
        assert_eq!(j.eta_secs(), Some(8));
    }

    #[test]
    fn eta_unknown_without_speed_or_size() {
        let mut j = job();
        j.download_speed = 0;
        assert_eq!(j.eta_secs(), None);
        j.download_speed = 5;
        j.total_bytes = 0;
        assert_eq!(j.eta_secs(), None);
    }

    #[test]
    fn options_skip_empty_fields() {
        let json = serde_json::to_string(&JobOptions::default()).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn add_source_is_tagged() {
        let s = AddSource::Uris {
            uris: vec!["magnet:?xt=urn:btih:abc".into()],
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["type"], "uris");
    }
}
