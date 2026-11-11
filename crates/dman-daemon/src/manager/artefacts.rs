//! Removing the engine's leftovers of finished jobs.
//!
//! aria2 litters the download directory next to the real files:
//!
//! * `<name>.aria2` control files. aria2 deletes them itself once everything it
//!   was asked for is downloaded, but keeps one when only some files of a
//!   torrent were selected;
//! * `<info hash>.torrent`, a copy of the metadata of a magnet job
//!   (`bt-save-metadata`, which dman needs to learn the metadata).
//!
//! dman keeps the `.torrent` bytes of every torrent and magnet job as a blob in
//! its store and re-adds from that, so once a job is complete (or removed) these
//! files only get in the way. They are removed with the path-safe helpers of
//! [`crate::ops`], and only when they are provably the job's own:
//!
//! * a control file is only ever named after one of the job's output paths;
//! * a `.torrent` is only removed when the job recorded, before aria2 could
//!   have written it, that no such file existed, its name is the job's info
//!   hash and its content parses to that same info hash. A `.torrent` the user
//!   put there earlier, or one of another torrent, stays.
//!
//! Unfinished, paused and failed jobs keep everything: their control file is
//! what lets a retry resume.

use std::path::{Path, PathBuf};

use dman_proto::{JobId, JobState, SourceKind};
use dman_store::JobRecord;
use serde_json::{Value, json};

use super::Manager;
use super::util::recorded_files;
use crate::ops;

/// Record key: `{"hash": <info hash>, "before": <a `.torrent` of that name
/// existed before the job was added>}`.
pub(super) const KEY: &str = "torrent_file";

/// Ticks to wait after a job completed before cleaning: aria2 may still be
/// writing its control file when it reports the download as complete.
pub(crate) const SETTLE_TICKS: u8 = 2;

/// The info hash (40 hex digits, lower case) of a `magnet:` link, if it has a
/// plain hex `btih`.
pub(crate) fn magnet_hash(uri: &str) -> Option<String> {
    let query = uri.strip_prefix("magnet:?")?;
    query.split('&').find_map(|kv| {
        let v = kv.strip_prefix("xt=urn:btih:")?;
        (v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_ascii_lowercase())
    })
}

/// Paths aria2 may save the metadata of `hash` under inside `dir`.
fn torrent_paths(dir: &Path, hash: &str) -> [PathBuf; 2] {
    [
        dir.join(format!("{}.torrent", hash.to_ascii_lowercase())),
        dir.join(format!("{}.torrent", hash.to_ascii_uppercase())),
    ]
}

/// Remembers, when a job is added, which info hash its metadata copy would
/// carry and whether a file of that name is already there (then it is not
/// ours). Without a known hash nothing is recorded and no `.torrent` is ever
/// removed for the job.
pub(crate) fn note_torrent_file(rec: &mut JobRecord, hash: Option<String>) {
    let Some(hash) = hash.map(|h| h.to_ascii_lowercase()) else {
        return;
    };
    if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return;
    }
    let dir = PathBuf::from(&rec.dir);
    let before = torrent_paths(&dir, &hash)
        .iter()
        .any(|p| std::fs::symlink_metadata(p).is_ok());
    rec.extra
        .insert(KEY.into(), json!({ "hash": hash, "before": before }));
}

/// The info hash recorded for `rec` if no `.torrent` of that name existed
/// when the job was added.
fn recorded_hash(rec: &JobRecord) -> Option<String> {
    let v = rec.extra.get(KEY)?;
    if v.get("before").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let hash = v.get("hash").and_then(Value::as_str)?.to_ascii_lowercase();
    // The engine's own report must agree.
    match rec.info_hash.as_deref() {
        Some(h) if !h.eq_ignore_ascii_case(&hash) => None,
        _ => Some(hash),
    }
}

/// The metadata copy aria2 saved for `rec`, if it is the job's own: path and
/// bytes. See the module documentation for the rules.
pub(crate) fn our_torrent_file(rec: &JobRecord) -> Option<(PathBuf, Vec<u8>)> {
    if !matches!(rec.kind, SourceKind::Torrent | SourceKind::Magnet) {
        return None;
    }
    let hash = recorded_hash(rec)?;
    let dir = PathBuf::from(&rec.dir);
    for path in torrent_paths(&dir, &hash) {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if dman_torrent::parse_torrent(&bytes)
            .is_ok_and(|m| m.info_hash.eq_ignore_ascii_case(&hash))
        {
            return Some((path, bytes));
        }
    }
    None
}

