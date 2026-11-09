//! Free helpers shared by the manager modules.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dman_aria2::{Aria2Client, DownloadStatus, Gid, Status};
use dman_proto::{ErrorCode, JobOptions, RpcError};
use dman_store::JobRecord;
use serde_json::Value;

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub(crate) fn store_err(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::General, format!("store error: {e}"))
}

/// Maps an aria2 client error to an API error.
pub(crate) fn engine_err(e: dman_aria2::Error) -> RpcError {
    match e {
        dman_aria2::Error::Rpc { message, .. } => {
            RpcError::new(ErrorCode::General, format!("aria2: {message}"))
        }
        other => RpcError::new(
            ErrorCode::EngineUnavailable,
            format!("aria2 unavailable: {other}"),
        ),
    }
}

pub(super) fn unavailable(reason: &str) -> RpcError {
    RpcError::new(
        ErrorCode::EngineUnavailable,
        format!("aria2 is not available: {reason}"),
    )
}

pub(super) fn is_rpc_msg(e: &dman_aria2::Error, needle: &str) -> bool {
    matches!(e, dman_aria2::Error::Rpc { message, .. } if message.contains(needle))
}

/// `pause`, retried once after 200 ms on "cannot be paused now", then
/// `forcePause` (spike finding).
pub(crate) async fn pause_gid(client: &Aria2Client, gid: &Gid) -> dman_aria2::Result<()> {
    match client.pause(gid).await {
        Ok(_) => Ok(()),
        Err(e) if is_rpc_msg(&e, "cannot be paused now") => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            match client.pause(gid).await {
                Ok(_) => Ok(()),
                Err(_) => client.force_pause(gid).await.map(drop),
            }
        }
        Err(e) => Err(e),
    }
}

pub(super) fn gid_of(st: &Status) -> Option<String> {
    st.gid.as_ref().map(|g| g.as_str().to_owned())
}

/// Resolves a requested target directory (tilde-expanded, must be absolute).
pub(super) fn resolve_dir(requested: Option<&str>, default: &Path) -> Result<PathBuf, RpcError> {
    match requested {
        None => Ok(default.to_owned()),
        Some(d) => {
            let p = dman_config::expand_tilde(d);
            if p.is_absolute() {
                Ok(p)
            } else {
                Err(RpcError::invalid(format!(
                    "dir `{d}` must be an absolute path"
                )))
            }
        }
    }
}

/// Overlays the fields set in `new` onto `base`.
pub(super) fn merge_options(base: &JobOptions, new: &JobOptions) -> JobOptions {
    let mut m = base.clone();
    macro_rules! opt {
        ($($f:ident),*) => { $( if new.$f.is_some() { m.$f = new.$f.clone(); } )* };
    }
    opt!(
        dir,
        out,
        max_download_limit,
        max_upload_limit,
        select_files,
        seed_ratio,
        seed_time,
        sequential,
        user_agent,
        referer,
        proxy,
        checksum,
        connections_per_server,
        split
    );
    if !new.file_names.is_empty() {
        m.file_names = new.file_names.clone();
    }
    if !new.headers.is_empty() {
        m.headers = new.headers.clone();
    }
    for (k, v) in &new.engine_options {
        m.engine_options.insert(k.clone(), v.clone());
    }
    m
}

/// Recorded file paths of a job (`extra.files`).
pub(crate) fn recorded_files(rec: &JobRecord) -> Vec<PathBuf> {
    rec.extra
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Paths of `files` as reported by aria2 (empty paths skipped).
pub(crate) fn file_paths(files: &[dman_aria2::File]) -> Vec<String> {
    files
        .iter()
        .filter_map(|f| f.path.clone())
        .filter(|p| !p.is_empty() && !p.starts_with("[METADATA]"))
        .collect()
}

/// Waits until aria2 has really stopped `gid` (removal is asynchronous for
/// active downloads), up to a few seconds.
pub(super) async fn wait_stopped(client: &Aria2Client, gid: &Gid) {
    for _ in 0..50 {
        match client.tell_status(gid, &["status"]).await {
            Ok(st) => {
                if matches!(
                    st.status,
                    Some(
                        DownloadStatus::Removed | DownloadStatus::Complete | DownloadStatus::Error
                    )
                ) {
                    return;
                }
            }
            Err(_) => return,
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_merge_overlays_set_fields() {
        let base = JobOptions {
            max_download_limit: Some(5),
            out: Some("a".into()),
            ..Default::default()
        };
        let new = JobOptions {
            max_download_limit: Some(9),
            ..Default::default()
        };
        let m = merge_options(&base, &new);
        assert_eq!(m.max_download_limit, Some(9));
        assert_eq!(m.out.as_deref(), Some("a"));
    }

    #[test]
    fn dirs_must_be_absolute() {
        let d = Path::new("/dl");
        assert_eq!(resolve_dir(None, d).unwrap(), PathBuf::from("/dl"));
        assert_eq!(resolve_dir(Some("/x/y"), d).unwrap(), PathBuf::from("/x/y"));
        assert!(resolve_dir(Some("rel"), d).is_err());
    }
}
