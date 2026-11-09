//! `fs.list`: directory listing for client-side directory pickers.

use std::cmp::Ordering;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use dman_config::{Profile, expand_tilde};
use dman_proto::method::{FsEntry, FsListParams, FsListing};
use dman_proto::{ErrorCode, RpcError};

/// Most entries returned for one directory.
pub(crate) const MAX_ENTRIES: usize = 2000;

/// Handles `fs.list`. The blocking directory read runs off the manager thread.
pub(crate) async fn list(profile: &Profile, p: FsListParams) -> Result<FsListing, RpcError> {
    let dir = match p.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => expand_tilde(s),
        None => nearest_existing(profile.download_path()),
    };
    let dirs_only = p.dirs_only;
    tokio::task::spawn_blocking(move || list_dir(&dir, dirs_only, MAX_ENTRIES))
        .await
        .map_err(|e| RpcError::new(ErrorCode::General, format!("listing failed: {e}")))?
}

/// `path` or its closest ancestor that exists.
fn nearest_existing(path: PathBuf) -> PathBuf {
    path.ancestors()
        .find(|a| a.is_dir())
        .map_or(path.clone(), Path::to_path_buf)
}

fn io_err(path: &Path, e: &std::io::Error) -> RpcError {
    let code = if e.kind() == ErrorKind::NotFound {
        ErrorCode::NotFound
    } else {
        ErrorCode::InvalidInput
    };
    RpcError::new(code, format!("{}: {e}", path.display()))
}

/// List `dir`: directories first, then files, each sorted by name
/// (case-insensitive). At most `cap` entries are read; `truncated` says more
/// existed.
pub(crate) fn list_dir(dir: &Path, dirs_only: bool, cap: usize) -> Result<FsListing, RpcError> {
    let canonical = std::fs::canonicalize(dir).map_err(|e| io_err(dir, &e))?;
    let rd = std::fs::read_dir(&canonical).map_err(|e| io_err(&canonical, &e))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for item in rd {
        let Ok(item) = item else { continue };
        // Follow symlinks so a link to a directory is browsable.
        let Ok(meta) = std::fs::metadata(item.path()).or_else(|_| item.metadata()) else {
            continue;
        };
        let is_dir = meta.is_dir();
        if dirs_only && !is_dir {
            continue;
        }
        if entries.len() >= cap {
            truncated = true;
            break;
        }
        entries.push(FsEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            is_dir,
            size: (!is_dir).then_some(meta.len()),
        });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a
            .name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name)),
    });
    Ok(FsListing {
        path: canonical.display().to_string(),
        parent: canonical.parent().map(|p| p.display().to_string()),
        entries,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_first_sorted_and_filterable() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("b-dir")).unwrap();
        std::fs::create_dir(t.path().join("A-dir")).unwrap();
        std::fs::write(t.path().join("a.txt"), b"12345").unwrap();
        let l = list_dir(t.path(), false, 100).unwrap();
        let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A-dir", "b-dir", "a.txt"]);
        assert_eq!(l.entries[2].size, Some(5));
        assert_eq!(l.entries[0].size, None);
        assert!(l.parent.is_some() && !l.truncated);
        let only = list_dir(t.path(), true, 100).unwrap();
        assert_eq!(only.entries.len(), 2);
    }

    #[test]
    fn cap_and_errors() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(t.path().join(format!("f{i}")), b"").unwrap();
        }
        let l = list_dir(t.path(), false, 3).unwrap();
        assert_eq!(l.entries.len(), 3);
        assert!(l.truncated);
        let e = list_dir(&t.path().join("nope"), false, 3).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        let e = list_dir(&t.path().join("f0"), false, 3).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn default_falls_back_to_existing_ancestor() {
        let t = tempfile::tempdir().unwrap();
        let deep = t.path().join("x/y/z");
        assert_eq!(nearest_existing(deep), t.path());
    }
}
