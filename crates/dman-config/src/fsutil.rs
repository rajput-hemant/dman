use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use crate::ConfigError;

/// Atomically replace `path` with `contents`: write a temp file in the same
/// directory, fsync it, then rename over the target. `mode` (Unix only) is the
/// permission set of the new file.
pub(crate) fn atomic_write(
    path: &Path,
    contents: &[u8],
    mode: Option<u32>,
) -> Result<(), ConfigError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| ConfigError::io(dir, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));

    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(m);
    }
    #[cfg(not(unix))]
    let _ = mode;

    // Without an explicit mode the replacement keeps the target's permissions
    // (a 0600 config must not become world-readable by being edited).
    let keep = if mode.is_none() {
        std::fs::metadata(path).ok().map(|m| m.permissions())
    } else {
        None
    };
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::PermissionsExt;
            // A stale temp file would otherwise keep its old, wider mode.
            f.set_permissions(std::fs::Permissions::from_mode(m))?;
        }
        if let Some(p) = keep {
            f.set_permissions(p)?;
        }
        f.write_all(contents)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConfigError::io(path, e));
    }
    Ok(())
}
