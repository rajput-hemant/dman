//! Owns the `aria2c` process: config generation, spawn/attach, health
//! watching with restart/backoff, and clean shutdown.
//!
//! The RPC secret only ever lives in the `0600` conf file passed with
//! `--conf-path`; it is never put on the command line where `ps` shows it.

use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::client::Aria2Client;
use crate::error::Error;
use crate::types::Version;

/// Oldest aria2 minor version (1.x) dman supports.
const MIN_MINOR: u32 = 36;
const START_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const GRACEFUL_EXIT: Duration = Duration::from_secs(5);
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES: usize = 5;
const BACKOFF_MAX_SECS: u64 = 30;

/// How to run (or reach) aria2c.
#[derive(Debug, Clone)]
pub struct Aria2Settings {
    /// Binary to execute (default `aria2c`, looked up in `PATH`).
    pub binary: PathBuf,
    /// Loopback RPC port.
    pub rpc_port: u16,
    /// RPC secret (written to the conf file only).
    pub secret: SecretString,
    /// aria2 `save-session` / `input-file` file.
    pub session_file: PathBuf,
    /// Generated aria2 conf file (mode 0600).
    pub conf_file: PathBuf,
    /// Optional aria2 log file.
    pub log_file: Option<PathBuf>,
    /// Default download directory (created before start).
    pub download_dir: PathBuf,
    /// Extra command-line arguments appended after `--conf-path`.
    pub extra_args: Vec<String>,
    /// `bt-detach-seed-only`: seeding items do not count toward the
    /// concurrency limit.
    pub detach_seed_only: bool,
}

impl Aria2Settings {
    /// Settings with `binary = "aria2c"`, no log file, no extra args and
    /// `detach_seed_only = true`.
    pub fn new(
        rpc_port: u16,
        secret: SecretString,
        session_file: PathBuf,
        conf_file: PathBuf,
        download_dir: PathBuf,
    ) -> Self {
        Self {
            binary: PathBuf::from("aria2c"),
            rpc_port,
            secret,
            session_file,
            conf_file,
            log_file: None,
            download_dir,
            extra_args: Vec::new(),
            detach_seed_only: true,
        }
    }
}

/// Strip line breaks so a value can never inject extra conf lines.
fn clean(v: &str) -> String {
    v.chars().filter(|c| !matches!(c, '\n' | '\r')).collect()
}

fn render(s: &Aria2Settings, secret: &str) -> String {
    let mut o = String::new();
    let mut line = |k: &str, v: &str| {
        o.push_str(k);
        o.push('=');
        o.push_str(&clean(v));
        o.push('\n');
    };
    line("enable-rpc", "true");
    line("rpc-listen-all", "false");
    line("rpc-listen-port", &s.rpc_port.to_string());
    line("rpc-secret", secret);
    line("rpc-allow-origin-all", "false");
    line("rpc-save-upload-metadata", "true");
    line("save-session", &s.session_file.display().to_string());
    line("save-session-interval", "30");
    line("force-save", "false");
    if s.session_file.exists() {
        line("input-file", &s.session_file.display().to_string());
    }
    line("continue", "true");
    line("bt-save-metadata", "true");
    line("bt-detach-seed-only", &s.detach_seed_only.to_string());
    line("max-concurrent-downloads", "1000");
    line("dir", &s.download_dir.display().to_string());
    line("check-certificate", "true");
    line("summary-interval", "0");
    line("console-log-level", "warn");
    if let Some(l) = &s.log_file {
        line("log", &l.display().to_string());
        line("log-level", "notice");
    }
    o
}

/// Render the aria2 conf file. Contains the secret: keep it at mode 0600.
pub fn render_conf(s: &Aria2Settings) -> String {
    render(s, s.secret.expose_secret())
}

/// Like [`render_conf`] with the secret replaced by `<redacted>`; safe to
/// print (`dman daemon gen-config`).
pub fn render_conf_redacted(s: &Aria2Settings) -> String {
    render(s, "<redacted>")
}

