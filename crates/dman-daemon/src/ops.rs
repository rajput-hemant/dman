//! Deleting a job's downloaded files, with strict path safety.
//!
//! Only called for `jobs.remove` with `delete_files = true` (which clients
//! confirm twice). A path is deleted only when
//!
//! * it is one of the job's own files as reported by aria2 (or its `.aria2`
//!   control file),
//! * its parent directory canonicalizes to a location strictly inside the
//!   job's (canonicalized) target directory, so neither `..` nor symlinked
//!   parent directories can escape, and
//! * it is not a directory. A symlink is removed as a link; its target is never
//!   touched.
//!
//! Afterwards, directories that became empty are removed (`remove_dir` only
//! succeeds on empty directories), walking up but never reaching the target
//! directory itself.

use std::io;
use std::path::{Path, PathBuf};

/// What happened to each candidate path.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeleteReport {
    /// Files (and control files) that were removed.
    pub deleted: Vec<PathBuf>,
    /// Directories that were removed because they became empty.
    pub removed_dirs: Vec<PathBuf>,
    /// Paths refused by the safety rules, with the reason.
    pub refused: Vec<(PathBuf, String)>,
    /// Paths whose removal failed.
    pub failed: Vec<(PathBuf, String)>,
}

/// Where a candidate path really lives: canonical parent joined with the file
/// name (so the entry itself is addressed, not a symlink target).
pub(crate) fn locate(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .filter(|n| *n != "." && *n != "..")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent"))?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}

fn control_file(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".aria2");
    PathBuf::from(s)
}

/// Deletes the job files `files` (absolute, or relative to `dir`) and their
/// `.aria2` control files, then prunes directories inside `dir` that became
/// empty. Never deletes anything outside `dir`, never `dir` itself, never a
/// directory tree.
pub fn delete_job_files(dir: &Path, files: &[PathBuf]) -> DeleteReport {
    delete_files(dir, files, true)
}

/// Like [`delete_job_files`] but touches exactly the listed files: `.aria2`
/// control files are left alone (used for unselected-file cleanup, where the
/// job itself stays).
pub fn delete_exact_files(dir: &Path, files: &[PathBuf]) -> DeleteReport {
    delete_files(dir, files, false)
}