/// The control files aria2 keeps for a job with output `files` (relative to
/// `dir`, or absolute): one per file and, for a torrent with a folder, the
/// folder's own `<folder>.aria2`. Without a file list `name` is used.
pub(crate) fn control_files(dir: &Path, files: &[PathBuf], name: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        let mut s = p.into_os_string();
        s.push(".aria2");
        let p = PathBuf::from(s);
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if files.is_empty() {
        if !name.is_empty() {
            push(dir.join(name));
        }
        return out;
    }
    for f in files {
        if f.as_os_str().is_empty() || f.to_string_lossy().starts_with("[METADATA]") {
            continue;
        }
        let abs = if f.is_absolute() {
            f.clone()
        } else {
            dir.join(f)
        };
        if let Ok(rel) = abs.strip_prefix(dir)
            && let Some(first) = rel.components().next()
            && rel.components().count() > 1
        {
            push(dir.join(first));
        }
        push(abs);
    }
    out
}

/// What to remove for a job.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    /// Control files (paths that may not exist).
    pub control: Vec<PathBuf>,
    /// The job's own metadata copy and its bytes.
    pub torrent: Option<(PathBuf, Vec<u8>)>,
}

/// Decides what is the job's own leftover. `with_control` = the job is done
/// (or its files go away), so its control files are no longer needed.
pub(crate) fn plan(rec: &JobRecord, with_control: bool) -> Plan {
    let dir = PathBuf::from(&rec.dir);
    Plan {
        control: if with_control {
            control_files(&dir, &recorded_files(rec), &rec.name)
        } else {
            Vec::new()
        },
        torrent: our_torrent_file(rec),
    }
}

impl Manager {
    /// Cleans every job whose settle time ran out. Every tick.
    pub(super) async fn clean_artefacts(&mut self) {
        let mut due = Vec::new();
        for e in self.jobs.values_mut() {
            let Some(n) = e.artefacts_due else { continue };
            if e.rec.state != JobState::Complete || e.running_in_engine() {
                e.artefacts_due = None;
                continue;
            }
            if n > 0 {
                e.artefacts_due = Some(n - 1);
            } else {
                e.artefacts_due = None;
                due.push(e.rec.id);
            }
        }
        for id in due {
            self.clean_artefacts_for(id, true).await;
        }
    }

    /// Removes the leftovers of job `id` (see the module documentation). The
    /// engine must no longer run the job. Never fails: a file that cannot be
    /// removed is logged and left.
    pub(super) async fn clean_artefacts_for(&mut self, id: JobId, with_control: bool) {
        let Some(e) = self.jobs.get(&id) else { return };
        if e.rec.engine != dman_proto::Engine::Aria2 {
            return;
        }
        let rec = e.rec.clone();
        let dir = PathBuf::from(&rec.dir);
        let Ok(plan) = tokio::task::spawn_blocking(move || (plan(&rec, with_control), rec)).await
        else {
            return;
        };
        let (plan, rec) = plan;
        let mut paths = plan.control;
        if let Some((path, bytes)) = plan.torrent {
            if !rec.has_blob {
                // The store must hold the metadata before the file goes.
                if let Err(err) = self.db(move |s| s.put_blob(id, &bytes)).await {
                    tracing::warn!(%id, "keeping {}: cannot store its metadata: {}", path.display(), err.message);
                    return self.delete_artefacts(id, &dir, paths).await;
                }
                if let Some(e) = self.jobs.get_mut(&id) {
                    e.rec.has_blob = true;
                    e.need_metadata = false;
                    e.dirty = true;
                }
                self.urgent = true;
            }
            paths.push(path);
        }
        self.delete_artefacts(id, &dir, paths).await;
    }

