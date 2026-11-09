//! `jobs.detail`: files, peers, trackers and pieces fetched from aria2 on
//! request, with the stored record as fallback.

use dman_aria2::{DownloadStatus, Gid, Peer, Status};
use dman_proto::{
    FileInfo, JobDetail, JobId, PeerInfo, PieceMap, RpcError, SourceKind, TrackerInfo,
    TrackerStatus,
};
use dman_store::JobRecord;

use super::Manager;
use super::util::recorded_files;

/// `tellStatus` keys needed for the detail view.
const STATUS_KEYS: &[&str] = &[
    "gid",
    "bitfield",
    "numPieces",
    "pieceLength",
    "bittorrent",
    "infoHash",
];

impl Manager {
    /// Builds the detail view of one job. Never fails because aria2 does not
    /// know the job (any more); only an unknown job id is an error.
    pub(super) async fn detail(&mut self, id: JobId) -> Result<JobDetail, RpcError> {
        let e = self
            .jobs
            .get(&id)
            .ok_or_else(|| RpcError::not_found(format!("job {id}")))?;
        let job = Self::job_view(e, &self.positions());
        let rec = e.rec.clone();
        let torrent = matches!(rec.kind, SourceKind::Magnet | SourceKind::Torrent);
        let active = e.aria2 == Some(DownloadStatus::Active);
        let mut d = JobDetail {
            job,
            files: stored_files(&rec),
            peers: Vec::new(),
            trackers: Vec::new(),
            pieces: None,
            info_hash: rec.info_hash.clone(),
            comment: None,
            options: rec.options.clone(),
            speed_history: self.history.job(id),
        };
        let (Some(client), Some(gid)) = (self.client.clone(), rec.gid.clone()) else {
            return Ok(d);
        };
        let gid = Gid::new(gid);
        match client.tell_status(&gid, STATUS_KEYS).await {
            Ok(st) => apply_status(&mut d, &st),
            Err(err) => {
                tracing::debug!(%id, "no live detail from aria2: {err}");
                return Ok(d);
            }
        }
        match client.get_files(&gid).await {
            Ok(files) => {
                let live = files_from_aria2(&files);
                if !live.is_empty() {
                    d.files = live;
                }
            }
            Err(err) => tracing::debug!(%id, "getFiles failed: {err}"),
        }
        if torrent && active {
            match client.get_peers(&gid).await {
                Ok(peers) => {
                    let n = d.pieces.as_ref().map_or(0, |p| u64::from(p.num_pieces));
                    d.peers = peers_from_aria2(&peers, n);
                }
                Err(err) => tracing::debug!(%id, "getPeers failed: {err}"),
            }
        }
        Ok(d)
    }
}

fn apply_status(d: &mut JobDetail, st: &Status) {
    if let Some(h) = st.info_hash.as_ref().filter(|h| !h.is_empty()) {
        d.info_hash = Some(h.clone());
    }
    if let (Some(bits), Some(n), Some(len)) = (&st.bitfield, st.num_pieces, st.piece_length)
        && !bits.is_empty()
        && n > 0
        && len > 0
    {
        d.pieces = Some(PieceMap {
            num_pieces: u32::try_from(n).unwrap_or(u32::MAX),
            piece_length: len,
            bitfield_hex: bits.clone(),
        });
    }
    if let Some(bt) = &st.bittorrent {
        d.comment = bt.comment.clone().filter(|c| !c.is_empty());
        d.trackers = trackers_from(&bt.announce_list);
    }
}

/// Trackers of an announce list; the tier is the 0-based list index.
pub(crate) fn trackers_from(tiers: &[Vec<String>]) -> Vec<TrackerInfo> {
    tiers
        .iter()
        .enumerate()
        .flat_map(|(tier, urls)| {
            urls.iter().map(move |url| TrackerInfo {
                url: url.clone(),
                tier: u32::try_from(tier).unwrap_or(u32::MAX),
                // aria2 does not report tracker status (decision D13).
                status: TrackerStatus::Unknown,
            })
        })
        .collect()
}

pub(crate) fn files_from_aria2(files: &[dman_aria2::File]) -> Vec<FileInfo> {
    files
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.path
                .as_deref()
                .is_some_and(|p| !p.is_empty() && !p.starts_with("[METADATA]"))
        })
        .map(|(i, f)| FileInfo {
            index: f
                .index
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or_else(|| u32::try_from(i + 1).unwrap_or(u32::MAX)),
            path: f.path.clone().unwrap_or_default(),
            length: f.length.unwrap_or(0),
            completed: f.completed_length.unwrap_or(0),
            selected: f.selected.unwrap_or(true),
        })
        .collect()
}

/// Files from the stored record (`extra.files`), used when aria2 no longer
/// knows the download. Sizes are only known for single-file jobs.
pub(crate) fn stored_files(rec: &JobRecord) -> Vec<FileInfo> {
    let paths = recorded_files(rec);
    let single = paths.len() == 1;
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| FileInfo {
            index: u32::try_from(i + 1).unwrap_or(u32::MAX),
            path: p.display().to_string(),
            length: if single { rec.total_bytes } else { 0 },
            completed: if single { rec.completed_bytes } else { 0 },
            selected: rec
                .options
                .select_files
                .as_ref()
                .is_none_or(|s| s.contains(&u32::try_from(i + 1).unwrap_or(u32::MAX))),
        })
        .collect()
}

