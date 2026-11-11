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
//! An http(s) URL of a `.torrent` file is fetched the same way (with
//! `follow-torrent=false`, so aria2 does not start the torrent), for the
//! preview and for `jobs.add`, which then adds the file as a torrent.
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
use dman_proto::{ErrorCode, FileInfo, JobOptions, RpcError};
use tokio::sync::{Semaphore, watch};

/// Default and maximum wait for the metadata.
pub const DEFAULT_TIMEOUT_SECS: u32 = 120;
pub const MAX_TIMEOUT_SECS: u32 = 600;
/// Previews that may run at the same time.
const MAX_PARALLEL: usize = 4;
const POLL: Duration = Duration::from_millis(200);
/// Default wait for a `.torrent` file fetched over http(s).
pub const TORRENT_FETCH_SECS: u32 = 30;
/// Larger downloads are not `.torrent` files (real ones stay far below).
const MAX_TORRENT_BYTES: u64 = 32 << 20;

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
        if dman_proto::is_torrent_url(&p.uri) {
            let secs = match p.timeout_secs {
                None => TORRENT_FETCH_SECS,
                Some(0) => return Err(RpcError::invalid("timeout_secs must be at least 1")),
                Some(n) => n.min(MAX_TIMEOUT_SECS),
            };
            let (_, bytes) = self
                .fetch_torrent(&p.uri, &JobOptions::default(), secs)
                .await?;
            return preview_of(&bytes);
        }
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
        let result = self
            .wait(
                &client,
                &gid,
                secs,
                "the magnet's metadata",
                " (no peers found?)",
            )
            .await;
        let result = match result {
            Ok(()) => {
                let dir = dir.clone();
                tokio::task::spawn_blocking(move || read_preview(&dir))
                    .await
                    .map_err(|e| err(ErrorCode::General, e.to_string()))
                    .and_then(|r| r)
            }
            Err(e) => Err(e),
        };
        cleanup.finish().await;
        result
    }

    /// Fetches the `.torrent` file at an http(s) URL through aria2 (so its
    /// proxy and network settings apply, plus the headers, user agent,
    /// referer and proxy of `opts`) into the private directory, without
    /// starting the torrent. Returns a file name ending in `.torrent` and the
    /// bytes, which are checked to be a valid torrent.
    pub async fn fetch_torrent(
        &self,
        url: &str,
        opts: &JobOptions,
        secs: u32,
    ) -> Result<(String, Vec<u8>), RpcError> {
        let url = url.trim();
        if url.contains(['\n', '\r', '\0']) {
            return Err(RpcError::invalid("the URL contains a line break"));
        }
        let client = self.client()?;
        let _slot = self
            .slots
            .try_acquire()
            .map_err(|_| RpcError::refused("too many previews in progress"))?;
        let (gid, dir) = self.registry.fresh();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            err(
                ErrorCode::General,
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
        self.registry.set().insert(gid.clone());
        let cleanup = Cleanup::new(
            client.clone(),
            gid.clone(),
            dir.clone(),
            self.registry.clone(),
        );
        // Only the request options of the job matter for fetching the file.
        let request = JobOptions {
            headers: opts.headers.clone(),
            user_agent: opts.user_agent.clone(),
            referer: opts.referer.clone(),
            proxy: opts.proxy.clone(),
            ..JobOptions::default()
        };
        let mut aopts = match dman_aria2::job_options_to_aria2(&request) {
            Ok(o) => o,
            Err(e) => {
                cleanup.finish().await;
                return Err(RpcError::invalid(e.to_string()));
            }
        };
        aopts.set("dir", dir.display().to_string());
        aopts.set("gid", gid.clone());
        aopts.set("follow-torrent", "false");
        aopts.set("follow-metalink", "false");
        aopts.set("split", "1");
        if let Err(e) = client.add_uri(&[url.to_owned()], &aopts, None).await {
            cleanup.finish().await;
            return Err(match e {
                dman_aria2::Error::Rpc { message, .. } => {
                    RpcError::invalid(format!("aria2 rejected the URL: {message}"))
                }
                other => err(
                    ErrorCode::EngineUnavailable,
                    format!("aria2 unavailable: {other}"),
                ),
            });
        }
        let result = self
            .wait(&client, &gid, secs, "the .torrent file", "")
            .await;
        let result = match result {
            Ok(()) => {
                let dir = dir.clone();
                tokio::task::spawn_blocking(move || read_fetched(&dir))
                    .await
                    .map_err(|e| err(ErrorCode::General, e.to_string()))
                    .and_then(|r| r)
            }
            Err(e) => Err(e),
        };
        cleanup.finish().await;
        result
    }

    /// Waits until the aria2 download `gid` completes. `what` names what is
    /// fetched in errors; `hint` is appended to a timeout.
    async fn wait(
        &self,
        client: &Aria2Client,
        gid: &str,
        secs: u32,
        what: &str,
        hint: &str,
    ) -> Result<(), RpcError> {
        let g = Gid::new(gid);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(secs));
        loop {
            match client
                .tell_status(&g, &["status", "errorCode", "errorMessage", "totalLength"])
                .await
            {
                Ok(st) => match st.status {
                    Some(DownloadStatus::Complete) => return Ok(()),
                    Some(DownloadStatus::Error) => {
                        return Err(err(
                            ErrorCode::General,
                            format!(
                                "could not fetch {what}: {}",
                                st.error_message.unwrap_or_else(|| "unknown error".into())
                            ),
                        ));
                    }
                    Some(DownloadStatus::Removed) => {
                        return Err(err(
                            ErrorCode::General,
                            format!("the download of {what} was removed"),
                        ));
                    }
                    _ if st.total_length.is_some_and(|n| n > MAX_TORRENT_BYTES) => {
                        return Err(err(
                            ErrorCode::Refused,
                            format!(
                                "{what} is larger than {} MiB; not a torrent?",
                                MAX_TORRENT_BYTES >> 20
                            ),
                        ));
                    }
                    _ => {}
                },
                Err(dman_aria2::Error::Rpc { message, .. }) => {
                    return Err(err(
                        ErrorCode::General,
                        format!("could not fetch {what}: {message}"),
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
                    format!("timed out after {secs} s waiting for {what}{hint}"),
                ));
            }
            tokio::time::sleep(POLL).await;
        }
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
    dman_torrent::parse_torrent(&bytes)
        .map_err(|e| general(format!("the saved metadata is not a valid torrent: {e}")))?;
    preview_of(&bytes)
}

/// The file aria2 fetched into `dir` (the only regular file that is not a
/// control file), checked to be a torrent; its name always ends in `.torrent`.
fn read_fetched(dir: &Path) -> Result<(String, Vec<u8>), RpcError> {
    let general = |m: String| err(ErrorCode::General, m);
    let path = std::fs::read_dir(dir)
        .map_err(|e| general(format!("cannot read {}: {e}", dir.display())))?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .find(|p| p.extension().is_none_or(|x| x != "aria2"))
        .ok_or_else(|| general("aria2 did not save the .torrent file".into()))?;
    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if len > MAX_TORRENT_BYTES {
        return Err(err(
            ErrorCode::Refused,
            "the .torrent file is too large; not a torrent?",
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| general(format!("cannot read it: {e}")))?;
    dman_torrent::parse_torrent(&bytes).map_err(|e| {
        RpcError::invalid(format!(
            "the URL did not return a valid .torrent file ({e})"
        ))
    })?;
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem.strip_suffix(".torrent").unwrap_or(&stem);
    let name = if stem.is_empty() {
        "download.torrent".to_owned()
    } else {
        format!("{stem}.torrent")
    };
    Ok((name, bytes))
}

/// The preview of a `.torrent` file.
fn preview_of(bytes: &[u8]) -> Result<MagnetPreview, RpcError> {
    let meta = dman_torrent::parse_torrent(bytes)
        .map_err(|e| err(ErrorCode::General, format!("not a valid torrent: {e}")))?;
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
        torrent_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
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