/// Atomically write the conf file with mode 0600.
pub fn write_conf(s: &Aria2Settings) -> io::Result<()> {
    let tmp = {
        let mut n = s.conf_file.clone().into_os_string();
        n.push(".tmp");
        PathBuf::from(n)
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let res = (|| {
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // A pre-existing tmp file keeps its old mode otherwise.
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(render_conf(s).as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &s.conf_file)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Compiled-in aria2 features dman cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// BitTorrent support.
    pub bittorrent: bool,
    /// Metalink support.
    pub metalink: bool,
    /// SFTP support.
    pub sftp: bool,
    /// HTTPS support.
    pub https: bool,
    /// Asynchronous DNS.
    pub async_dns: bool,
    /// gzip content encoding.
    pub gzip: bool,
}

impl Capabilities {
    fn from_features(features: &[String]) -> Self {
        let has = |n: &str| features.iter().any(|f| f.eq_ignore_ascii_case(n));
        Self {
            bittorrent: has("BitTorrent"),
            metalink: has("Metalink"),
            sftp: has("SFTP"),
            https: has("HTTPS"),
            async_dns: has("Async DNS"),
            gzip: has("GZip"),
        }
    }
}

/// What `getVersion` told us about the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    /// Version string, e.g. `1.37.0`.
    pub version: String,
    /// Raw `enabledFeatures`.
    pub features: Vec<String>,
    /// Parsed capabilities.
    pub caps: Capabilities,
}

impl EngineInfo {
    fn from_version(v: Version) -> Self {
        let caps = Capabilities::from_features(&v.enabled_features);
        Self {
            version: v.version,
            features: v.enabled_features,
            caps,
        }
    }
}

/// Lifecycle state published by the [`Supervisor`].
#[derive(Debug, Clone)]
pub enum EngineState {
    /// First start in progress.
    Starting,
    /// Engine reachable.
    Running {
        /// Connected client.
        client: Aria2Client,
        /// Version and capabilities.
        info: EngineInfo,
        /// True if we attached to an engine we did not spawn.
        attached: bool,
    },
    /// Engine went away; waiting to restart.
    Restarting {
        /// 1-based consecutive failure count.
        attempt: u32,
        /// Why.
        reason: String,
    },
    /// Gave up (too many failures, or a fatal condition).
    Failed {
        /// Why.
        reason: String,
    },
    /// Shut down on request.
    Stopped,
}

/// Errors from [`Supervisor::start`].
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// The aria2c binary could not be found.
    #[error("aria2c not found (`{}`). {hint}", binary.display())]
    NotInstalled {
        /// Binary that was tried.
        binary: PathBuf,
        /// Install hint.
        hint: String,
    },
    /// Something that is not our aria2 listens on the RPC port.
    #[error("RPC port {port} is occupied: {detail}")]
    PortOccupied {
        /// The port.
        port: u16,
        /// What was found.
        detail: String,
    },
    /// aria2 older than 1.36.
    #[error("aria2 {found} is too old; dman needs 1.{MIN_MINOR} or newer")]
    TooOld {
        /// Reported version.
        found: String,
    },
    /// Filesystem trouble preparing directories or the conf file.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// aria2c started but did not become usable.
    #[error("aria2c failed to start: {0}")]
    StartFailed(String),
    /// An RPC call during startup failed.
    #[error("aria2 rpc error: {0}")]
    Rpc(#[from] Error),
}

impl SupervisorError {
    /// Errors that restarting cannot fix.
    fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::NotInstalled { .. } | Self::PortOccupied { .. } | Self::TooOld { .. }
        )
    }
}

const INSTALL_HINT: &str = "install aria2: `apt install aria2` / `brew install aria2` / `dnf install aria2`, or set the aria2 binary path in the config";

