//! End-to-end tests: the daemon runs in-process, a local HTTP server provides
//! downloads, and `dman_client` talks to the API.
//!
//! The aria2 test needs a real `aria2c` and runs with
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::EngineStatus;
use dman_proto::method::{Event, Level, LimitsUpdate, ListParams, RemoveParams};
use dman_proto::{AddOutcome, AddParams, AddSource, ErrorCode, Job, JobId, JobOptions, JobState};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use url::Url;

fn aria2_enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// ----- a tiny Range-capable HTTP file server ----------------------------------

async fn serve_files(files: HashMap<String, Vec<u8>>) -> SocketAddr {
    let files = Arc::new(files);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    let mut h = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
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

fn content(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

// ----- daemon harness ---------------------------------------------------------

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
}

fn config(root: &Path, aria2_path: &str, min_free: DiskThreshold) -> Config {
    let mut c = Config::default();
    let p = c.profiles.get_mut("local").unwrap();
    p.daemon = DaemonTarget::Local {
        listen: format!("127.0.0.1:{}", free_port()),
        secret_ref: "daemon".into(),
    };
    p.download_dir = root.join("dl").display().to_string();
    p.min_free = min_free;
    p.warn_free = DiskThreshold::Absolute(0);
    c.aria2.path = aria2_path.into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c.limits.max_concurrent = 1;
    c
}

impl Daemon {
    async fn start(root: &Path, config: Config) -> Self {
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

    fn token(&self) -> SecretString {
        Secrets::load(&self.paths)
            .ok()
            .and_then(|s| s.get("daemon"))
            .unwrap_or_else(|| SecretString::from(String::new()))
    }

    async fn try_connect(&self) -> Result<Client, ClientError> {
        Client::connect(&self.url, self.token(), "dman-test").await
    }

    async fn client(&self) -> Client {
        self.try_connect().await.expect("connect")
    }

    async fn stop(self, client: &Client) {
        client.shutdown().await.expect("shutdown call");
        tokio::time::timeout(Duration::from_secs(30), self.task)
            .await
            .expect("daemon did not stop")
            .expect("join")
            .expect("daemon result");
    }
}

async fn wait_jobs(c: &Client, secs: u64, what: &str, f: impl Fn(&[Job]) -> bool) -> Vec<Job> {
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

fn job(jobs: &[Job], id: JobId) -> &Job {
    jobs.iter()
        .find(|j| j.id == id)
        .unwrap_or_else(|| panic!("job {id} missing"))
}

fn count(jobs: &[Job], s: JobState) -> usize {
    jobs.iter().filter(|j| j.state == s).count()
}

fn added(outcomes: &[AddOutcome]) -> Vec<JobId> {
    outcomes
        .iter()
        .map(|o| match o {
            AddOutcome::Added { id, .. } => *id,
            AddOutcome::Failed { error } => panic!("add failed: {error}"),
        })
        .collect()
}

/// Drains events until `f` matches one, or the timeout passes.
async fn wait_event(
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

// ----- tests ------------------------------------------------------------------

/// No aria2 needed: a missing engine degrades gracefully, auth is enforced, and
/// only one daemon per profile can run.
#[tokio::test(flavor = "multi_thread")]
async fn serves_without_engine_and_enforces_auth_and_lock() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        DiskThreshold::Absolute(0),
    );
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    assert_eq!(c.profile(), "local");

    let status = c.engine_status().await.unwrap();
    // aria2 first, then the yt-dlp video engine.
    assert_eq!(status.len(), 2);
    assert_eq!(status[0].engine, dman_proto::Engine::Aria2);
    assert_eq!(status[1].engine, dman_proto::Engine::YtDlp);
    assert!(!status[0].available);
    assert!(
        status[0]
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("not found"),
        "{status:?}"
    );

    let err = c
        .add(AddParams {
            sources: vec![AddSource::Uris {
                uris: vec!["http://127.0.0.1:1/x".into()],
            }],
            options: JobOptions::default(),
            paused: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::EngineUnavailable),
        "{err:?}"
    );
    assert!(c.list(ListParams::default()).await.unwrap().is_empty());
    let limits = c.limits_get().await.unwrap();
    assert_eq!(limits.max_concurrent, 1);
    let stats = c.stats().await.unwrap();
    assert!(stats.disk_free.is_some());

    // Wrong token.
    let bad = Client::connect(&d.url, SecretString::from("wrong".to_owned()), "t").await;
    assert!(matches!(bad, Err(ClientError::Unauthorized)), "{bad:?}");

    // A second daemon for the same profile refuses to start.
    let second = run(DaemonOptions {
        paths: Paths::with_root(root.path()),
        config: cfg,
        profile: "local".into(),
    })
    .await
    .unwrap_err();
    assert!(second.to_string().contains("already running"), "{second:#}");

    d.stop(&c).await;
}

/// `fs.list` over the API (no aria2 needed): default = profile download dir.
#[tokio::test(flavor = "multi_thread")]
async fn fs_list_over_the_api() {
    use dman_proto::method::FsListParams;
    let root = tempfile::tempdir().unwrap();
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        DiskThreshold::Absolute(0),
    );
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    // The profile's download dir does not exist yet: nearest ancestor is listed.
    let def = c.fs_list(FsListParams::default()).await.unwrap();
    assert!(!def.path.is_empty());
    std::fs::create_dir_all(root.path().join("dl/sub")).unwrap();
    std::fs::write(root.path().join("dl/file.bin"), b"abc").unwrap();
    let def = c.fs_list(FsListParams::default()).await.unwrap();
    assert!(def.path.ends_with("dl"), "{}", def.path);
    let names: Vec<_> = def.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["sub", "file.bin"]);
    let dirs = c
        .fs_list(FsListParams {
            path: Some(def.path.clone()),
            dirs_only: true,
        })
        .await
        .unwrap();
    assert_eq!(dirs.entries.len(), 1);
    assert!(dirs.parent.is_some());
    let err = c
        .fs_list(FsListParams {
            path: Some(format!("{}/missing", def.path)),
            dirs_only: true,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );
    d.stop(&c).await;
}

/// `engine.restart` with a binary that does not exist: EngineUnavailable, and
/// the daemon keeps serving (also detail/history on what it has).
#[tokio::test(flavor = "multi_thread")]
async fn engine_restart_without_binary_keeps_serving() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        DiskThreshold::Absolute(0),
    );
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    for _ in 0..2 {
        let err = c.engine_restart().await.unwrap_err();
        assert!(
            matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::EngineUnavailable),
            "{err:?}"
        );
        let status = c.engine_status().await.unwrap();
        assert!(!status[0].available, "{status:?}");
        assert!(c.list(ListParams::default()).await.unwrap().is_empty());
    }
    // The global history keeps filling even without an engine.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!c.stats_history().await.unwrap().is_empty());
    let err = c.detail(JobId(1)).await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );
    d.stop(&c).await;
}

