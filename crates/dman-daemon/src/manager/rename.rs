//! `jobs.rename`.
//!
//! * Waiting / paused: the engine renames the output when it (re)starts
//!   (`out`, or `index-out` with the FULL mapping, see the aria2 spike).
//! * Complete: the file (or a torrent's top-level directory) is moved inside
//!   the job directory and the move is recorded. Nothing is overwritten.
//! * Active, seeding, failed and removed jobs are refused.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use dman_aria2::{Gid, job_options_to_aria2};
use dman_proto::method::{Level, RenameParams};
use dman_proto::{ErrorCode, JobOptions, JobState, RpcError, SourceKind};
use dman_store::JobRecord;
use serde_json::{Value, json};

use super::util::{engine_err, now, recorded_files};
use super::{Entry, Manager};

/// Longest file name accepted (bytes), as on most filesystems.
const MAX_NAME: usize = 255;

/// A new name must be one plain path component.
pub(crate) fn validate_name(name: &str) -> Result<(), RpcError> {
    let bad = |why: &str| Err(RpcError::invalid(format!("invalid name: {why}")));
    if name.is_empty() {
        return bad("it is empty");
    }
    if name.trim() != name {
        return bad("it starts or ends with whitespace");
    }
    if name == "." || name == ".." {
        return bad("`.` and `..` are not allowed");
    }
    if name.contains(['/', '\\']) {
        return bad("it must be a file name, not a path (no `/`)");
    }
    if name.chars().any(char::is_control) {
        return bad("it contains control characters");
    }
    if name.len() > MAX_NAME {
        return bad("it is too long");
    }
    Ok(())
}

/// The full `index-out` mapping after renaming file `index` to `name`.
pub(crate) fn merge_index_out(
    existing: &BTreeMap<u32, String>,
    index: u32,
    name: &str,
) -> BTreeMap<u32, String> {
    let mut m = existing.clone();
    m.insert(index, name.to_owned());
    m
}

/// How an in-place rename is applied.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InPlace {
    /// `out`
    Out,
    /// `index-out` of this 1-based file.
    Index(u32),
}

/// Chooses between `out` and `index-out`. `files` is the number of files when known.
pub(crate) fn plan_in_place(
    kind: SourceKind,
    files: Option<usize>,
    file_index: Option<u32>,
) -> Result<InPlace, RpcError> {
    if let Some(0) = file_index {
        return Err(RpcError::invalid("file indexes start at 1"));
    }
    let bt = matches!(kind, SourceKind::Torrent | SourceKind::Magnet);
    if bt || files.is_some_and(|n| n > 1) {
        let idx = match (file_index, files) {
            (Some(i), _) => i,
            (None, Some(1)) => 1,
            (None, _) => {
                return Err(RpcError::invalid(
                    "this download has several files (or they are not known yet): give file_index",
                ));
            }
        };
        if let Some(n) = files
            && usize::try_from(idx).unwrap_or(usize::MAX) > n
        {
            return Err(RpcError::invalid(format!(
                "file_index {idx} is out of range (the download has {n} file(s))"
            )));
        }
        Ok(InPlace::Index(idx))
    } else if file_index.is_some_and(|i| i != 1) {
        Err(RpcError::invalid("this download has a single file"))
    } else {
        Ok(InPlace::Out)
    }
}