    async fn delete_artefacts(&mut self, id: JobId, dir: &Path, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let dir = dir.to_owned();
        let report = tokio::task::spawn_blocking(move || ops::delete_exact_files(&dir, &paths))
            .await
            .ok();
        let Some(report) = report else { return };
        if !report.deleted.is_empty() {
            tracing::info!(%id, count = report.deleted.len(), "removed leftover engine files");
        }
        for (p, why) in report.refused.iter().chain(report.failed.iter()) {
            tracing::warn!(%id, "kept engine file {}: {why}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::{Engine, JobOptions};
    use dman_store::Desired;

    fn torrent() -> (Vec<u8>, String) {
        let info = b"d6:lengthi3e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let mut t = b"d4:info".to_vec();
        t.extend_from_slice(info);
        t.push(b'e');
        let hash = dman_torrent::parse_torrent(&t).unwrap().info_hash;
        (t, hash)
    }

    fn rec(dir: &Path, kind: SourceKind) -> JobRecord {
        JobRecord {
            id: JobId(1),
            engine: Engine::Aria2,
            kind,
            name: "Album".into(),
            dir: dir.display().to_string(),
            source: String::new(),
            options: JobOptions::default(),
            desired: Desired::Stopped,
            state: JobState::Complete,
            gid: None,
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: Some(1),
            queue_key: 0,
            error: None,
            has_blob: true,
            extra: Default::default(),
        }
    }

    #[test]
    fn magnet_hash_needs_a_hex_btih() {
        let h = "0123456789abcdef0123456789ABCDEF01234567";
        assert_eq!(
            magnet_hash(&format!("magnet:?dn=x&xt=urn:btih:{h}&tr=y")),
            Some(h.to_ascii_lowercase())
        );
        assert_eq!(magnet_hash("magnet:?xt=urn:btih:SHORT"), None);
        assert_eq!(magnet_hash("http://x/?xt=urn:btih:{h}"), None);
    }

    #[test]
    fn removes_only_a_torrent_that_appeared_after_the_add() {
        let d = tempfile::tempdir().unwrap();
        let (bytes, hash) = torrent();
        let file = d.path().join(format!("{hash}.torrent"));

        // Recorded as absent, then aria2 writes it: ours.
        let mut r = rec(d.path(), SourceKind::Magnet);
        note_torrent_file(&mut r, Some(hash.clone()));
        assert!(our_torrent_file(&r).is_none(), "nothing there yet");
        std::fs::write(&file, &bytes).unwrap();
        let (p, b) = our_torrent_file(&r).expect("ours");
        assert_eq!(p, file);
        assert_eq!(b, bytes);

        // The same file present before the job was added: the user's.
        let mut r = rec(d.path(), SourceKind::Magnet);
        note_torrent_file(&mut r, Some(hash.clone()));
        assert_eq!(r.extra[KEY]["before"], json!(true));
        assert!(our_torrent_file(&r).is_none());

        // Nothing recorded (adopted or older job): never touched.
        assert!(our_torrent_file(&rec(d.path(), SourceKind::Torrent)).is_none());
    }

    #[test]
    fn never_removes_a_torrent_of_another_hash_or_with_other_content() {
        let d = tempfile::tempdir().unwrap();
        let (bytes, hash) = torrent();
        let mut r = rec(d.path(), SourceKind::Torrent);
        note_torrent_file(&mut r, Some(hash.clone()));

        // Content describes a different info hash than the name says.
        let other = "0".repeat(40);
        let mut r2 = rec(d.path(), SourceKind::Torrent);
        note_torrent_file(&mut r2, Some(other.clone()));
        std::fs::write(d.path().join(format!("{other}.torrent")), &bytes).unwrap();
        assert!(our_torrent_file(&r2).is_none());

        // Junk under the right name.
        std::fs::write(d.path().join(format!("{hash}.torrent")), b"junk").unwrap();
        assert!(our_torrent_file(&r).is_none());

        // Real content, but the engine reports another info hash for the job.
        std::fs::write(d.path().join(format!("{hash}.torrent")), &bytes).unwrap();
        r.info_hash = Some("1".repeat(40));
        assert!(our_torrent_file(&r).is_none());
        r.info_hash = Some(hash.to_ascii_uppercase());
        assert!(our_torrent_file(&r).is_some());

        // Not a torrent job at all.
        let mut h = rec(d.path(), SourceKind::Http);
        h.extra = r.extra.clone();
        assert!(our_torrent_file(&h).is_none());

        // A directory or a symlink of that name is not a metadata file.
        let (_, hash2) = {
            let info =
                b"d6:lengthi4e4:name1:b12:piece lengthi16384e6:pieces20:bbbbbbbbbbbbbbbbbbbbe";
            let mut t = b"d4:info".to_vec();
            t.extend_from_slice(info);
            t.push(b'e');
            (
                t.clone(),
                dman_torrent::parse_torrent(&t).unwrap().info_hash,
            )
        };
        let mut r3 = rec(d.path(), SourceKind::Torrent);
        note_torrent_file(&mut r3, Some(hash2.clone()));
        std::os::unix::fs::symlink(
            d.path().join(format!("{hash}.torrent")),
            d.path().join(format!("{hash2}.torrent")),
        )
        .unwrap();
        assert!(our_torrent_file(&r3).is_none());
    }

    #[test]
    fn control_files_are_named_after_the_jobs_own_outputs() {
        let dir = Path::new("/dl");
        let files = vec![
            PathBuf::from("Album/01.bin"),
            PathBuf::from("/dl/Album/sub/02.bin"),
        ];
        let c = control_files(dir, &files, "Album");
        assert_eq!(
            c,
            vec![
                PathBuf::from("/dl/Album.aria2"),
                PathBuf::from("/dl/Album/01.bin.aria2"),
                PathBuf::from("/dl/Album/sub/02.bin.aria2"),
            ]
        );
        assert_eq!(
            control_files(dir, &[PathBuf::from("file.bin")], "x"),
            vec![PathBuf::from("/dl/file.bin.aria2")]
        );
        assert_eq!(
            control_files(dir, &[], "file.bin"),
            vec![PathBuf::from("/dl/file.bin.aria2")]
        );
        assert!(control_files(dir, &[PathBuf::from("[METADATA]x")], "x").is_empty());
    }

    #[test]
    fn plan_keeps_control_files_of_unfinished_jobs() {
        let d = tempfile::tempdir().unwrap();
        let r = rec(d.path(), SourceKind::Http);
        assert!(plan(&r, false).control.is_empty());
        assert_eq!(plan(&r, true).control.len(), 1);
    }
}
