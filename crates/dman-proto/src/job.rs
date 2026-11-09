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
    /// Explicit seeding switch. `None` follows `[torrents] seed`, except that a
    /// positive `seed_ratio` / `seed_time` on the job implies seeding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<bool>,
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
    /// A video page or stream for the yt-dlp engine.
    Video {
        url: String,
        #[serde(default)]
        video: Box<crate::video::VideoOptions>,
    },
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
    /// `id` is the first created job; metalinks that describe several files
    /// create more, listed in `extra_ids` (in order).
    Added {
        id: JobId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra_ids: Vec<JobId>,
    },
    Failed {
        error: crate::RpcError,
    },
}

impl AddOutcome {
    /// Every job created by this outcome (empty for a failure).
    pub fn ids(&self) -> Vec<JobId> {
        match self {
            AddOutcome::Added { id, extra_ids } => std::iter::once(*id)
                .chain(extra_ids.iter().copied())
                .collect(),
            AddOutcome::Failed { .. } => Vec::new(),
        }
    }
}

/// One file of a job (`jobs.detail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// 1-based index as used by `select_files`.
    pub index: u32,
    pub path: String,
    /// Bytes; 0 when unknown.
    pub length: u64,
    pub completed: u64,
    pub selected: bool,
}

/// A connected peer of an active torrent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub ip: String,
    pub port: u16,
    /// Bytes per second from this peer.
    pub download_speed: u64,
    /// Bytes per second to this peer.
    pub upload_speed: u64,
    pub seeder: bool,
    /// Fraction `0.0..=1.0` of pieces the peer has (from its bitfield).
    pub progress: f64,
}

/// Tracker status. aria2 does not report it (decision D13), so it is always
/// `Unknown` for now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerStatus {
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerInfo {
    pub url: String,
    /// 0-based tier of the announce list.
    pub tier: u32,
    pub status: TrackerStatus,
}

/// Piece bitfield of a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PieceMap {
    pub num_pieces: u32,
    pub piece_length: u64,
    /// aria2's hex bitfield: most significant bit of the first byte = piece 0.
    pub bitfield_hex: String,
}

/// One point of a speed history. Bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedSample {
    /// Unix milliseconds.
    pub t: i64,
    pub down: u64,
    pub up: u64,
}

/// Result of `jobs.detail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobDetail {
    pub job: Job,
    #[serde(default)]
    pub files: Vec<FileInfo>,
    #[serde(default)]
    pub peers: Vec<PeerInfo>,
    #[serde(default)]
    pub trackers: Vec<TrackerInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pieces: Option<PieceMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// The options currently stored for the job.
    #[serde(default)]
    pub options: JobOptions,
    /// Oldest first; only kept while the job runs, not persisted.
    #[serde(default)]
    pub speed_history: Vec<SpeedSample>,
    /// Renames of the completed output on disk, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renames: Vec<RenameEntry>,
}

/// One recorded rename of a completed job's output (`jobs.detail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameEntry {
    /// Previous path (relative to the job directory).
    pub from: String,
    /// New path (relative to the job directory).
    pub to: String,
    /// Unix seconds.
    pub at: i64,
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
    /// Schedule summary; absent when no `[[schedule]]` is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleBrief>,
}

/// Short schedule state pushed with [`GlobalStats`] (see `schedule.get` for detail).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleBrief {
    /// Name of the window active now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Downloads are held back by a pause window.
    #[serde(default)]
    pub paused: bool,
    /// Unix seconds of the next change of the active window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_change: Option<i64>,
    /// Window after the next change (`None` = outside all windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_window: Option<String>,
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
    fn added_outcome_wire_format() {
        let one = AddOutcome::Added {
            id: JobId(3),
            extra_ids: vec![],
        };
        let v = serde_json::to_value(&one).unwrap();
        assert_eq!(v, serde_json::json!({"status":"added","id":3}));
        // Old payloads without extra_ids still parse.
        let back: AddOutcome = serde_json::from_value(v).unwrap();
        assert_eq!(back, one);
        let many = AddOutcome::Added {
            id: JobId(3),
            extra_ids: vec![JobId(4), JobId(5)],
        };
        let v = serde_json::to_value(&many).unwrap();
        assert_eq!(v["extra_ids"], serde_json::json!([4, 5]));
        assert_eq!(many.ids(), vec![JobId(3), JobId(4), JobId(5)]);
        assert_eq!(serde_json::from_value::<AddOutcome>(v).unwrap(), many);
    }

    #[test]
    fn detail_round_trip_and_defaults() {
        let d = JobDetail {
            job: job(),
            files: vec![FileInfo {
                index: 1,
                path: "/tmp/a".into(),
                length: 10,
                completed: 5,
                selected: true,
            }],
            peers: vec![PeerInfo {
                ip: "1.2.3.4".into(),
                port: 6881,
                download_speed: 1,
                upload_speed: 2,
                seeder: false,
                progress: 0.5,
            }],
            trackers: vec![TrackerInfo {
                url: "http://t/announce".into(),
                tier: 0,
                status: TrackerStatus::Unknown,
            }],
            pieces: Some(PieceMap {
                num_pieces: 8,
                piece_length: 1024,
                bitfield_hex: "f0".into(),
            }),
            info_hash: Some("ab".into()),
            comment: None,
            options: JobOptions::default(),
            speed_history: vec![SpeedSample {
                t: 1,
                down: 2,
                up: 3,
            }],
            renames: vec![RenameEntry {
                from: "a".into(),
                to: "b".into(),
                at: 5,
            }],
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["trackers"][0]["status"], "unknown");
        assert_eq!(v["pieces"]["bitfield_hex"], "f0");
        assert!(v.get("comment").is_none());
        assert_eq!(serde_json::from_value::<JobDetail>(v).unwrap(), d);
        // Only the job is required.
        let minimal = serde_json::json!({"job": serde_json::to_value(job()).unwrap()});
        let m: JobDetail = serde_json::from_value(minimal).unwrap();
        assert!(m.files.is_empty() && m.pieces.is_none() && m.speed_history.is_empty());
    }

    #[test]
    fn speed_sample_wire_format() {
        let s = SpeedSample {
            t: 1_700_000_000_000,
            down: 5,
            up: 6,
        };
        assert_eq!(
            serde_json::to_value(s).unwrap(),
            serde_json::json!({"t":1_700_000_000_000i64,"down":5,"up":6})
        );
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