pub(crate) fn peers_from_aria2(peers: &[Peer], num_pieces: u64) -> Vec<PeerInfo> {
    peers
        .iter()
        .filter_map(|p| {
            let seeder = p.seeder.unwrap_or(false);
            Some(PeerInfo {
                ip: p.ip.clone()?,
                port: p.port?,
                download_speed: p.download_speed.unwrap_or(0),
                upload_speed: p.upload_speed.unwrap_or(0),
                seeder,
                progress: if seeder {
                    1.0
                } else {
                    p.bitfield
                        .as_deref()
                        .map_or(0.0, |b| bitfield_progress(b, num_pieces))
                },
            })
        })
        .collect()
}

/// Fraction of set bits among the first `num_pieces` bits of an aria2 hex
/// bitfield (most significant bit of the first byte = piece 0). With
/// `num_pieces == 0` all bits of the string count.
pub(crate) fn bitfield_progress(hex: &str, num_pieces: u64) -> f64 {
    let total = if num_pieces > 0 {
        num_pieces
    } else {
        hex.len() as u64 * 4
    };
    if total == 0 {
        return 0.0;
    }
    let mut set = 0u64;
    let mut idx = 0u64;
    'outer: for c in hex.chars() {
        let Some(v) = c.to_digit(16) else { break };
        for bit in (0..4).rev() {
            if idx >= total {
                break 'outer;
            }
            if (v >> bit) & 1 == 1 {
                set += 1;
            }
            idx += 1;
        }
    }
    (set as f64 / total as f64).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_aria2::File;
    use dman_proto::{Engine, JobOptions, JobState};
    use dman_store::Desired;
    use serde_json::json;

    #[test]
    fn bitfield_counts_leading_pieces_only() {
        assert_eq!(bitfield_progress("ff", 8), 1.0);
        assert_eq!(bitfield_progress("f0", 8), 0.5);
        assert_eq!(bitfield_progress("00", 8), 0.0);
        // 6 pieces in "fc": padding bits beyond num_pieces are ignored.
        assert_eq!(bitfield_progress("fc", 6), 1.0);
        assert_eq!(bitfield_progress("ff", 6), 1.0);
        assert_eq!(bitfield_progress("a0", 4), 0.5);
        assert_eq!(bitfield_progress("", 8), 0.0);
        assert_eq!(bitfield_progress("f", 0), 1.0);
    }

    #[test]
    fn files_and_trackers_map() {
        let files = vec![
            File {
                index: Some(1),
                path: Some("[METADATA]x".into()),
                ..Default::default()
            },
            File {
                index: Some(2),
                path: Some("/dl/a".into()),
                length: Some(10),
                completed_length: Some(4),
                selected: Some(false),
                ..Default::default()
            },
        ];
        let f = files_from_aria2(&files);
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0],
            FileInfo {
                index: 2,
                path: "/dl/a".into(),
                length: 10,
                completed: 4,
                selected: false
            }
        );
        let t = trackers_from(&[vec!["a".into(), "b".into()], vec!["c".into()]]);
        assert_eq!(t.len(), 3);
        assert_eq!((t[2].tier, t[2].url.as_str()), (1, "c"));
        assert_eq!(t[0].status, TrackerStatus::Unknown);
    }

    #[test]
    fn peers_map_with_progress() {
        let peers = vec![
            Peer {
                ip: Some("1.1.1.1".into()),
                port: Some(6881),
                bitfield: Some("f0".into()),
                download_speed: Some(7),
                ..Default::default()
            },
            Peer {
                ip: Some("2.2.2.2".into()),
                port: Some(1),
                seeder: Some(true),
                ..Default::default()
            },
            Peer::default(),
        ];
        let p = peers_from_aria2(&peers, 8);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].progress, 0.5);
        assert_eq!(p[0].download_speed, 7);
        assert!(p[1].seeder && p[1].progress == 1.0);
    }

    #[test]
    fn stored_files_fallback() {
        let mut rec = JobRecord {
            id: JobId(1),
            engine: Engine::Aria2,
            kind: SourceKind::Http,
            name: "a".into(),
            dir: "/dl".into(),
            source: "x".into(),
            options: JobOptions::default(),
            desired: Desired::Stopped,
            state: JobState::Complete,
            gid: None,
            info_hash: None,
            total_bytes: 10,
            completed_bytes: 10,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: None,
            queue_key: 0,
            error: None,
            has_blob: false,
            extra: Default::default(),
        };
        assert!(stored_files(&rec).is_empty());
        rec.extra.insert("files".into(), json!(["/dl/a"]));
        let f = stored_files(&rec);
        assert_eq!((f.len(), f[0].length, f[0].completed), (1, 10, 10));
        assert!(f[0].selected);
    }
}