/// Moves `src` to `dst` without ever replacing an existing entry.
pub(crate) fn move_no_overwrite(src: &Path, dst: &Path) -> io::Result<()> {
    let exists = || {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", dst.display()),
        )
    };
    if std::fs::symlink_metadata(dst).is_ok() {
        return Err(exists());
    }
    let is_file = std::fs::symlink_metadata(src).is_ok_and(|m| !m.is_dir());
    if is_file {
        // hard_link fails atomically when `dst` exists: no check/rename race.
        match std::fs::hard_link(src, dst) {
            Ok(()) => {
                return std::fs::remove_file(src).inspect_err(|_| {
                    let _ = std::fs::remove_file(dst);
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(exists()),
            Err(_) => {} // e.g. no hard links on this filesystem: plain rename
        }
    }
    std::fs::rename(src, dst)
}

/// What was moved on disk.
#[derive(Debug)]
struct Moved {
    /// Source and target as recorded (job-dir relative when possible).
    from: PathBuf,
    to: PathBuf,
    /// Source as stored in the job's file list, and its replacement.
    raw_from: PathBuf,
    raw_to: PathBuf,
}

fn rel(p: &Path, base: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

fn refuse_active() -> RpcError {
    RpcError::refused("cannot rename an active transfer — pause it first")
}

/// Blocking part of renaming a completed job. `files` as recorded (absolute
/// or relative to `dir`).
fn move_on_disk(
    dir: &Path,
    files: &[PathBuf],
    file_index: Option<u32>,
    name: &str,
) -> Result<Moved, RpcError> {
    let general = |m: String| RpcError::new(ErrorCode::General, m);
    if files.is_empty() {
        return Err(general("the job's files are not known".into()));
    }
    let root = std::fs::canonicalize(dir)
        .map_err(|e| general(format!("job directory unavailable: {e}")))?;
    let abs = |f: &PathBuf| {
        if f.is_absolute() {
            f.clone()
        } else {
            dir.join(f)
        }
    };
    let multi = files.len() > 1;
    // (raw source path, is the top-level directory)
    let (raw_from, is_dir) = match file_index {
        Some(i) => {
            let f = usize::try_from(i)
                .ok()
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| files.get(i))
                .ok_or_else(|| {
                    RpcError::invalid(format!(
                        "file_index {i} is out of range (the job has {} file(s))",
                        files.len()
                    ))
                })?;
            (abs(f), false)
        }
        None if !multi => (abs(&files[0]), false),
        None => {
            // The top-level directory below the job directory.
            let first = abs(&files[0]);
            let top = first
                .strip_prefix(dir)
                .ok()
                .and_then(|r| r.components().next())
                .filter(|_| {
                    first
                        .strip_prefix(dir)
                        .is_ok_and(|r| r.components().count() > 1)
                })
                .map(|c| dir.join(c.as_os_str()))
                .ok_or_else(|| {
                    RpcError::invalid(
                        "the files are not inside a common directory: give file_index",
                    )
                })?;
            (top, true)
        }
    };
    let src = crate::ops::locate(&raw_from).map_err(|e| general(format!("bad path: {e}")))?;
    if src == root || !src.starts_with(&root) {
        return Err(RpcError::refused("the file is outside the job's directory"));
    }
    match std::fs::symlink_metadata(&src) {
        Ok(m) if m.is_dir() == is_dir => {}
        Ok(_) => {
            return Err(general(format!(
                "{} is not what was expected",
                src.display()
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                format!("{} no longer exists on disk", src.display()),
            ));
        }
        Err(e) => return Err(general(e.to_string())),
    }
    let parent = src.parent().unwrap_or(&root);
    let dst = parent.join(name);
    if dst == src {
        return Err(RpcError::invalid("that is already the name"));
    }
    move_no_overwrite(&src, &dst).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            RpcError::refused(format!("`{name}` already exists; nothing was overwritten"))
        } else {
            general(format!("cannot rename {}: {e}", src.display()))
        }
    })?;
    let raw_to = raw_from
        .parent()
        .map_or_else(|| PathBuf::from(name), |p| p.join(name));
    Ok(Moved {
        from: PathBuf::from(rel(&src, &root)),
        to: PathBuf::from(rel(&dst, &root)),
        raw_from,
        raw_to,
    })
}

/// Rewrites stored file paths after a move (a directory move rewrites every path below it).
fn rewrite_files(rec: &mut JobRecord, dir: &Path, mv: &Moved) {
    let files = recorded_files(rec);
    let out: Vec<String> = files
        .iter()
        .map(|f| {
            let abs = if f.is_absolute() {
                f.clone()
            } else {
                dir.join(f)
            };
            let new = match abs.strip_prefix(&mv.raw_from) {
                Ok(rest) if rest.as_os_str().is_empty() => mv.raw_to.clone(),
                Ok(rest) => mv.raw_to.join(rest),
                Err(_) => f.clone(),
            };
            new.display().to_string()
        })
        .collect();
    rec.extra.insert("files".into(), json!(out));
}

impl Manager {
    pub(super) async fn rename(&mut self, p: RenameParams) -> Result<(), RpcError> {
        validate_name(&p.name)?;
        let e = self
            .jobs
            .get(&p.id)
            .ok_or_else(|| RpcError::not_found(format!("job {}", p.id)))?;
        match e.rec.state {
            JobState::Active | JobState::Seeding => Err(refuse_active()),
            JobState::Error | JobState::Removed => Err(RpcError::refused(format!(
                "job {} has failed or was removed; it cannot be renamed",
                p.id
            ))),
            JobState::Waiting | JobState::Paused => {
                if matches!(e.aria2, Some(dman_aria2::DownloadStatus::Active)) {
                    return Err(refuse_active());
                }
                self.rename_in_place(p).await
            }
            JobState::Complete => self.rename_complete(p).await,
        }
    }