fn parse_minor(version: &str) -> Option<(u32, u32)> {
    let mut it = version.split('.');
    let lead = |s: &str| -> Option<u32> {
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    };
    Some((lead(it.next()?)?, lead(it.next()?)?))
}

fn check_version(v: &str) -> Result<(), SupervisorError> {
    match parse_minor(v) {
        Some((major, minor)) if major > 1 || (major == 1 && minor >= MIN_MINOR) => Ok(()),
        _ => Err(SupervisorError::TooOld {
            found: v.to_owned(),
        }),
    }
}

fn addr(port: u16) -> SocketAddr {
    ([127, 0, 0, 1], port).into()
}

async fn connect_and_version(s: &Aria2Settings) -> Result<(Aria2Client, Version), Error> {
    let client = Aria2Client::connect(addr(s.rpc_port), s.secret.clone()).await?;
    let v = client.get_version().await?;
    Ok((client, v))
}

/// An engine we are connected to, plus the child if we own it.
struct Live {
    client: Aria2Client,
    info: EngineInfo,
    child: Option<Child>,
}

fn pipe_logs(r: impl AsyncRead + Unpin + Send + 'static, stderr: bool) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if stderr {
                tracing::warn!(target: "aria2c", "{l}");
            } else {
                tracing::info!(target: "aria2c", "{l}");
            }
        }
    });
}

