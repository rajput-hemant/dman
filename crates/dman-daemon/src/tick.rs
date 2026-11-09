//! Pure helpers for the poll tick: aria2 status mapping, transfer deltas and
//! upload bookkeeping. Nothing here does I/O.

use dman_aria2::{DownloadStatus, Status};
use dman_proto::{JobState, SourceKind};
use dman_store::{Desired, JobRecord};

/// Keys fetched for every download on every tick (kept small on purpose).
pub const STATUS_KEYS: &[&str] = &[
    "gid",
    "status",
    "totalLength",
    "completedLength",
    "uploadLength",
    "downloadSpeed",
    "uploadSpeed",
    "connections",
    "numSeeders",
    "seeder",
    "errorMessage",
    "errorCode",
    "followedBy",
    "following",
    "infoHash",
];

/// Keys fetched only while a job's name or file list is not yet known.
pub const DETAIL_KEYS: &[&str] = &["gid", "files", "bittorrent", "totalLength"];

/// Maps an aria2 status to the user-visible state. `None` for statuses this
/// version does not understand (the record keeps its previous state).
pub fn map_state(status: DownloadStatus, seeder: bool, desired: Desired) -> Option<JobState> {
    Some(match status {
        DownloadStatus::Active if seeder => JobState::Seeding,
        DownloadStatus::Active => JobState::Active,
        DownloadStatus::Waiting | DownloadStatus::Paused => {
            if desired == Desired::Queued {
                JobState::Waiting
            } else {
                JobState::Paused
            }
        }
        DownloadStatus::Complete => JobState::Complete,
        DownloadStatus::Error => JobState::Error,
        DownloadStatus::Removed => JobState::Removed,
        DownloadStatus::Unknown => return None,
    })
}

/// Last observed engine counters of a job, used to compute deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counters {
    /// Gid the counters belong to.
    pub gid: String,
    /// `completedLength` at the last observation.
    pub completed: u64,
    /// `uploadLength` at the last observation.
    pub uploaded: u64,
}

impl Counters {
    /// Counters of a download that was just added by dman (nothing transferred yet).
    pub fn fresh(gid: &str) -> Self {
        Self {
            gid: gid.to_owned(),
            completed: 0,
            uploaded: 0,
        }
    }
}

/// Bytes transferred since the previous observation `prev`, and the new
/// baseline. The first observation of a gid (no baseline, or a different gid,
/// e.g. after a re-add where aria2 resumes from an existing partial file) and
/// counters that went backwards (engine restart, restarted download) only set
/// a new baseline and count nothing, so bytes are never counted twice.
pub fn delta(prev: Option<&Counters>, gid: &str, completed: u64, uploaded: u64) -> (u64, u64) {
    match prev {
        Some(p) if p.gid == gid => (
            completed.saturating_sub(p.completed),
            uploaded.saturating_sub(p.uploaded),
        ),
        _ => (0, 0),
    }
}

/// Updates the D12 upload bookkeeping of `rec` for a new `uploadLength`
/// reading of its current gid. When the engine counter went backwards (new
/// engine session for the same gid) the previous session is folded into the
/// base first.
pub fn apply_upload(rec: &mut JobRecord, upload_length: u64) {
    if upload_length < rec.uploaded_session {
        rec.uploaded_base = rec.uploaded_base.saturating_add(rec.uploaded_session);
    }
    rec.uploaded_session = upload_length;
}

/// Folds the current session's upload counter into the base; called whenever
/// a job gets a new gid (re-add, magnet follow-up).
pub fn fold_upload(rec: &mut JobRecord) {
    rec.uploaded_base = rec.uploaded_base.saturating_add(rec.uploaded_session);
    rec.uploaded_session = 0;
}

