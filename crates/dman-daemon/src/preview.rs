//! `jobs.preview_magnet`: fetch a magnet's metadata (file list) without
//! creating a job.
//!
//! The magnet is added to aria2 with `bt-metadata-only` and
//! `bt-save-metadata` into a dman-private directory (`<state>/metadata/<n>`),
//! bypassing dman's queue and store. aria2 then saves `<infohash>.torrent` and
//! (spike) reports no `followedBy`, so the file is read, parsed and returned
//! together with its bytes. The aria2 result and the saved file are removed
//! afterwards, also when the caller disconnects or the timeout hits.
//!
//! The job manager must not adopt these downloads as jobs: their gids are
//! registered in a [`PreviewRegistry`] shared with it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use dman_aria2::{Aria2Client, DownloadStatus, EngineState, Gid, Options};
use dman_proto::method::{MagnetPreview, PreviewMagnetParams};
use dman_proto::{ErrorCode, FileInfo, RpcError};
use tokio::sync::{Semaphore, watch};

/// Default and maximum wait for the metadata.
pub const DEFAULT_TIMEOUT_SECS: u32 = 120;
pub const MAX_TIMEOUT_SECS: u32 = 600;
/// Previews that may run at the same time.
const MAX_PARALLEL: usize = 4;
const POLL: Duration = Duration::from_millis(200);

/// Which aria2 downloads belong to previews (and must be left alone by the job manager).
#[derive(Debug)]
pub struct PreviewRegistry {
    dir: PathBuf,
    gids: Mutex<HashSet<String>>,
    counter: AtomicU64,
}

impl PreviewRegistry {
    /// `dir` is the private metadata directory.
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            dir,
            gids: Mutex::new(HashSet::new()),
            counter: AtomicU64::new(0),
        })
    }

    /// The private metadata directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn set(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.gids.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `gid` is a running preview.
    pub fn is_preview_gid(&self, gid: &str) -> bool {
        self.set().contains(gid)
    }

    /// Whether an aria2 download directory lies inside the private directory
    /// (also catches previews left behind by a crashed daemon).
    pub fn owns_dir(&self, dir: &str) -> bool {
        Path::new(dir).starts_with(&self.dir)
    }

    /// Empties the private directory (at daemon start).
    pub fn reset_dir(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::create_dir_all(&self.dir);
    }

    fn fresh(&self) -> (String, PathBuf) {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let mix = nanos.rotate_left(17)
            ^ (u64::from(std::process::id()) << 32)
            ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // aria2 gids are 16 hex digits; all-zero is reserved.
        let gid = format!("{:016x}", mix | 1);
        let dir = self.dir.join(&gid);
        (gid, dir)
    }
}

/// The preview service.
#[derive(Debug)]
pub struct Preview {
    registry: Arc<PreviewRegistry>,
    engine: watch::Receiver<EngineState>,
    slots: Semaphore,
}

fn err(code: ErrorCode, m: impl Into<String>) -> RpcError {
    RpcError::new(code, m)
}

/// Checks that `uri` is a magnet link with a BitTorrent info hash.
pub(crate) fn check_magnet(uri: &str) -> Result<(), RpcError> {
    let u = uri.trim();
    if !u
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))
    {
        return Err(RpcError::invalid("not a magnet link"));
    }
    if u.contains(['\n', '\r', '\0']) {
        return Err(RpcError::invalid("the magnet link contains a line break"));
    }
    if !u.to_ascii_lowercase().contains("xt=urn:btih:") {
        return Err(RpcError::invalid(
            "the magnet link has no `xt=urn:btih:` hash",
        ));
    }
    Ok(())
}

impl Preview {
    pub fn new(registry: Arc<PreviewRegistry>, engine: watch::Receiver<EngineState>) -> Self {
        Self {
            registry,
            engine,
            slots: Semaphore::new(MAX_PARALLEL),
        }
    }

    fn client(&self) -> Result<Aria2Client, RpcError> {
        match &*self.engine.borrow() {
            EngineState::Running { client, .. } => Ok(client.clone()),
            EngineState::Failed { reason } | EngineState::Restarting { reason, .. } => Err(err(
                ErrorCode::EngineUnavailable,
                format!("aria2 is not available: {reason}"),
            )),
            _ => Err(err(ErrorCode::EngineUnavailable, "aria2 is not running")),
        }
    }