/// Attach to a running aria2 or spawn our own.
async fn acquire(s: &Aria2Settings, pid: &AtomicU32) -> Result<Live, SupervisorError> {
    let port = s.rpc_port;
    let listening = matches!(
        tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(addr(port))
        )
        .await,
        Ok(Ok(_))
    );
    if listening {
        let res = tokio::time::timeout(PROBE_TIMEOUT, connect_and_version(s)).await;
        return match res {
            Ok(Ok((client, v))) => {
                check_version(&v.version)?;
                Ok(Live {
                    client,
                    info: EngineInfo::from_version(v),
                    child: None,
                })
            }
            Ok(Err(Error::Rpc { message, .. })) if message.contains("Unauthorized") => {
                Err(SupervisorError::PortOccupied {
                    port,
                    detail: "an aria2 is listening but rejected our RPC secret".into(),
                })
            }
            Ok(Err(e)) => Err(SupervisorError::PortOccupied {
                port,
                detail: format!("something that is not our aria2 answers there ({e})"),
            }),
            Err(_) => Err(SupervisorError::PortOccupied {
                port,
                detail: "a service accepts connections but does not speak aria2 JSON-RPC".into(),
            }),
        };
    }

    for p in [&s.session_file, &s.conf_file] {
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
    }
    if let Some(parent) = s.log_file.as_deref().and_then(Path::parent) {
        std::fs::create_dir_all(parent)?;
    }
    // aria2 silently drops torrents whose dir is missing.
    std::fs::create_dir_all(&s.download_dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&s.session_file)?;
    write_conf(s)?;

    let mut child = Command::new(&s.binary)
        .arg(format!("--conf-path={}", s.conf_file.display()))
        .args(&s.extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                SupervisorError::NotInstalled {
                    binary: s.binary.clone(),
                    hint: INSTALL_HINT.into(),
                }
            } else {
                SupervisorError::StartFailed(format!("cannot spawn {}: {e}", s.binary.display()))
            }
        })?;
    if let Some(o) = child.stdout.take() {
        pipe_logs(o, false);
    }
    if let Some(e) = child.stderr.take() {
        pipe_logs(e, true);
    }
    pid.store(child.id().unwrap_or(0), Ordering::Relaxed);

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            pid.store(0, Ordering::Relaxed);
            return Err(SupervisorError::StartFailed(format!(
                "aria2c exited during startup ({status}); see its log"
            )));
        }
        if let Ok(Ok((client, v))) =
            tokio::time::timeout(Duration::from_secs(1), connect_and_version(s)).await
        {
            if let Err(e) = check_version(&v.version) {
                let _ = child.kill().await;
                pid.store(0, Ordering::Relaxed);
                return Err(e);
            }
            return Ok(Live {
                client,
                info: EngineInfo::from_version(v),
                child: Some(child),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill().await;
            pid.store(0, Ordering::Relaxed);
            return Err(SupervisorError::StartFailed(format!(
                "aria2c did not accept RPC connections within {}s",
                START_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct ShutdownCmd {
    stop_engine: bool,
    done: oneshot::Sender<()>,
}

async fn wait_child(child: &mut Option<Child>) -> String {
    match child {
        Some(c) => match c.wait().await {
            Ok(st) => format!("aria2c exited ({st})"),
            Err(e) => format!("waiting for aria2c failed: {e}"),
        },
        None => std::future::pending().await,
    }
}

async fn stop_live(live: Live, stop_engine: bool) {
    let owned = live.child.is_some();
    if !owned && !stop_engine {
        return;
    }
    let client = live.client.with_timeout(Duration::from_secs(5));
    let _ = client.save_session().await;
    let _ = client.shutdown().await;
    if let Some(mut child) = live.child
        && tokio::time::timeout(GRACEFUL_EXIT, child.wait())
            .await
            .is_err()
    {
        let _ = client.force_shutdown().await;
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
}

async fn run(
    settings: Arc<Aria2Settings>,
    tx: watch::Sender<EngineState>,
    mut ctl: mpsc::Receiver<ShutdownCmd>,
    pid: Arc<AtomicU32>,
    first: Live,
) {
    let mut live = first;
    let mut failures: VecDeque<Instant> = VecDeque::new();
    'outer: loop {
        tx.send_replace(EngineState::Running {
            client: live.client.clone(),
            info: live.info.clone(),
            attached: live.child.is_none(),
        });
        let mut reason = {
            let Live { client, child, .. } = &mut live;
            tokio::select! {
                cmd = ctl.recv() => {
                    pid.store(0, Ordering::Relaxed);
                    let stop = cmd.as_ref().is_none_or(|c| c.stop_engine);
                    stop_live(live, stop).await;
                    tx.send_replace(EngineState::Stopped);
                    if let Some(c) = cmd {
                        let _ = c.done.send(());
                    }
                    return;
                }
                r = wait_child(child) => r,
                () = client.closed() => "connection to aria2 lost".to_owned(),
            }
        };
        if let Some(mut c) = live.child.take() {
            let _ = c.kill().await;
        }
        pid.store(0, Ordering::Relaxed);

        loop {
            let now = Instant::now();
            failures.push_back(now);
            while failures
                .front()
                .is_some_and(|t| now.duration_since(*t) > FAILURE_WINDOW)
            {
                failures.pop_front();
            }
            if failures.len() >= MAX_FAILURES {
                tx.send_replace(EngineState::Failed {
                    reason: format!(
                        "{} failures within a minute; last: {reason}",
                        failures.len()
                    ),
                });
                if let Some(c) = ctl.recv().await {
                    let _ = c.done.send(());
                }
                return;
            }
            let attempt = failures.len() as u32;
            let delay = (1u64 << (attempt - 1)).min(BACKOFF_MAX_SECS);
            tracing::warn!("aria2 down ({reason}); restart attempt {attempt} in {delay}s");
            tx.send_replace(EngineState::Restarting {
                attempt,
                reason: reason.clone(),
            });
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(delay)) => {}
                cmd = ctl.recv() => {
                    tx.send_replace(EngineState::Stopped);
                    if let Some(c) = cmd {
                        let _ = c.done.send(());
                    }
                    return;
                }
            }
            match acquire(&settings, &pid).await {
                Ok(l) => {
                    live = l;
                    continue 'outer;
                }
                Err(e) if e.is_fatal() => {
                    tx.send_replace(EngineState::Failed {
                        reason: e.to_string(),
                    });
                    if let Some(c) = ctl.recv().await {
                        let _ = c.done.send(());
                    }
                    return;
                }
                Err(e) => reason = e.to_string(),
            }
        }
    }
}

/// Owner of the aria2 engine (spawned by us, or attached to).
#[derive(Debug)]
pub struct Supervisor {
    state: watch::Receiver<EngineState>,
    ctl: mpsc::Sender<ShutdownCmd>,
    pid: Arc<AtomicU32>,
    task: JoinHandle<()>,
}

impl Supervisor {
    /// Attach to an engine already on the port, or spawn aria2c, and start
    /// watching it. Fails fast on a missing binary, an occupied port or a
    /// too-old version.
    pub async fn start(settings: Aria2Settings) -> Result<Supervisor, SupervisorError> {
        let settings = Arc::new(settings);
        let pid = Arc::new(AtomicU32::new(0));
        let (tx, state) = watch::channel(EngineState::Starting);
        let first = acquire(&settings, &pid).await?;
        let (ctl, ctl_rx) = mpsc::channel(1);
        let task = tokio::spawn(run(settings, tx, ctl_rx, pid.clone(), first));
        let mut state = state;
        // The task publishes Running immediately; wait so callers see it.
        let _ = state
            .wait_for(|s| !matches!(s, EngineState::Starting))
            .await;
        Ok(Supervisor {
            state,
            ctl,
            pid,
            task,
        })
    }

    /// Receiver for state changes.
    pub fn state(&self) -> watch::Receiver<EngineState> {
        self.state.clone()
    }

    /// Current client, if the engine is running.
    pub fn client(&self) -> Option<Aria2Client> {
        match &*self.state.borrow() {
            EngineState::Running { client, .. } => Some(client.clone()),
            _ => None,
        }
    }

    /// Current engine info, if running.
    pub fn info(&self) -> Option<EngineInfo> {
        match &*self.state.borrow() {
            EngineState::Running { info, .. } => Some(info.clone()),
            _ => None,
        }
    }

    /// Whether the running engine was attached rather than spawned by us.
    pub fn is_attached(&self) -> bool {
        matches!(
            &*self.state.borrow(),
            EngineState::Running { attached: true, .. }
        )
    }

    /// PID of the aria2c we spawned (None when attached or not running).
    pub fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Relaxed) {
            0 => None,
            p => Some(p),
        }
    }

    /// Stop supervising. An engine we spawned is always stopped (save
    /// session, `shutdown`, wait 5 s, `forceShutdown`, kill). An attached
    /// engine is only shut down when `stop_engine` is true.
    pub async fn shutdown(self, stop_engine: bool) {
        let (done, rx) = oneshot::channel();
        if self
            .ctl
            .send(ShutdownCmd { stop_engine, done })
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // Dropping the task drops the Child, and kill_on_drop reaps it.
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dir: &Path) -> Aria2Settings {
        Aria2Settings::new(
            6800,
            SecretString::from("sekrit"),
            dir.join("session"),
            dir.join("aria2.conf"),
            dir.join("dl"),
        )
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dman-sup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn conf_content() {
        let d = tmp("conf");
        let mut s = settings(&d);
        let c = render_conf(&s);
        for want in [
            "enable-rpc=true",
            "rpc-listen-all=false",
            "rpc-listen-port=6800",
            "rpc-secret=sekrit",
            "rpc-allow-origin-all=false",
            "rpc-save-upload-metadata=true",
            "save-session-interval=30",
            "force-save=false",
            "continue=true",
            "bt-save-metadata=true",
            "bt-detach-seed-only=true",
            "max-concurrent-downloads=1000",
            "check-certificate=true",
            "summary-interval=0",
            "console-log-level=warn",
        ] {
            assert!(c.lines().any(|l| l == want), "missing {want}\n{c}");
        }
        assert!(c.contains(&format!("save-session={}/session", d.display())));
        assert!(c.contains(&format!("dir={}/dl", d.display())));
        // No session file yet, no log file.
        assert!(!c.contains("input-file="));
        assert!(!c.contains("log="));
        std::fs::write(&s.session_file, "").unwrap();
        s.log_file = Some(d.join("a.log"));
        s.detach_seed_only = false;
        let c = render_conf(&s);
        assert!(c.contains(&format!("input-file={}/session", d.display())));
        assert!(c.contains(&format!("log={}/a.log", d.display())));
        assert!(c.contains("log-level=notice"));
        assert!(c.contains("bt-detach-seed-only=false"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn redacted_has_no_secret() {
        let d = tmp("redact");
        let c = render_conf_redacted(&settings(&d));
        assert!(!c.contains("sekrit"));
        assert!(c.contains("rpc-secret=<redacted>"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn conf_values_cannot_inject_lines() {
        let d = tmp("inject");
        let mut s = settings(&d);
        s.download_dir = PathBuf::from("/x\nrpc-secret=evil");
        let c = render_conf(&s);
        assert_eq!(
            c.lines().filter(|l| l.starts_with("rpc-secret=")).count(),
            1
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn write_conf_is_0600_and_atomic() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("write");
        let s = settings(&d);
        // A stale, world-readable tmp file must not leak the secret mode.
        std::fs::write(d.join("aria2.conf.tmp"), "old").unwrap();
        std::fs::set_permissions(
            d.join("aria2.conf.tmp"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        write_conf(&s).unwrap();
        let mode = std::fs::metadata(&s.conf_file)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            std::fs::read_to_string(&s.conf_file)
                .unwrap()
                .contains("rpc-secret=sekrit")
        );
        assert!(!d.join("aria2.conf.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn version_check() {
        assert!(check_version("1.37.0").is_ok());
        assert!(check_version("1.36.0").is_ok());
        assert!(check_version("1.37.0-dev").is_ok());
        assert!(check_version("2.0.0").is_ok());
        assert!(matches!(
            check_version("1.35.0"),
            Err(SupervisorError::TooOld { .. })
        ));
        assert!(matches!(
            check_version("garbage"),
            Err(SupervisorError::TooOld { .. })
        ));
    }

    #[test]
    fn capabilities_from_features() {
        let f: Vec<String> = [
            "Async DNS",
            "BitTorrent",
            "GZip",
            "HTTPS",
            "Metalink",
            "SFTP",
        ]
        .map(String::from)
        .to_vec();
        let c = Capabilities::from_features(&f);
        assert!(c.bittorrent && c.metalink && c.sftp && c.https && c.async_dns && c.gzip);
        assert_eq!(Capabilities::from_features(&[]), Capabilities::default());
    }

    #[tokio::test]
    async fn bogus_binary_is_not_installed() {
        let d = tmp("bogus");
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut s = settings(&d);
        s.rpc_port = port;
        s.binary = PathBuf::from("/nonexistent/definitely-not-aria2c");
        let err = Supervisor::start(s).await.expect_err("must fail");
        match err {
            SupervisorError::NotInstalled { hint, .. } => {
                assert!(hint.contains("apt install aria2"))
            }
            other => panic!("unexpected {other:?}"),
        }
        // Directories were still prepared.
        assert!(d.join("dl").is_dir());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn foreign_listener_is_port_occupied() {
        let d = tmp("occupied");
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                drop(s);
            }
        });
        let mut s = settings(&d);
        s.rpc_port = port;
        s.binary = PathBuf::from("/nonexistent/definitely-not-aria2c");
        let err = Supervisor::start(s).await.expect_err("must fail");
        match err {
            SupervisorError::PortOccupied { port: p, .. } => assert_eq!(p, port),
            other => panic!("unexpected {other:?}"),
        }
        // No conf was written: we never tried to start a second engine.
        assert!(!d.join("aria2.conf").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
