//! Removal of the partial files of deselected torrent files (D28).
//!
//! aria2's own `bt-remove-unselected-file` deletes without asking, so dman
//! implements the feature itself: with `[torrents] remove_unselected = "ask"` a
//! completed torrent that has deselected files whose partial files exist is
//! flagged (`cleanup_pending` in the record, a `Message` and a
//! `CleanupPending` event). Nothing is deleted until a client confirms with
//! `jobs.cleanup_unselected`, which removes only the files of deselected
//! indexes that exist inside the job directory (see [`crate::ops`]).

use std::path::{Path, PathBuf};

use dman_aria2::Gid;
use dman_config::RemoveUnselected;
use dman_proto::method::{CleanupFile, CleanupParams, CleanupResult, Event, Level};
use dman_proto::{ErrorCode, FileInfo, JobId, JobState, RpcError, SourceKind};
use serde_json::Value;

use crate::ops;

use super::Manager;
use super::detail::{files_from_aria2, stored_files};

/// Record key: a client has to be asked about the unselected files.
pub(crate) const PENDING: &str = "cleanup_pending";
/// Record key: the job was examined once (never flag a job twice).
const CHECKED: &str = "cleanup_checked";

/// Existing partial files of the deselected entries of `files`, relative to
/// `dir`, with their sizes. Only regular files (or symlinks, which would be
/// removed as links) that live strictly inside `dir` qualify; a path that is
/// also the path of a selected file is never included.
pub(crate) fn unselected_files(dir: &Path, files: &[FileInfo]) -> Vec<(PathBuf, u64)> {
    let Ok(root) = std::fs::canonicalize(dir) else {
        return Vec::new();
    };
    let abs = |f: &FileInfo| dir.join(&f.path);
    let selected: Vec<PathBuf> = files.iter().filter(|f| f.selected).map(abs).collect();
    let mut out = Vec::new();
    for f in files.iter().filter(|f| !f.selected) {
        if f.path.is_empty() || f.path.starts_with("[METADATA]") {
            continue;
        }
        let p = abs(f);
        if selected.contains(&p) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            continue;
        }
        let Ok(real) = ops::locate(&p) else { continue };
        if real == root || !real.starts_with(&root) {
            continue;
        }
        let rel = real.strip_prefix(&root).unwrap_or(&real).to_owned();
        out.push((rel, meta.len()));
    }
    out
}

fn is_torrent(kind: SourceKind) -> bool {
    matches!(kind, SourceKind::Torrent | SourceKind::Magnet)
}

fn human(bytes: u64) -> String {
    dman_config::format_bytes(bytes)
}