fn delete_files(dir: &Path, files: &[PathBuf], with_control: bool) -> DeleteReport {
    let mut report = DeleteReport::default();
    let root = match std::fs::canonicalize(dir) {
        Ok(r) => r,
        Err(e) => {
            for f in files {
                report
                    .refused
                    .push((f.clone(), format!("job directory unavailable: {e}")));
            }
            return report;
        }
    };
    let mut parents: Vec<PathBuf> = Vec::new();
    for f in files {
        if f.as_os_str().is_empty() || f.to_string_lossy().starts_with("[METADATA]") {
            continue;
        }
        let abs = if f.is_absolute() {
            f.clone()
        } else {
            dir.join(f)
        };
        let mut candidates = vec![abs.clone()];
        if with_control {
            candidates.push(control_file(&abs));
        }
        for candidate in candidates {
            let meta = match std::fs::symlink_metadata(&candidate) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    report.failed.push((candidate, e.to_string()));
                    continue;
                }
            };
            let real = match locate(&candidate) {
                Ok(r) => r,
                Err(e) => {
                    report.refused.push((candidate, e.to_string()));
                    continue;
                }
            };
            if real == root || !real.starts_with(&root) {
                report
                    .refused
                    .push((candidate, "outside the job's directory".into()));
                continue;
            }
            if meta.is_dir() {
                report
                    .refused
                    .push((candidate, "is a directory, not a file".into()));
                continue;
            }
            match std::fs::remove_file(&real) {
                Ok(()) => {
                    if let Some(p) = real.parent() {
                        parents.push(p.to_owned());
                    }
                    report.deleted.push(real);
                }
                Err(e) => report.failed.push((real, e.to_string())),
            }
        }
    }
    // Prune now-empty directories created for the job (deepest first).
    parents.sort_by(|a, b| {
        b.components()
            .count()
            .cmp(&a.components().count())
            .then_with(|| a.cmp(b))
    });
    parents.dedup();
    for start in parents {
        let mut cur = start;
        while cur != root && cur.starts_with(&root) {
            if report.removed_dirs.contains(&cur) {
                break;
            }
            if std::fs::remove_dir(&cur).is_err() {
                break; // not empty (or not removable): stop walking up
            }
            report.removed_dirs.push(cur.clone());
            match cur.parent() {
                Some(p) => cur = p.to_owned(),
                None => break,
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn deletes_only_listed_files_and_their_control_files() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.bin"), "a").unwrap();
        fs::write(dir.join("a.bin.aria2"), "c").unwrap();
        fs::write(dir.join("other.bin"), "o").unwrap();
        let r = delete_job_files(&dir, &[dir.join("a.bin")]);
        assert_eq!(r.deleted.len(), 2, "{r:?}");
        assert!(!dir.join("a.bin").exists());
        assert!(!dir.join("a.bin.aria2").exists());
        assert!(dir.join("other.bin").exists());
        assert!(dir.exists());
        assert!(r.removed_dirs.is_empty());
    }

    #[test]
    fn refuses_paths_outside_dir() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(&dir).unwrap();
        let outside = t.path().join("precious.txt");
        fs::write(&outside, "keep").unwrap();
        let r = delete_job_files(&dir, &[outside.clone(), dir.join("../precious.txt")]);
        assert!(outside.exists());
        assert!(r.deleted.is_empty());
        assert_eq!(r.refused.len(), 2, "{r:?}");
    }

    #[cfg(unix)] // symlinks need extra rights on Windows
    #[test]
    fn refuses_symlinked_parent_escape() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        let elsewhere = t.path().join("elsewhere");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("x"), "keep").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("link")).unwrap();
        let r = delete_job_files(&dir, &[dir.join("link/x")]);
        assert!(elsewhere.join("x").exists());
        assert!(r.deleted.is_empty());
        assert_eq!(r.refused.len(), 1);
    }

    #[cfg(unix)] // symlinks need extra rights on Windows
    #[test]
    fn symlink_file_is_removed_as_link_only() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(&dir).unwrap();
        let target = t.path().join("target");
        fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("f")).unwrap();
        let r = delete_job_files(&dir, &[dir.join("f")]);
        assert_eq!(r.deleted.len(), 1);
        assert!(target.exists());
        assert!(fs::symlink_metadata(dir.join("f")).is_err());
    }

    #[test]
    fn refuses_directories_and_the_root() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/keep"), "k").unwrap();
        let r = delete_job_files(&dir, &[dir.join("sub"), dir.clone(), dir.join(".")]);
        assert!(dir.join("sub/keep").exists());
        assert!(r.deleted.is_empty());
        assert_eq!(r.refused.len(), 3, "{r:?}");
    }

    #[test]
    fn prunes_empty_torrent_dirs_but_not_root_or_nonempty() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("dl");
        fs::create_dir_all(dir.join("tor/sub")).unwrap();
        fs::create_dir_all(dir.join("tor2")).unwrap();
        fs::write(dir.join("tor/sub/1"), "1").unwrap();
        fs::write(dir.join("tor/2"), "2").unwrap();
        fs::write(dir.join("tor2/a"), "a").unwrap();
        fs::write(dir.join("tor2/unrelated"), "u").unwrap();
        let r = delete_job_files(
            &dir,
            &[
                PathBuf::from("tor/sub/1"),
                dir.join("tor/2"),
                dir.join("tor2/a"),
            ],
        );
        assert_eq!(r.deleted.len(), 3, "{r:?}");
        assert!(!dir.join("tor").exists());
        assert!(dir.join("tor2/unrelated").exists());
        assert!(dir.exists());
    }

    #[test]
    fn missing_dir_refuses_everything() {
        let t = tempfile::tempdir().unwrap();
        let r = delete_job_files(&t.path().join("nope"), &[t.path().join("nope/x")]);
        assert_eq!(r.refused.len(), 1);
    }
}
