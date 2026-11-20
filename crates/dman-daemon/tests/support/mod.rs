//! Helpers shared by the daemon integration tests.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::{Event, ListParams};
use dman_proto::{AddOutcome, Job, JobId, JobState};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use url::Url;

/// A local TCP port nobody listens on, for a daemon or aria2 about to start.
///
/// Binding to port 0 and dropping the listener can hand the same port to two
/// callers (or to a parallel test binary) before the daemon binds it. Ports
/// are therefore taken from a window derived from the process id, never
/// handed out twice by one process, and skipped while something listens.
pub fn free_port() -> u16 {
    static GIVEN: Mutex<Option<HashSet<u16>>> = Mutex::new(None);
    const WINDOW: u32 = 256;
    let base = 10_000 + (std::process::id() % 190) * WINDOW;
    let mut given = GIVEN.lock().unwrap_or_else(|e| e.into_inner());
    let given = given.get_or_insert_with(HashSet::new);
    for i in 0..WINDOW {
        let port = u16::try_from(base + i).unwrap();
        if !given.contains(&port) && TcpListener::bind(("127.0.0.1", port)).is_ok() {
            given.insert(port);
            return port;
        }
    }
    // Window exhausted: fall back to an ephemeral port.
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Whether the tests that need a real `aria2c` should run.
pub fn aria2_enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

/// A tiny HTTP server for `files` (path without the leading `/` -> bytes)
/// that honours `Range`, like the servers aria2 resumes from.
pub async fn serve_files(files: HashMap<String, Vec<u8>>) -> SocketAddr {
    let files = Arc::new(files);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(handle_http(s, files.clone()));
        }
    });
    addr
}

async fn handle_http(mut s: TcpStream, files: Arc<HashMap<String, Vec<u8>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().trim_start_matches('/');
    let range = lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("range")
            .then(|| v.trim().to_owned())
    });
    let Some(data) = files.get(path) else {
        let _ = s
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };
    let len = data.len();
    let (status, start, end) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap_or((r, ""));
            let a: usize = a.parse().unwrap_or(0);
            let b: usize = b.parse().unwrap_or(len - 1).min(len - 1);
            ("206 Partial Content", a, b)
        }
        None => ("200 OK", 0, len - 1),
    };
    let body = &data[start..=end];
    let ctype = if path.ends_with(".mp4") {
        "video/mp4"
    } else {
        "application/octet-stream"
    };
    let mut h = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: {ctype}\r\nConnection: close\r\n",
        body.len()
    );
    if status.starts_with("206") {
        h.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
    }
    h.push_str("\r\n");
    if s.write_all(h.as_bytes()).await.is_err() {
        return;
    }
    if method != "HEAD" {
        for c in body.chunks(16 * 1024) {
            if s.write_all(c).await.is_err() {
                return;
            }
        }
    }
    let _ = s.shutdown().await;
}

/// `len` deterministic bytes; different `seed`s give different files.
pub fn content(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// A daemon running in-process on `run`, stopped with [`Daemon::stop`].
pub struct Daemon {
    pub paths: Paths,
    pub url: Url,
    pub task: JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    /// Starts the daemon and waits until it accepts authenticated connections.
    pub async fn start(root: &Path, config: Config) -> Self {
        let paths = Paths::with_root(root);
        let DaemonTarget::Local { listen, .. } = &config.profiles["local"].daemon else {
            unreachable!()
        };
        let url = Url::parse(&format!("ws://{listen}/api/v1/ws")).unwrap();
        let task = tokio::spawn(run(DaemonOptions {
            paths: paths.clone(),
            config,
            profile: "local".into(),
        }));
        let d = Self { paths, url, task };
        // Wait until it accepts authenticated connections.
        for _ in 0..200 {
            if d.task.is_finished() {
                break;
            }
            if d.try_connect().await.is_ok() {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let r = d.task.await;
        panic!("daemon did not come up: {r:?}");
    }

    pub fn token(&self) -> SecretString {
        Secrets::load(&self.paths)
            .ok()
            .and_then(|s| s.get("daemon"))
            .unwrap_or_else(|| SecretString::from(String::new()))
    }

    pub async fn try_connect(&self) -> Result<Client, ClientError> {
        Client::connect(&self.url, self.token(), "dman-test").await
    }

    pub async fn client(&self) -> Client {
        self.try_connect().await.expect("connect")
    }

    pub async fn stop(self, client: &Client) {
        client.shutdown().await.expect("shutdown call");
        tokio::time::timeout(Duration::from_secs(30), self.task)
            .await
            .expect("daemon did not stop")
            .expect("join")
            .expect("daemon result");
    }
}

/// Polls `jobs.list` until `f` holds, panicking with `what` after `secs`.
pub async fn wait_jobs(c: &Client, secs: u64, what: &str, f: impl Fn(&[Job]) -> bool) -> Vec<Job> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let jobs = c.list(ListParams::default()).await.expect("list");
        if f(&jobs) {
            return jobs;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for {what}: {jobs:#?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The ids of the added jobs; panics on any failed source.
pub fn added(outcomes: &[AddOutcome]) -> Vec<JobId> {
    outcomes
        .iter()
        .map(|o| match o {
            AddOutcome::Added { id, .. } => *id,
            AddOutcome::Failed { error } => panic!("add failed: {error}"),
        })
        .collect()
}

/// The job `id` of a listing; panics when it is missing.
pub fn job(jobs: &[Job], id: JobId) -> &Job {
    jobs.iter()
        .find(|j| j.id == id)
        .unwrap_or_else(|| panic!("job {id} missing"))
}

/// How many of `jobs` are in state `s`.
pub fn count(jobs: &[Job], s: JobState) -> usize {
    jobs.iter().filter(|j| j.state == s).count()
}

/// Live (non-zombie) aria2c processes whose command line mentions `root`.
pub fn aria2_pids(root: &Path) -> Vec<u32> {
    let needle = root.display().to_string();
    let mut out = Vec::new();
    let Ok(proc_dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in proc_dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let cmd: PathBuf = entry.path().join("cmdline");
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        if let Ok(c) = std::fs::read(&cmd) {
            let c = String::from_utf8_lossy(&c);
            if c.contains("aria2c") && c.contains(&needle) && !stat.contains(") Z ") {
                out.push(pid);
            }
        }
    }
    out
}

pub fn assert_no_aria2(root: &Path) {
    let pids = aria2_pids(root);
    assert!(pids.is_empty(), "aria2c still running: {pids:?}");
}

/// Drains events until `f` matches one, or the timeout passes.
pub async fn wait_event(
    rx: &mut broadcast::Receiver<Event>,
    secs: u64,
    f: impl Fn(&Event) -> bool,
) -> Option<Event> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(e)) if f(&e) => return Some(e),
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(_)) | Err(_) => return None,
        }
    }
}