    /// Fetches the metadata. Dropping the returned future cancels the
    /// preview and cleans up in the background.
    pub async fn run(&self, p: PreviewMagnetParams) -> Result<MagnetPreview, RpcError> {
        check_magnet(&p.uri)?;
        let secs = match p.timeout_secs {
            None => DEFAULT_TIMEOUT_SECS,
            Some(0) => return Err(RpcError::invalid("timeout_secs must be at least 1")),
            Some(n) => n.min(MAX_TIMEOUT_SECS),
        };
        let client = self.client()?;
        let _slot = self
            .slots
            .try_acquire()
            .map_err(|_| RpcError::refused("too many magnet previews in progress"))?;
        let (gid, dir) = self.registry.fresh();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            err(
                ErrorCode::General,
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
        // Register before adding so the job manager never sees an unknown gid.
        self.registry.set().insert(gid.clone());
        let cleanup = Cleanup::new(
            client.clone(),
            gid.clone(),
            dir.clone(),
            self.registry.clone(),
        );
        let mut opts = Options::new();
        opts.set("dir", dir.display().to_string());
        opts.set("gid", gid.clone());
        opts.set("bt-metadata-only", "true");
        opts.set("bt-save-metadata", "true");
        if let Err(e) = client
            .add_uri(&[p.uri.trim().to_owned()], &opts, None)
            .await
        {
            cleanup.finish().await;
            return Err(match e {
                dman_aria2::Error::Rpc { message, .. } => {
                    RpcError::invalid(format!("aria2 rejected the magnet link: {message}"))
                }
                other => err(
                    ErrorCode::EngineUnavailable,
                    format!("aria2 unavailable: {other}"),
                ),
            });
        }
        let result = self.wait(&client, &gid, &dir, secs).await;
        cleanup.finish().await;
        result
    }

    async fn wait(
        &self,
        client: &Aria2Client,
        gid: &str,
        dir: &Path,
        secs: u32,
    ) -> Result<MagnetPreview, RpcError> {
        let g = Gid::new(gid);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(secs));
        loop {
            match client
                .tell_status(&g, &["status", "errorCode", "errorMessage"])
                .await
            {
                Ok(st) => match st.status {
                    Some(DownloadStatus::Complete) => break,
                    Some(DownloadStatus::Error) => {
                        return Err(err(
                            ErrorCode::General,
                            format!(
                                "could not fetch the magnet's metadata: {}",
                                st.error_message.unwrap_or_else(|| "unknown error".into())
                            ),
                        ));
                    }
                    Some(DownloadStatus::Removed) => {
                        return Err(err(ErrorCode::General, "the metadata download was removed"));
                    }
                    _ => {}
                },
                Err(dman_aria2::Error::Rpc { message, .. }) => {
                    return Err(err(
                        ErrorCode::General,
                        format!("could not fetch the magnet's metadata: {message}"),
                    ));
                }
                Err(e) => {
                    return Err(err(
                        ErrorCode::EngineUnavailable,
                        format!("aria2 unavailable: {e}"),
                    ));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(err(
                    ErrorCode::General,
                    format!(
                        "timed out after {secs} s waiting for the magnet's metadata (no peers found?)"
                    ),
                ));
            }
            tokio::time::sleep(POLL).await;
        }
        let dir = dir.to_owned();
        tokio::task::spawn_blocking(move || read_preview(&dir))
            .await
            .map_err(|e| err(ErrorCode::General, e.to_string()))?
    }
}

/// Reads the single `.torrent` aria2 saved in `dir`.
fn read_preview(dir: &Path) -> Result<MagnetPreview, RpcError> {
    let general = |m: String| err(ErrorCode::General, m);
    let path = std::fs::read_dir(dir)
        .map_err(|e| general(format!("cannot read {}: {e}", dir.display())))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "torrent"))
        .ok_or_else(|| general("aria2 did not save the torrent metadata".into()))?;
    let bytes = std::fs::read(&path).map_err(|e| general(format!("cannot read metadata: {e}")))?;
    let meta = dman_torrent::parse_torrent(&bytes)
        .map_err(|e| general(format!("the saved metadata is not a valid torrent: {e}")))?;
    Ok(MagnetPreview {
        info_hash: meta.info_hash,
        name: meta.name,
        total_bytes: meta.total,
        files: meta
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| FileInfo {
                index: u32::try_from(i + 1).unwrap_or(u32::MAX),
                path: f.path.clone(),
                length: f.length,
                completed: 0,
                selected: true,
            })
            .collect(),
        torrent_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Removes the aria2 download, its result and the private files. Runs from
/// [`Cleanup::finish`], or in the background when the preview was cancelled.
struct Cleanup {
    inner: Option<Inner>,
}

struct Inner {
    client: Aria2Client,
    gid: String,
    dir: PathBuf,
    registry: Arc<PreviewRegistry>,
}

impl Cleanup {
    fn new(client: Aria2Client, gid: String, dir: PathBuf, registry: Arc<PreviewRegistry>) -> Self {
        Self {
            inner: Some(Inner {
                client,
                gid,
                dir,
                registry,
            }),
        }
    }

    async fn finish(mut self) {
        if let Some(i) = self.inner.take() {
            i.run().await;
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(i) = self.inner.take()
            && let Ok(h) = tokio::runtime::Handle::try_current()
        {
            h.spawn(i.run());
        }
    }
}

impl Inner {
    async fn run(self) {
        let g = Gid::new(&self.gid);
        // Complete results are only dropped; running ones are stopped first.
        let _ = self.client.force_remove(&g).await;
        for _ in 0..30 {
            match self.client.tell_status(&g, &["status"]).await {
                Ok(st)
                    if matches!(
                        st.status,
                        Some(
                            DownloadStatus::Active
                                | DownloadStatus::Waiting
                                | DownloadStatus::Paused
                        )
                    ) => {}
                _ => break,
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = self.client.remove_download_result(&g).await;
        let dir = self.dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            // Only the files aria2 wrote into our private directory.
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_file()) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            let _ = std::fs::remove_dir(&dir);
        })
        .await;
        self.registry.set().remove(&self.gid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnets_are_checked() {
        assert!(
            check_magnet("magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef01").is_ok()
        );
        assert!(check_magnet("MAGNET:?XT=URN:BTIH:abc&dn=x").is_ok());
        assert!(check_magnet("http://x/a.torrent").is_err());
        assert!(check_magnet("magnet:?dn=x").is_err());
        assert!(check_magnet("magnet:?xt=urn:btih:a\nb").is_err());
        assert!(check_magnet("").is_err());
    }

    #[test]
    fn gids_are_sixteen_hex_digits_and_distinct() {
        let r = PreviewRegistry::new(PathBuf::from("/x/metadata"));
        let (a, da) = r.fresh();
        let (b, _) = r.fresh();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(da.starts_with("/x/metadata"));
        assert!(r.owns_dir("/x/metadata/abc"));
        assert!(!r.owns_dir("/x/metadata2/abc"));
        assert!(!r.owns_dir("/dl"));
    }
}