/// Full lifecycle against a real aria2c.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn lifecycle_with_real_aria2() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    const SIZE: usize = 300 * 1024;
    let files: HashMap<String, Vec<u8>> = ["a", "b", "c", "d"]
        .iter()
        .enumerate()
        .map(|(i, n)| (format!("{n}.bin"), content(i as u8, SIZE)))
        .collect();
    let http = serve_files(files.clone()).await;
    let uri = |n: &str| format!("http://{http}/{n}.bin");
    let root = tempfile::tempdir().unwrap();
    let dl = root.path().join("dl");
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));

    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let status = c.engine_status().await.unwrap();
    assert!(status[0].available, "{status:?}");
    let mut events = c.subscribe().await.unwrap();
    let first = wait_event(&mut events, 5, |_| true).await;
    assert!(matches!(first, Some(Event::Snapshot { .. })), "{first:?}");

    // Three slow downloads with max_concurrent = 1.
    let out = c
        .add(AddParams {
            sources: ["a", "b", "c"]
                .iter()
                .map(|n| AddSource::Uris { uris: vec![uri(n)] })
                .collect(),
            options: JobOptions {
                max_download_limit: Some(50 * 1024),
                ..Default::default()
            },
            paused: false,
        })
        .await
        .unwrap();
    let ids = added(&out);
    let (a, b, cc) = (ids[0], ids[1], ids[2]);
    let jobs = wait_jobs(&c, 10, "one active", |j| {
        count(j, JobState::Active) == 1 && count(j, JobState::Waiting) == 2
    })
    .await;
    assert_eq!(job(&jobs, a).state, JobState::Active);
    assert_eq!(job(&jobs, b).queue_position, Some(0));
    assert_eq!(job(&jobs, cc).queue_position, Some(1));
    assert_eq!(job(&jobs, a).name, "a.bin");
    // It stays that way over several ticks.
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let j = c.list(ListParams::default()).await.unwrap();
        assert!(count(&j, JobState::Active) <= 1, "{j:#?}");
    }

    // Detail of a running HTTP job: one file, no peers, options and history.
    let det = c.detail(a).await.unwrap();
    assert_eq!(det.job.id, a);
    assert_eq!(det.files.len(), 1, "{:?}", det.files);
    assert_eq!(det.files[0].index, 1);
    assert!(det.files[0].path.ends_with("a.bin"), "{:?}", det.files);
    assert_eq!(det.files[0].length, SIZE as u64);
    assert!(det.peers.is_empty() && det.trackers.is_empty());
    assert_eq!(det.options.max_download_limit, Some(50 * 1024));
    assert!(!det.speed_history.is_empty(), "no per-job speed history");
    assert!(det.speed_history.windows(2).all(|w| w[0].t <= w[1].t));
    let err = c.detail(JobId(9999)).await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );
    let hist = c.stats_history().await.unwrap();
    assert!(hist.len() >= 3, "global history too short: {hist:?}");
    assert!(hist.iter().any(|s| s.down > 0), "{hist:?}");

    // Options that need a paused download are refused while it runs.
    let err = c
        .set_options(
            a,
            JobOptions {
                out: Some("renamed.bin".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::Refused),
        "{err:?}"
    );

    // Pause the active job: the next one starts.
    let r = c.pause(&[a]).await.unwrap();
    assert_eq!(r.ok, vec![a]);
    wait_jobs(&c, 10, "b active after pausing a", |j| {
        job(j, a).state == JobState::Paused && job(j, b).state == JobState::Active
    })
    .await;
    // Resume: back in the queue, ahead of c.
    c.resume(&[a]).await.unwrap();
    let jobs = wait_jobs(&c, 5, "a waiting", |j| job(j, a).state == JobState::Waiting).await;
    assert_eq!(job(&jobs, a).queue_position, Some(0));
    assert_eq!(count(&jobs, JobState::Active), 1);

    // Let everything finish quickly.
    for id in [a, b, cc] {
        c.set_options(
            id,
            JobOptions {
                max_download_limit: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    c.limits_set(LimitsUpdate {
        max_concurrent: Some(3),
        ..Default::default()
    })
    .await
    .unwrap();
    let jobs = wait_jobs(&c, 30, "all complete", |j| {
        count(j, JobState::Complete) == 3
    })
    .await;
    for j in &jobs {
        assert_eq!(j.completed_bytes, SIZE as u64);
        assert!(j.completed_at.is_some());
    }
    let ev = wait_event(&mut events, 5, |e| {
        matches!(e, Event::JobsUpdated { jobs } if jobs.iter().any(|j| j.state == JobState::Complete))
    })
    .await;
    assert!(ev.is_some(), "no completion event received");
    for n in ["a", "b", "c"] {
        let got = std::fs::read(dl.join(format!("{n}.bin"))).unwrap();
        assert_eq!(got, files[&format!("{n}.bin")], "{n}");
    }
    let stats = c.stats().await.unwrap();
    assert!(stats.total_downloaded > 0, "{stats:?}");
    let det = c.detail(cc).await.unwrap();
    assert_eq!(det.files.len(), 1, "{:?}", det.files);
    assert_eq!(det.files[0].completed, SIZE as u64);
    assert!(
        det.speed_history.is_empty(),
        "history of a stopped job must be dropped"
    );

    // Remove without deleting files keeps the file.
    let r = c
        .remove(RemoveParams {
            ids: vec![a],
            delete_files: false,
            force: false,
        })
        .await
        .unwrap();
    assert_eq!(r.ok, vec![a], "{r:?}");
    assert!(dl.join("a.bin").exists());
    // Remove with delete_files deletes exactly that file.
    let r = c
        .remove(RemoveParams {
            ids: vec![b],
            delete_files: true,
            force: false,
        })
        .await
        .unwrap();
    assert_eq!(r.ok, vec![b], "{r:?}");
    assert!(!dl.join("b.bin").exists());
    assert!(dl.join("a.bin").exists());
    assert!(dl.join("c.bin").exists());
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(job(&jobs, a).state, JobState::Removed);
    assert_eq!(job(&jobs, b).state, JobState::Removed);

    // Purge drops the record but never files.
    let r = c.purge(&[a]).await.unwrap();
    assert_eq!(r.ok, vec![a]);
    let ev = wait_event(&mut events, 5, |e| matches!(e, Event::JobsRemoved { .. })).await;
    assert!(ev.is_some());
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert!(jobs.iter().all(|j| j.id != a));
    assert!(dl.join("a.bin").exists());
    // Purging an unfinished job is refused.
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Uris {
                uris: vec![uri("d")],
            }],
            options: JobOptions::default(),
            paused: true,
        })
        .await
        .unwrap();
    let dj = added(&out)[0];
    let r = c.purge(&[dj]).await.unwrap();
    assert_eq!(r.failed.len(), 1);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(c.get(dj).await.unwrap().state, JobState::Paused);

    d.stop(&c).await;

    // Restart on the same directories: history and paused state survive.
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let jobs = wait_jobs(&c, 10, "history after restart", |j| j.len() == 3).await;
    assert_eq!(job(&jobs, cc).state, JobState::Complete);
    assert_eq!(job(&jobs, b).state, JobState::Removed);
    assert_eq!(job(&jobs, dj).state, JobState::Paused);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(c.get(dj).await.unwrap().state, JobState::Paused);

    // aria2 crashes: the supervisor restarts it and the daemon reconciles.
    let mut events = c.subscribe().await.unwrap();
    let pids = aria2_pids(root.path());
    assert_eq!(pids.len(), 1, "{pids:?}");
    let st = std::process::Command::new("kill")
        .args(["-9", &pids[0].to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    let down = wait_event(
        &mut events,
        10,
        |e| matches!(e, Event::Engine { status } if !status.available),
    )
    .await;
    assert!(down.is_some(), "no engine-down event");
    let up = wait_event(
        &mut events,
        20,
        |e| matches!(e, Event::Engine { status } if status.available),
    )
    .await;
    assert!(up.is_some(), "engine did not come back");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(jobs.len(), 3, "{jobs:#?}");
    assert_eq!(job(&jobs, dj).state, JobState::Paused);
    assert_eq!(job(&jobs, cc).state, JobState::Complete);

    // engine.restart on a running engine restarts it cleanly and reconciles.
    let status: Vec<EngineStatus> = c.engine_restart().await.unwrap();
    assert!(status[0].available, "{status:?}");
    assert_eq!(aria2_pids(root.path()).len(), 1);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(jobs.len(), 3, "{jobs:#?}");
    assert_eq!(job(&jobs, dj).state, JobState::Paused);
    // aria2 forgot the finished job; detail falls back to the stored record.
    let det = c.detail(cc).await.unwrap();
    assert_eq!(det.files.len(), 1, "{:?}", det.files);
    assert!(c.engine_status().await.unwrap()[0].available);
    d.stop(&c).await;

    // Disk guard: with min_free = 100% nothing may start.
    let mut cfg2 = cfg.clone();
    cfg2.profiles.get_mut("local").unwrap().min_free = DiskThreshold::Percent(100.0);
    let d = Daemon::start(root.path(), cfg2).await;
    let c = d.client().await;
    let mut events = c.subscribe().await.unwrap();
    c.resume(&[dj]).await.unwrap();
    let ev = wait_event(
        &mut events,
        5,
        |e| matches!(e, Event::Message { level: Level::Warn, text } if text.contains("free space")),
    )
    .await;
    assert!(ev.is_some(), "no disk warning");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let j = c.get(dj).await.unwrap();
    assert_eq!(j.state, JobState::Waiting);
    assert_eq!(j.completed_bytes, 0);
    assert!(!dl.join("d.bin").exists() || std::fs::metadata(dl.join("d.bin")).unwrap().len() == 0);
    d.stop(&c).await;

    assert_no_aria2(root.path());
}

/// Completing an HTTP download fires `JobFinished` and the hook exactly once,
/// with the documented environment; a daemon restart does not repeat it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn hook_fires_once_on_completion_with_real_aria2() {
    use std::os::unix::fs::PermissionsExt as _;
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    const SIZE: usize = 64 * 1024;
    let http = serve_files(HashMap::from([("h.bin".to_owned(), content(9, SIZE))])).await;
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("hook.log");
    let script = root.path().join("hook.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$DMAN_EVENT|$DMAN_ID|$DMAN_NAME|$DMAN_PATH|$DMAN_STATUS|$DMAN_ENGINE|$DMAN_SIZE|$DMAN_ERROR\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    cfg.notify.hook.push(dman_config::Hook {
        on: vec![dman_config::NotifyEvent::Completed],
        command: vec![script.display().to_string()],
        timeout_secs: 20,
    });
    let dl = root.path().join("dl");

    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let mut events = c.subscribe().await.unwrap();
    let _ = wait_event(&mut events, 5, |_| true).await;
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Uris {
                uris: vec![format!("http://{http}/h.bin")],
            }],
            options: JobOptions::default(),
            paused: false,
        })
        .await
        .unwrap();
    let id = added(&out)[0];
    let ev = wait_event(&mut events, 30, |e| matches!(e, Event::JobFinished { .. })).await;
    let Some(Event::JobFinished {
        id: eid,
        name,
        path,
        outcome,
    }) = ev
    else {
        panic!("no JobFinished event");
    };
    assert_eq!(eid, id);
    assert_eq!(name, "h.bin");
    assert_eq!(path, dl.join("h.bin").display().to_string());
    assert_eq!(outcome, dman_proto::method::Outcome::Completed);
    for _ in 0..100 {
        if log.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let text = std::fs::read_to_string(&log).expect("hook did not run");
    assert_eq!(
        text,
        format!(
            "completed|{}|h.bin|{}|complete|aria2|{SIZE}|\n",
            id.0,
            dl.join("h.bin").display()
        )
    );
    // No second event for the same job.
    let again = wait_event(&mut events, 2, |e| matches!(e, Event::JobFinished { .. })).await;
    assert!(again.is_none(), "duplicate: {again:?}");
    d.stop(&c).await;
    assert_no_aria2(root.path());

    // After a restart the finished job is not announced again.
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut events = c.subscribe().await.unwrap();
    let again = wait_event(&mut events, 3, |e| matches!(e, Event::JobFinished { .. })).await;
    assert!(again.is_none(), "announced again after restart: {again:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    d.stop(&c).await;
    assert_no_aria2(root.path());
}

/// Live (non-zombie) aria2c processes whose command line mentions `root`.
fn aria2_pids(root: &Path) -> Vec<u32> {
    let needle = root.display().to_string();
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
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

fn assert_no_aria2(root: &Path) {
    let pids = aria2_pids(root);
    assert!(pids.is_empty(), "aria2c still running: {pids:?}");
}