    async fn known_file_count(&self, e: &Entry) -> Option<usize> {
        let n = recorded_files(&e.rec).len();
        if n > 0 {
            return Some(n);
        }
        if e.rec.kind == SourceKind::Torrent && e.rec.has_blob {
            let id = e.rec.id;
            let blob = self.db(move |s| s.get_blob(id)).await.ok().flatten()?;
            return dman_torrent::parse_torrent(&blob)
                .ok()
                .map(|m| m.files.len());
        }
        None
    }

    async fn rename_in_place(&mut self, p: RenameParams) -> Result<(), RpcError> {
        let e = &self.jobs[&p.id];
        let files = self.known_file_count(e).await;
        let e = &self.jobs[&p.id];
        let plan = plan_in_place(e.rec.kind, files, p.file_index)?;
        let mut delta = JobOptions::default();
        let mut merged = e.rec.options.clone();
        match plan {
            InPlace::Out => {
                delta.out = Some(p.name.clone());
                merged.out = Some(p.name.clone());
            }
            InPlace::Index(i) => {
                // aria2 only keeps the last mapping: always send the full one.
                let full = merge_index_out(&e.rec.options.file_names, i, &p.name);
                delta.file_names = full.clone();
                merged.file_names = full;
            }
        }
        let changes = job_options_to_aria2(&delta).map_err(|e| RpcError::invalid(e.to_string()))?;
        if let (Some(c), Some(gid)) = (self.client.clone(), e.rec.gid.clone())
            && e.aria2.is_some()
        {
            c.change_option(&Gid::new(gid), &changes)
                .await
                .map_err(engine_err)?;
        }
        let single = files.is_none_or(|n| n <= 1);
        if let Some(e) = self.jobs.get_mut(&p.id) {
            e.rec.options = merged;
            if matches!(plan, InPlace::Out) || (single && plan == InPlace::Index(1)) {
                e.rec.name = p.name.clone();
            }
            // Re-read the file list from aria2 to show the new paths.
            e.need_details = true;
            e.rec.extra.remove("details_final");
        }
        self.mark(p.id, true);
        self.flush(true).await;
        Ok(())
    }