impl Manager {
    /// Flags completed torrents with leftovers of deselected files; called
    /// every tick, examines each job once.
    pub(super) async fn flag_cleanups(&mut self) {
        let ask = self.torrents.remove_unselected == RemoveUnselected::Ask;
        let todo: Vec<(JobId, PathBuf, Vec<FileInfo>)> = self
            .jobs
            .values()
            .filter(|e| {
                is_torrent(e.rec.kind)
                    && matches!(e.rec.state, JobState::Complete | JobState::Seeding)
                    && e.rec.extra.get("details_final").and_then(Value::as_bool) == Some(true)
                    && e.rec.extra.get(CHECKED).and_then(Value::as_bool) != Some(true)
            })
            .map(|e| (e.rec.id, PathBuf::from(&e.rec.dir), stored_files(&e.rec)))
            .collect();
        for (id, dir, files) in todo {
            let found = if ask && files.iter().any(|f| !f.selected) {
                tokio::task::spawn_blocking(move || unselected_files(&dir, &files))
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let Some(e) = self.jobs.get_mut(&id) else {
                continue;
            };
            e.rec.extra.insert(CHECKED.into(), Value::Bool(true));
            e.dirty = true;
            if found.is_empty() {
                continue;
            }
            let bytes: u64 = found.iter().map(|f| f.1).sum();
            let count = u32::try_from(found.len()).unwrap_or(u32::MAX);
            e.rec.extra.insert(
                PENDING.into(),
                serde_json::json!({"count": count, "bytes": bytes}),
            );
            let name = e.rec.name.clone();
            self.message(
                Level::Info,
                format!(
                    "{name}: {count} unselected partial file(s) ({}) can be deleted: `dman cleanup {id}`",
                    human(bytes)
                ),
            );
            self.emit(Event::CleanupPending {
                id,
                name,
                count,
                bytes,
            });
        }
    }

    /// `jobs.cleanup_unselected`.
    pub(super) async fn cleanup_unselected(
        &mut self,
        p: CleanupParams,
    ) -> Result<CleanupResult, RpcError> {
        let e = self
            .jobs
            .get(&p.id)
            .ok_or_else(|| RpcError::not_found(format!("job {}", p.id)))?;
        if !is_torrent(e.rec.kind) {
            return Err(RpcError::invalid("only torrents have unselected files"));
        }
        if !matches!(e.rec.state, JobState::Complete | JobState::Seeding) {
            return Err(RpcError::invalid(
                "the torrent has not completed yet; unselected files are cleaned up afterwards",
            ));
        }
        let dir = PathBuf::from(&e.rec.dir);
        let mut files = stored_files(&e.rec);
        let gid = e.rec.gid.clone();
        if let (Some(client), Some(gid)) = (self.client.clone(), gid)
            && let Ok(live) = client.get_files(&Gid::new(gid)).await
        {
            let live = files_from_aria2(&live);
            // aria2 keeps the original paths after a rename on disk; the
            // record knows the current ones.
            if live.len() == files.len() {
                for (f, l) in files.iter_mut().zip(&live) {
                    f.selected = l.selected;
                }
            }
        }
        let d2 = dir.clone();
        let found = tokio::task::spawn_blocking(move || unselected_files(&d2, &files))
            .await
            .map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))?;
        let list: Vec<CleanupFile> = found
            .iter()
            .map(|(rel, bytes)| CleanupFile {
                path: rel.display().to_string(),
                bytes: *bytes,
            })
            .collect();
        let mut res = CleanupResult {
            bytes: found.iter().map(|f| f.1).sum(),
            files: list,
            deleted: 0,
            dry_run: p.dry_run,
        };
        if p.dry_run {
            return Ok(res);
        }
        let rels: Vec<PathBuf> = found.into_iter().map(|f| f.0).collect();
        let id = p.id;
        let paths: Vec<PathBuf> = rels.iter().map(|r| dir.join(r)).collect();
        let report = tokio::task::spawn_blocking(move || ops::delete_exact_files(&dir, &paths))
            .await
            .map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))?;
        res.deleted = u32::try_from(report.deleted.len()).unwrap_or(u32::MAX);
        tracing::info!(%id, deleted = res.deleted, "deleted unselected partial files");
        if let Some(e) = self.jobs.get_mut(&id) {
            e.rec.extra.remove(PENDING);
            e.rec.extra.insert(CHECKED.into(), Value::Bool(true));
            e.dirty = true;
        }
        self.message(
            Level::Info,
            format!(
                "deleted {} unselected partial file(s) ({}) of job {id}",
                res.deleted,
                human(res.bytes)
            ),
        );
        if report.refused.is_empty() && report.failed.is_empty() {
            Ok(res)
        } else {
            let lines: Vec<String> = report
                .refused
                .iter()
                .map(|(p, r)| format!("refused {}: {r}", p.display()))
                .chain(
                    report
                        .failed
                        .iter()
                        .map(|(p, r)| format!("failed {}: {r}", p.display())),
                )
                .collect();
            let mut err = RpcError::new(
                ErrorCode::PartialFailure,
                format!("some files could not be deleted: {}", lines.join("; ")),
            );
            err.data = serde_json::to_value(&res).ok();
            Err(err)
        }
    }

    /// Re-announces every pending cleanup (for a client that just subscribed
    /// and missed the first announcement).
    pub(super) fn announce_pending_cleanups(&self) {
        let mut ids: Vec<JobId> = self
            .jobs
            .values()
            .filter(|e| e.rec.extra.contains_key(PENDING))
            .map(|e| e.rec.id)
            .collect();
        ids.sort();
        for id in ids {
            let e = &self.jobs[&id];
            let p = &e.rec.extra[PENDING];
            let num = |k: &str| p.get(k).and_then(Value::as_u64).unwrap_or(0);
            self.emit(Event::CleanupPending {
                id,
                name: e.rec.name.clone(),
                count: u32::try_from(num("count")).unwrap_or(u32::MAX),
                bytes: num("bytes"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fi(index: u32, path: &Path, selected: bool) -> FileInfo {
        FileInfo {
            index,
            path: path.display().to_string(),
            length: 10,
            completed: 0,
            selected,
        }
    }

    #[test]
    fn only_unselected_existing_files_inside_the_dir() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(dir.join("Album/sub")).unwrap();
        fs::write(dir.join("Album/01.bin"), "0123456789").unwrap(); // selected
        fs::write(dir.join("Album/sub/02.bin"), "abc").unwrap(); // unselected, partial
        fs::write(dir.join("Album/precious.txt"), "keep").unwrap(); // not in list
        let outside = t.path().join("outside.bin");
        fs::write(&outside, "x").unwrap();
        let files = vec![
            fi(1, &dir.join("Album/01.bin"), true),
            fi(2, &dir.join("Album/sub/02.bin"), false),
            fi(3, &dir.join("Album/03.bin"), false), // never created
            fi(4, &outside, false),                  // outside the job dir
            fi(5, &dir.join("Album/../../outside.bin"), false),
            fi(6, &dir.join("Album/sub"), false), // a directory
            fi(7, &dir.join("Album/01.bin"), false), // same path as a selected file
        ];
        let got = unselected_files(&dir, &files);
        assert_eq!(got, vec![(PathBuf::from("Album/sub/02.bin"), 3)]);
    }

    #[test]
    fn symlinks_are_reported_as_links_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(&dir).unwrap();
        let target = t.path().join("secret");
        fs::write(&target, "s").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("link")).unwrap();
        let got = unselected_files(&dir, &[fi(1, &dir.join("link"), false)]);
        assert_eq!(got.len(), 1);
        let rels: Vec<PathBuf> = got.iter().map(|g| dir.join(&g.0)).collect();
        let r = ops::delete_exact_files(&dir, &rels);
        assert_eq!(r.deleted.len(), 1, "{r:?}");
        assert!(target.exists(), "the link target must survive");
    }

    #[test]
    fn deleting_leaves_control_files_and_selected_files() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.bin"), "a").unwrap();
        fs::write(dir.join("a.bin.aria2"), "c").unwrap();
        let r = ops::delete_exact_files(&dir, &[dir.join("a.bin")]);
        assert_eq!(r.deleted.len(), 1);
        assert!(dir.join("a.bin.aria2").exists());
    }
}