/// Display name derived from aria2 details: the torrent name, else the first
/// file name. `None` while aria2 does not know it yet.
pub fn name_from_details(st: &Status) -> Option<String> {
    if let Some(name) = st
        .bittorrent
        .as_ref()
        .and_then(|b| b.info.as_ref())
        .and_then(|i| i.name.as_deref())
        .filter(|n| !n.is_empty())
    {
        return Some(name.to_owned());
    }
    let path = st.files.as_ref()?.first()?.path.as_deref()?;
    if path.is_empty() || path.starts_with("[METADATA]") {
        return None;
    }
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Whether the details in `st` are final enough to stop refreshing the name:
/// torrents once the info dictionary is known, other downloads once aria2
/// knows the size (response headers seen).
pub fn details_final(kind: SourceKind, st: &Status) -> bool {
    match kind {
        SourceKind::Magnet | SourceKind::Torrent => st
            .bittorrent
            .as_ref()
            .and_then(|b| b.info.as_ref())
            .is_some_and(|i| i.name.is_some()),
        _ => st.total_length.unwrap_or(0) > 0,
    }
}

/// Source kind of a URI, by scheme.
pub fn kind_of_uri(uri: &str) -> Option<SourceKind> {
    let lower = uri.trim().to_ascii_lowercase();
    let scheme = lower.split_once(':')?.0;
    Some(match scheme {
        "magnet" => SourceKind::Magnet,
        "http" | "https" => SourceKind::Http,
        "ftp" => SourceKind::Ftp,
        "sftp" => SourceKind::Sftp,
        _ => return None,
    })
}

/// Initial display name for a URI source: the magnet `dn`, or the last path
/// segment of the URL; falls back to the URI itself.
pub fn name_of_uri(uri: &str) -> String {
    let Ok(url) = url::Url::parse(uri) else {
        return uri.to_owned();
    };
    if url.scheme() == "magnet" {
        if let Some((_, dn)) = url.query_pairs().find(|(k, _)| k == "dn") {
            return dn.into_owned();
        }
        if let Some((_, xt)) = url.query_pairs().find(|(k, _)| k == "xt") {
            let hash = xt.rsplit(':').next().unwrap_or_default().to_owned();
            return format!("magnet {}", hash.chars().take(12).collect::<String>());
        }
        return uri.to_owned();
    }
    url.path_segments()
        .and_then(|mut s| s.next_back().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .map(|s| {
            url::form_urlencoded::parse(format!("x={s}").as_bytes())
                .next()
                .map_or(s.clone(), |(_, v)| v.into_owned())
        })
        .or_else(|| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| uri.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_aria2::{BitTorrent, BtInfo, File};
    use dman_proto::{Engine, JobId, JobOptions};

    #[test]
    fn status_mapping() {
        use DownloadStatus as S;
        assert_eq!(
            map_state(S::Active, true, Desired::Queued),
            Some(JobState::Seeding)
        );
        assert_eq!(
            map_state(S::Active, false, Desired::Queued),
            Some(JobState::Active)
        );
        assert_eq!(
            map_state(S::Paused, false, Desired::Queued),
            Some(JobState::Waiting)
        );
        assert_eq!(
            map_state(S::Waiting, false, Desired::Queued),
            Some(JobState::Waiting)
        );
        assert_eq!(
            map_state(S::Paused, false, Desired::Paused),
            Some(JobState::Paused)
        );
        assert_eq!(
            map_state(S::Complete, false, Desired::Queued),
            Some(JobState::Complete)
        );
        assert_eq!(
            map_state(S::Error, false, Desired::Queued),
            Some(JobState::Error)
        );
        assert_eq!(
            map_state(S::Removed, false, Desired::Stopped),
            Some(JobState::Removed)
        );
        assert_eq!(map_state(S::Unknown, false, Desired::Queued), None);
    }

    #[test]
    fn deltas_and_counter_reset() {
        let p = Counters {
            gid: "a".into(),
            completed: 100,
            uploaded: 10,
        };
        assert_eq!(delta(Some(&p), "a", 150, 15), (50, 5));
        // Reset on the same gid: no negative or double counting.
        assert_eq!(delta(Some(&p), "a", 20, 0), (0, 0));
        // New gid (re-add resumes from partial data): baseline only.
        assert_eq!(delta(Some(&p), "b", 500, 0), (0, 0));
        // First observation after a daemon start.
        assert_eq!(delta(None, "a", 500, 7), (0, 0));
        // A freshly added job counts from zero.
        assert_eq!(delta(Some(&Counters::fresh("c")), "c", 42, 1), (42, 1));
    }

    fn record() -> JobRecord {
        JobRecord {
            id: JobId(1),
            engine: Engine::Aria2,
            kind: SourceKind::Torrent,
            name: String::new(),
            dir: "/tmp".into(),
            source: "x".into(),
            options: JobOptions::default(),
            desired: Desired::Queued,
            state: JobState::Seeding,
            gid: None,
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: None,
            queue_key: 0,
            error: None,
            has_blob: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn upload_bookkeeping() {
        let mut r = record();
        apply_upload(&mut r, 100);
        assert_eq!(r.uploaded_total(), 100);
        apply_upload(&mut r, 150);
        assert_eq!(r.uploaded_total(), 150);
        // Engine restarted, same gid, counter from zero.
        apply_upload(&mut r, 20);
        assert_eq!((r.uploaded_base, r.uploaded_session), (150, 20));
        assert_eq!(r.uploaded_total(), 170);
        // Re-add with a new gid.
        fold_upload(&mut r);
        assert_eq!((r.uploaded_base, r.uploaded_session), (170, 0));
        apply_upload(&mut r, 5);
        assert_eq!(r.uploaded_total(), 175);
    }

    #[test]
    fn uri_kinds_and_names() {
        assert_eq!(
            kind_of_uri("magnet:?xt=urn:btih:ab"),
            Some(SourceKind::Magnet)
        );
        assert_eq!(kind_of_uri("HTTPS://x/y"), Some(SourceKind::Http));
        assert_eq!(kind_of_uri("ftp://x/y"), Some(SourceKind::Ftp));
        assert_eq!(kind_of_uri("sftp://x/y"), Some(SourceKind::Sftp));
        assert_eq!(kind_of_uri("file:///etc/passwd"), None);
        assert_eq!(kind_of_uri("nothing"), None);
        assert_eq!(name_of_uri("http://h/a/b%20c.iso?x=1"), "b c.iso");
        assert_eq!(name_of_uri("http://h/"), "h");
        assert_eq!(
            name_of_uri("magnet:?xt=urn:btih:0123456789abcdef0123&dn=Some+Name"),
            "Some Name"
        );
        assert_eq!(
            name_of_uri("magnet:?xt=urn:btih:0123456789abcdef0123"),
            "magnet 0123456789ab"
        );
    }

    #[test]
    fn names_from_details() {
        let st = Status {
            bittorrent: Some(BitTorrent {
                info: Some(BtInfo {
                    name: Some("ubuntu".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(name_from_details(&st).as_deref(), Some("ubuntu"));
        assert!(details_final(SourceKind::Magnet, &st));
        let st = Status {
            files: Some(vec![File {
                path: Some("/dl/file.bin".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(name_from_details(&st).as_deref(), Some("file.bin"));
        assert!(!details_final(SourceKind::Http, &st));
        let st = Status {
            files: Some(vec![File {
                path: Some("[METADATA]abc".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(name_from_details(&st), None);
    }
}