    async fn rename_complete(&mut self, p: RenameParams) -> Result<(), RpcError> {
        let e = &self.jobs[&p.id];
        let rec = e.rec.clone();
        let dir = PathBuf::from(&rec.dir);
        let files = recorded_files(&rec);
        let (name, idx) = (p.name.clone(), p.file_index);
        let (d, f) = (dir.clone(), files);
        let mv = tokio::task::spawn_blocking(move || move_on_disk(&d, &f, idx, &name))
            .await
            .map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))??;
        let (id, from, to, at) = (
            p.id,
            mv.from.display().to_string(),
            mv.to.display().to_string(),
            now(),
        );
        if let Err(err) = self.db(move |s| s.record_rename(id, &from, &to, at)).await {
            // The file is already moved; do not hide that.
            self.message(
                Level::Error,
                format!(
                    "job {id}: renamed on disk but could not record it: {}",
                    err.message
                ),
            );
        }
        if let Some(e) = self.jobs.get_mut(&p.id) {
            let whole = e
                .rec
                .extra
                .get("files")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
                <= 1;
            rewrite_files(&mut e.rec, &dir, &mv);
            if whole || p.file_index.is_none() {
                e.rec.name = p.name.clone();
            }
            if whole && e.rec.kind != SourceKind::Torrent {
                e.rec.options.out = Some(p.name.clone());
            }
        }
        self.message(
            Level::Info,
            format!(
                "job {}: renamed {} to {}",
                p.id,
                mv.from.display(),
                mv.to.display()
            ),
        );
        self.mark(p.id, true);
        self.flush(true).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn names_are_validated() {
        for ok in ["a.bin", "my file (1).iso", ".hidden", "a..b", "ü"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            " ",
            ".",
            "..",
            "a/b",
            "/abs",
            "../x",
            "a\\b",
            "..\\x",
            "a\nb",
            " a",
            "a ",
            &"x".repeat(256),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn index_out_mapping_is_merged_not_replaced() {
        let mut m = BTreeMap::new();
        m.insert(1, "a".to_owned());
        m.insert(3, "c".to_owned());
        let n = merge_index_out(&m, 2, "b");
        assert_eq!(n.len(), 3);
        let n = merge_index_out(&n, 1, "z");
        assert_eq!(n[&1], "z");
        assert_eq!(n[&2], "b");
        assert_eq!(n[&3], "c");
        assert_eq!(m.len(), 2, "input untouched");
        // The aria2 mapping carries all entries.
        let o = JobOptions {
            file_names: n,
            ..Default::default()
        };
        let a = job_options_to_aria2(&o).unwrap();
        assert_eq!(a.get("index-out"), Some("1=z\n2=b\n3=c"));
    }

    #[test]
    fn plan_chooses_out_or_index() {
        use SourceKind::*;
        assert_eq!(plan_in_place(Http, Some(1), None).unwrap(), InPlace::Out);
        assert_eq!(plan_in_place(Http, None, None).unwrap(), InPlace::Out);
        assert!(plan_in_place(Http, Some(1), Some(2)).is_err());
        assert_eq!(
            plan_in_place(Torrent, Some(1), None).unwrap(),
            InPlace::Index(1)
        );
        assert_eq!(
            plan_in_place(Torrent, Some(3), Some(2)).unwrap(),
            InPlace::Index(2)
        );
        assert!(plan_in_place(Torrent, Some(3), None).is_err());
        assert!(plan_in_place(Magnet, None, None).is_err());
        assert!(plan_in_place(Torrent, Some(3), Some(4)).is_err());
        assert!(plan_in_place(Torrent, Some(3), Some(0)).is_err());
        assert_eq!(
            plan_in_place(Metalink, Some(2), Some(2)).unwrap(),
            InPlace::Index(2)
        );
        assert_eq!(
            plan_in_place(Metalink, Some(1), None).unwrap(),
            InPlace::Out
        );
    }

    #[test]
    fn move_never_overwrites() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("a"), "a").unwrap();
        fs::write(t.path().join("b"), "b").unwrap();
        let e = move_no_overwrite(&t.path().join("a"), &t.path().join("b")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(t.path().join("b")).unwrap(), "b");
        assert!(t.path().join("a").exists());
        move_no_overwrite(&t.path().join("a"), &t.path().join("c")).unwrap();
        assert!(!t.path().join("a").exists());
        assert_eq!(fs::read_to_string(t.path().join("c")).unwrap(), "a");
    }

    #[test]
    fn complete_single_file_and_torrent_dir() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(dir.join("T")).unwrap();
        fs::write(dir.join("f.bin"), "f").unwrap();
        fs::write(dir.join("T/a"), "a").unwrap();
        fs::write(dir.join("T/b"), "b").unwrap();
        let mv = move_on_disk(&dir, &[dir.join("f.bin")], None, "g.bin").unwrap();
        assert_eq!(mv.from, PathBuf::from("f.bin"));
        assert_eq!(mv.to, PathBuf::from("g.bin"));
        assert!(dir.join("g.bin").exists() && !dir.join("f.bin").exists());
        // Torrent: one file, then the directory.
        let files = [dir.join("T/a"), dir.join("T/b")];
        let mv = move_on_disk(&dir, &files, Some(2), "bb").unwrap();
        assert_eq!(mv.to, PathBuf::from("T/bb"));
        let files = [dir.join("T/a"), dir.join("T/bb")];
        let mv = move_on_disk(&dir, &files, None, "U").unwrap();
        assert_eq!(mv.to, PathBuf::from("U"));
        assert!(dir.join("U/a").exists() && dir.join("U/bb").exists());
        let mut rec = test_rec(&files);
        rewrite_files(&mut rec, &dir, &mv);
        let nf = recorded_files(&rec);
        assert_eq!(nf, vec![dir.join("U/a"), dir.join("U/bb")]);
    }

    #[test]
    fn complete_refuses_existing_target_missing_source_and_escapes() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a"), "a").unwrap();
        fs::write(dir.join("b"), "b").unwrap();
        let e = move_on_disk(&dir, &[dir.join("a")], None, "b").unwrap_err();
        assert_eq!(e.code, ErrorCode::Refused);
        assert_eq!(fs::read_to_string(dir.join("b")).unwrap(), "b");
        let e = move_on_disk(&dir, &[dir.join("zz")], None, "y").unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        let outside = t.path().join("out.txt");
        fs::write(&outside, "o").unwrap();
        let e = move_on_disk(&dir, std::slice::from_ref(&outside), None, "y").unwrap_err();
        assert_eq!(e.code, ErrorCode::Refused);
        assert!(outside.exists());
        let e = move_on_disk(&dir, &[dir.join("../out.txt")], None, "y").unwrap_err();
        assert_eq!(e.code, ErrorCode::Refused);
        assert!(outside.exists());
        assert!(move_on_disk(&dir, &[], None, "y").is_err());
    }

    fn test_rec(files: &[PathBuf]) -> JobRecord {
        use dman_proto::Engine;
        use dman_store::Desired;
        let mut rec = JobRecord {
            id: dman_proto::JobId(1),
            engine: Engine::Aria2,
            kind: SourceKind::Torrent,
            name: "T".into(),
            dir: "/dl".into(),
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
            completed_at: None,
            queue_key: 0,
            error: None,
            has_blob: false,
            extra: Default::default(),
        };
        rec.extra.insert(
            "files".into(),
            json!(
                files
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
            ),
        );
        rec
    }
}
