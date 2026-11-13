//! Video jobs (yt-dlp engine) end to end: the daemon runs in-process and
//! `dman_client` talks to the API.
//!
//! * Without any environment variable the tests use a fake `yt-dlp` (a shell
//!   script speaking the same progress protocol) and a missing one.
//! * `DMAN_TEST_YTDLP=1` adds tests against the real yt-dlp + ffmpeg with an
//!   ffmpeg-generated video on a local Range-capable HTTP server; the mixed
//!   queue test also needs `DMAN_TEST_ARIA2=1`. Run with
//!   `DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`.
// The fake engines are shell scripts, so these tests run on Unix only.
#![cfg(unix)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::{Event, Level, ListParams, Outcome, RemoveParams};
use dman_proto::{
    AddOutcome, AddParams, AddSource, Engine, ErrorCode, Job, JobId, JobOptions, JobState,
    SourceKind, VideoOptions, VideoProbeParams,
};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use url::Url;

fn env_on(name: &str) -> bool {
    std::env::var(name).as_deref() == Ok("1")
}

mod support;
use support::free_port;

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

// ----- daemon harness ---------------------------------------------------------

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
}

fn config(root: &Path, aria2: &str, ytdlp: &str, ffmpeg: Option<&str>) -> Config {
    let mut c = Config::default();
    let p = c.profiles.get_mut("local").unwrap();
    p.daemon = DaemonTarget::Local {
        listen: format!("127.0.0.1:{}", free_port()),
        secret_ref: "daemon".into(),
        tls: None,
    };
    p.download_dir = root.join("dl").display().to_string();
    p.min_free = DiskThreshold::Absolute(0);
    p.warn_free = DiskThreshold::Absolute(0);
    c.aria2.path = aria2.into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c.limits.max_concurrent = 1;
    c.video.ytdlp_path = Some(ytdlp.into());
    c.video.ffmpeg_path = ffmpeg.map(str::to_owned);
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn job(jobs: &[Job], id: JobId) -> &Job {
    jobs.iter()
        .find(|j| j.id == id)
        .unwrap_or_else(|| panic!("job {id} missing"))
}

fn state_of(jobs: &[Job], id: JobId) -> JobState {
    job(jobs, id).state
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

fn video(url: impl Into<String>, video: VideoOptions) -> AddSource {
    AddSource::Video {
        url: url.into(),
        video: Box::new(video),
    }
}

async fn add_one(c: &Client, src: AddSource, options: JobOptions) -> JobId {
    let out = c
        .add(AddParams {
            sources: vec![src],
            options,
            paused: false,
            force: false,
        })
        .await
        .expect("add");
    added(&out)[0]
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Live (non-zombie) processes whose command line contains `what` and `root`.
/// /proc is Linux-only, so elsewhere the leak checks degrade to no-ops.
fn pids(what: &str, root: &Path) -> Vec<u32> {
    let needle = root.display().to_string();
    let mut out = Vec::new();
    let Ok(proc_dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in proc_dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        if let Ok(c) = std::fs::read(entry.path().join("cmdline")) {
            let c = String::from_utf8_lossy(&c);
            if c.contains(what) && c.contains(&needle) && !stat.contains(") Z ") {
                out.push(pid);
            }
        }
    }
    out
}

async fn assert_no_process(what: &str, root: &Path) {
    for _ in 0..50 {
        if pids(what, root).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what} still running: {:?}", pids(what, root));
}

// ----- fake yt-dlp ------------------------------------------------------------

/// A stand-in for yt-dlp: `--version`, the `-o` template, `--print-to-file`
/// marker and JSON progress lines like the real one. The last URL path
/// segment selects the behaviour: `fail*` exits 1 with an `ERROR:` line,
/// `split*` leaves unmerged `.fN.` files, `slow*` takes ~3 s, `long*` ~30 s, anything else is
/// quick. Partial files are continued, like `--continue`. Every invocation's
/// arguments are appended to `args.log` next to the script.
const FAKE_YTDLP: &str = r#"#!/bin/bash
if [ "$1" = "--version" ]; then echo 2026.10.26; exit 0; fi
for a in "$@"; do
  if [ "$a" = "-J" ]; then cat "$(dirname "$0")/probe.json"; exit 0; fi
done
echo "$*" >> "$(dirname "$0")/args.log"
out=""; marker=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2;;
    --print-to-file) marker="$3"; shift 3;;
    --) url="$2"; shift 2;;
    *) shift;;
  esac
done
name="${url##*/}"
path=$(printf '%s' "$out" | sed -e 's/%(title)s/Fake/g' -e "s/%(id)s/$name/g" -e 's/%(ext)s/mp4/g')
progress() { printf '{"status":"downloading","downloaded_bytes":%d,"total_bytes":%d,"speed":100000,"eta":1,"filename":"%s"}\n' "$1" "$2" "$3"; }
case "$name" in
  fail*) echo "[generic] Extracting URL"; echo "ERROR: [generic] Unable to download webpage: HTTP Error 404: Not Found" >&2; exit 1;;
  split*)
    base="${path%.mp4}"
    progress 10 10 "$base.f1.mp4"; printf v > "$base.f1.mp4"
    progress 10 10 "$base.f2.m4a"; printf a > "$base.f2.m4a"
    echo "$path" >> "$marker"; exit 0;;
esac
steps=5; delay=0.02
case "$name" in slow*) steps=60; delay=0.05;; long*) steps=600; delay=0.05;; esac
part="$path.part"
have=0; [ -f "$part" ] && have=$(wc -c < "$part" | tr -d ' ')
total=$((steps*1000))
while [ "$have" -lt "$total" ]; do
  head -c 1000 /dev/zero >> "$part"
  have=$((have+1000))
  progress "$have" "$total" "$path"
  sleep "$delay"
done
mv "$part" "$path"
echo "$path" >> "$marker"
"#;

fn fake_ytdlp(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("yt-dlp");
    std::fs::write(&p, FAKE_YTDLP).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// A fake `ffmpeg` that only answers `-version` (enough for merging to count as available).
fn fake_ffmpeg(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("ffmpeg");
    std::fs::write(
        &p,
        "#!/bin/sh\necho 'ffmpeg version 7.0 Copyright (c) test'\n",
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn fake_args(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("bin/args.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

// ----- tests without a real yt-dlp ---------------------------------------------

/// The global download limit is split between the engines: a lone video job
/// gets all of it as `--limit-rate`, a per-job limit below the share wins, and a
/// limit change respawns a running job only after the hysteresis interval.
#[tokio::test(flavor = "multi_thread")]
async fn global_limit_throttles_ytdlp_with_hysteresis() {
    let root = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(root.path());
    let mut cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        &ytdlp.display().to_string(),
        Some("/nonexistent/ffmpeg"),
    );
    cfg.limits.download = dman_config::ByteLimit(1_000_000);
    cfg.limits.max_concurrent = 2;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let rates = |root: &Path| -> Vec<String> {
        fake_args(root)
            .iter()
            .filter_map(|l| {
                let mut it = l.split_whitespace().skip_while(|w| *w != "--limit-rate");
                it.nth(1).map(str::to_owned)
            })
            .collect()
    };
    // The fake logs its arguments just after it started.
    async fn spawned(root: &Path, n: usize) {
        for _ in 0..100 {
            if fake_args(root).len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // One job, no per-job limit: the whole budget.
    let a = add_one(
        &c,
        video("https://e.example/v/longA", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    wait_jobs(&c, 10, "A active", |j| state_of(j, a) == JobState::Active).await;
    spawned(root.path(), 1).await;
    assert_eq!(rates(root.path()), vec!["1000000"]);

    // A second job with a low own limit keeps it; A's share halves, but A is
    // within its 30 s hysteresis window and is not respawned.
    let b = add_one(
        &c,
        video("https://e.example/v/longB", VideoOptions::default()),
        JobOptions {
            max_download_limit: Some(100_000),
            ..Default::default()
        },
    )
    .await;
    wait_jobs(&c, 10, "B active", |j| state_of(j, b) == JobState::Active).await;
    spawned(root.path(), 2).await;
    assert_eq!(rates(root.path()), vec!["1000000", "100000"]);
    c.limits_set(dman_proto::method::LimitsUpdate {
        download: Some(400_000),
        ..Default::default()
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(rates(root.path()).len(), 2, "no respawn inside the window");
    c.remove(RemoveParams {
        ids: vec![a, b],
        delete_files: false,
        force: false,
    })
    .await
    .unwrap();
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;

    // Second daemon with a short hysteresis interval: a change respawns once.
    let root = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(root.path());
    let mut cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        &ytdlp.display().to_string(),
        Some("/nonexistent/ffmpeg"),
    );
    cfg.limits.download = dman_config::ByteLimit(1_000_000);
    // Read when the daemon starts; only this test's cap changes depend on it.
    // Long enough that the "too soon" check below has seconds of margin on a
    // busy machine.
    dman_daemon::budget::RESPAWN_OVERRIDE_MS.store(4000, std::sync::atomic::Ordering::Relaxed);
    let d = Daemon::start(root.path(), cfg).await;
    dman_daemon::budget::RESPAWN_OVERRIDE_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    let c = d.client().await;
    let a = add_one(
        &c,
        video("https://e.example/v/longC", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    wait_jobs(&c, 10, "C active", |j| state_of(j, a) == JobState::Active).await;
    spawned(root.path(), 1).await;
    assert_eq!(rates(root.path()), vec!["1000000"]);
    let set = |n: u64| {
        c.limits_set(dman_proto::method::LimitsUpdate {
            download: Some(n),
            ..Default::default()
        })
    };
    // A small change (< 25 %) never respawns.
    set(900_000).await.unwrap();
    tokio::time::sleep(Duration::from_millis(4500)).await;
    assert_eq!(rates(root.path()).len(), 1);
    // A big change respawns once, with the new rate.
    set(300_000).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while rates(root.path()).len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "no respawn");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rates(root.path()), vec!["1000000", "300000"]);
    // Right after a respawn the next change waits for the interval.
    wait_jobs(&c, 10, "C active again", |j| {
        state_of(j, a) == JobState::Active
    })
    .await;
    set(100_000).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(rates(root.path()).len(), 2, "hysteresis after a respawn");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while rates(root.path()).len() < 3 {
        assert!(tokio::time::Instant::now() < deadline, "no second respawn");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rates(root.path())[2], "100000");
    c.remove(RemoveParams {
        ids: vec![a],
        delete_files: false,
        force: false,
    })
    .await
    .unwrap();
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
}

/// `jobs.restore` (undo of remove-from-list) for video jobs, no aria2 needed:
/// running/paused/complete jobs return to their state, deleted files and
/// not-removed jobs are refused, and the state survives a daemon restart.
#[tokio::test(flavor = "multi_thread")]
async fn restore_undoes_remove_but_not_file_deletion() {
    let root = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(root.path());
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        &ytdlp.display().to_string(),
        Some("/nonexistent/ffmpeg"),
    );
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let rm = |id: JobId, delete_files: bool| {
        c.remove(RemoveParams {
            ids: vec![id],
            delete_files,
            force: false,
        })
    };

    // A finished job goes back to Complete, files untouched.
    let done = add_one(
        &c,
        video("https://e.example/v/done", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    wait_jobs(&c, 20, "done", |j| state_of(j, done) == JobState::Complete).await;
    // Restoring a job that is not removed is refused.
    let r = c.restore(&[done]).await.unwrap();
    assert!(
        r.ok.is_empty() && r.failed[0].error.code == ErrorCode::Refused,
        "{r:?}"
    );
    assert!(rm(done, false).await.unwrap().failed.is_empty());
    assert_eq!(c.get(done).await.unwrap().state, JobState::Removed);
    let mut events = c.subscribe().await.unwrap();
    let r = c.restore(&[done]).await.unwrap();
    assert_eq!(r.ok, vec![done], "{r:?}");
    assert_eq!(c.get(done).await.unwrap().state, JobState::Complete);
    let ev = wait_event(&mut events, 5, |e| {
        matches!(e, Event::JobsUpdated { jobs } if jobs.iter().any(|j| j.id == done && j.state == JobState::Complete))
    })
    .await;
    assert!(ev.is_some(), "restore must emit JobsUpdated");

    // A running job is requeued and runs again.
    let run = add_one(
        &c,
        video("https://e.example/v/longR", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    wait_jobs(&c, 10, "running", |j| state_of(j, run) == JobState::Active).await;
    assert!(rm(run, false).await.unwrap().failed.is_empty());
    assert_eq!(c.get(run).await.unwrap().state, JobState::Removed);
    assert_eq!(c.restore(&[run]).await.unwrap().ok, vec![run]);
    wait_jobs(&c, 10, "running again", |j| {
        state_of(j, run) == JobState::Active
    })
    .await;

    // A paused job stays paused.
    c.pause(&[run]).await.unwrap();
    wait_jobs(&c, 10, "paused", |j| state_of(j, run) == JobState::Paused).await;
    rm(run, false).await.unwrap();
    // A second remove (now with file deletion) is a different decision: no undo.
    let _ = rm(run, true).await.unwrap();
    let r = c.restore(&[run]).await.unwrap();
    assert_eq!(r.failed.len(), 1, "{r:?}");
    assert_eq!(r.failed[0].error.code, ErrorCode::Refused);
    assert!(r.failed[0].error.message.contains("files were deleted"));
    assert_eq!(c.get(run).await.unwrap().state, JobState::Removed);

    // Removed with file deletion straight away: refused as well.
    let gone = add_one(
        &c,
        video("https://e.example/v/slowG", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    wait_jobs(&c, 10, "gone running", |j| {
        state_of(j, gone) == JobState::Active
    })
    .await;
    let _ = rm(gone, true).await;
    let r = c.restore(&[gone]).await.unwrap();
    assert!(r.ok.is_empty(), "{r:?}");
    assert!(r.failed[0].error.message.contains("files were deleted"));

    // Paused + removed + restart: still restorable, comes back paused.
    let p = add_one(
        &c,
        video("https://e.example/v/longP", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    c.pause(&[p]).await.unwrap();
    wait_jobs(&c, 10, "p paused", |j| state_of(j, p) == JobState::Paused).await;
    rm(p, false).await.unwrap();
    assert!(
        c.restore(&[JobId(9999)]).await.unwrap().failed[0]
            .error
            .code
            == ErrorCode::NotFound
    );
    d.stop(&c).await;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    assert_eq!(c.get(p).await.unwrap().state, JobState::Removed);
    assert_eq!(c.restore(&[p]).await.unwrap().ok, vec![p]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(c.get(p).await.unwrap().state, JobState::Paused);
    assert_eq!(c.get(gone).await.unwrap().state, JobState::Removed);
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
}

/// A missing yt-dlp degrades gracefully: explained in `engine.status`, video
/// adds and probes are refused with `EngineUnavailable`, nothing crashes.
#[tokio::test(flavor = "multi_thread")]
async fn missing_ytdlp_is_explained_and_refused() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        "/nonexistent/yt-dlp",
        Some("/nonexistent/ffmpeg"),
    );
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let status = c.engine_status().await.unwrap();
    let y = status
        .iter()
        .find(|s| s.engine == Engine::YtDlp)
        .expect("yt-dlp entry");
    assert!(!y.available);
    let reason = y.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("pipx install yt-dlp") && reason.contains("brew install yt-dlp"),
        "{reason}"
    );
    let err = c
        .add(AddParams {
            sources: vec![video(
                "https://www.youtube.com/watch?v=x",
                VideoOptions::default(),
            )],
            options: JobOptions::default(),
            paused: false,
            force: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::EngineUnavailable),
        "{err:?}"
    );
    let err = c
        .video_probe(VideoProbeParams {
            url: "https://www.youtube.com/watch?v=x".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::EngineUnavailable),
        "{err:?}"
    );
    let err = c
        .video_plan(dman_proto::VideoPlanParams {
            url: "https://www.youtube.com/watch?v=x".into(),
            options: VideoOptions::default(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::EngineUnavailable),
        "{err:?}"
    );
    // engine.restart re-detects (still missing; aria2 too).
    assert!(c.engine_restart().await.is_err());
    assert!(c.list(ListParams::default()).await.unwrap().is_empty());
    d.stop(&c).await;
}

/// Queue, pause/resume, completion, failures, split streams and removal of
/// video jobs with a fake yt-dlp (no ffmpeg).
#[tokio::test(flavor = "multi_thread")]
async fn video_jobs_share_the_queue_and_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(root.path());
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        &ytdlp.display().to_string(),
        Some("/nonexistent/ffmpeg"),
    );
    let dl = root.path().join("dl");
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let status = c.engine_status().await.unwrap();
    let y = status.iter().find(|s| s.engine == Engine::YtDlp).unwrap();
    assert!(y.available, "{y:?}");
    assert_eq!(y.version.as_deref(), Some("2026.10.26"));
    assert!(
        y.reason
            .as_deref()
            .unwrap_or("")
            .contains("ffmpeg not found"),
        "{y:?}"
    );
    let mut events = c.subscribe().await.unwrap();

    // Validation happens at add time.
    let bad = c
        .add(AddParams {
            sources: vec![video(
                "https://e.example/v",
                VideoOptions {
                    output_template: Some("../escape.%(ext)s".into()),
                    ..Default::default()
                },
            )],
            options: JobOptions::default(),
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    assert!(
        matches!(&bad[0], AddOutcome::Failed { error } if error.code == ErrorCode::InvalidInput),
        "{bad:?}"
    );

    // Two slow jobs, max_concurrent = 1: only one runs.
    let opts = JobOptions {
        max_download_limit: Some(123_456),
        ..Default::default()
    };
    let a = add_one(
        &c,
        video("https://e.example/v/slowA", VideoOptions::default()),
        opts.clone(),
    )
    .await;
    let b = add_one(
        &c,
        video("https://e.example/v/slowB", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    let jobs = wait_jobs(&c, 10, "A active", |j| {
        state_of(j, a) == JobState::Active && job(j, a).completed_bytes > 0
    })
    .await;
    assert_eq!(state_of(&jobs, b), JobState::Waiting);
    assert_eq!(job(&jobs, a).engine, Engine::YtDlp);
    assert_eq!(job(&jobs, a).kind, SourceKind::Video);
    assert_eq!(job(&jobs, b).queue_position, Some(0));

    // Pause A (= stop yt-dlp): B starts; A keeps its .part.
    c.pause(&[a]).await.unwrap();
    let jobs = wait_jobs(&c, 10, "B active", |j| state_of(j, b) == JobState::Active).await;
    assert_eq!(state_of(&jobs, a), JobState::Paused);
    assert!(
        files_in(&dl).iter().any(|f| f == "Fake [slowA].mp4.part"),
        "{:?}",
        files_in(&dl)
    );
    let a_before = std::fs::metadata(dl.join("Fake [slowA].mp4.part"))
        .unwrap()
        .len();
    // Never both active.
    for _ in 0..5 {
        let jobs = c.list(ListParams::default()).await.unwrap();
        assert!(
            jobs.iter().filter(|j| j.state == JobState::Active).count() <= 1,
            "{jobs:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Pause B, resume A: A continues its partial file and completes.
    c.pause(&[b]).await.unwrap();
    c.resume(&[a]).await.unwrap();
    let ev = wait_event(
        &mut events,
        20,
        |e| matches!(e, Event::JobFinished { id, .. } if *id == a),
    )
    .await
    .expect("A finished");
    let Event::JobFinished {
        name,
        path,
        outcome,
        ..
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(name, "Fake [slowA].mp4");
    assert_eq!(path, dl.join("Fake [slowA].mp4").display().to_string());
    let jobs = c.list(ListParams::default()).await.unwrap();
    let ja = job(&jobs, a);
    assert_eq!(ja.state, JobState::Complete);
    assert_eq!(ja.total_bytes, 60_000);
    assert_eq!(ja.completed_bytes, 60_000);
    assert_eq!(
        std::fs::metadata(dl.join("Fake [slowA].mp4"))
            .unwrap()
            .len(),
        60_000
    );
    let det = c.detail(a).await.unwrap();
    assert_eq!(det.files.len(), 1, "{det:?}");
    assert_eq!(
        det.files[0].path,
        dl.join("Fake [slowA].mp4").display().to_string()
    );
    // The resumed run continued (fewer bytes were written by it than in total).
    let args = fake_args(root.path());
    let a_runs: Vec<&String> = args.iter().filter(|l| l.contains("slowA")).collect();
    assert_eq!(a_runs.len(), 2, "{args:?}");
    assert!(a_before > 0 && a_before < 60_000);
    for l in &a_runs {
        assert!(l.contains("--continue"), "{l}");
        assert!(l.contains("--limit-rate 123456"), "{l}");
        // No ffmpeg: only single-file formats.
        assert!(l.contains("-f b "), "{l}");
        assert!(l.contains("--ffmpeg-location /nonexistent"), "{l}");
    }
    // Finished jobs are announced once.
    assert!(
        wait_event(
            &mut events,
            1,
            |e| matches!(e, Event::JobFinished { id, .. } if *id == a)
        )
        .await
        .is_none()
    );

    // A failing download: Error with yt-dlp's message, announced as failed.
    let f = add_one(
        &c,
        video("https://e.example/v/fail1", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    let ev = wait_event(
        &mut events,
        10,
        |e| matches!(e, Event::JobFinished { id, .. } if *id == f),
    )
    .await
    .expect("failure announced");
    assert!(
        matches!(&ev, Event::JobFinished { outcome: Outcome::Failed { error }, .. } if error.contains("HTTP Error 404")),
        "{ev:?}"
    );
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(state_of(&jobs, f), JobState::Error);

    // Unmerged streams without consent: Error explaining ffmpeg.
    let s1 = add_one(
        &c,
        video("https://e.example/v/split1", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    let jobs = wait_jobs(&c, 10, "split1 error", |j| {
        state_of(j, s1) == JobState::Error
    })
    .await;
    let err = job(&jobs, s1).error.clone().unwrap_or_default();
    assert!(err.contains("ffmpeg"), "{err}");
    // With consent: Complete, a warning, both files recorded.
    let s2 = add_one(
        &c,
        video(
            "https://e.example/v/split2",
            VideoOptions {
                allow_separate_streams: true,
                ..Default::default()
            },
        ),
        JobOptions::default(),
    )
    .await;
    let warn = wait_event(&mut events, 10, |e| {
        matches!(e, Event::Message { level: Level::Warn, text } if text.contains("separate files"))
    })
    .await;
    assert!(warn.is_some());
    let jobs = wait_jobs(&c, 10, "split2 complete", |j| {
        state_of(j, s2) == JobState::Complete
    })
    .await;
    assert!(job(&jobs, s2).error.is_none());
    assert_eq!(c.detail(s2).await.unwrap().files.len(), 2);
    let consent_run = fake_args(root.path())
        .into_iter()
        .find(|l| l.contains("split2"))
        .unwrap();
    assert!(consent_run.contains("-f bv*+ba/b"), "{consent_run}");

    // Retry of a failed video job queues it again.
    c.resume(&[f]).await.unwrap();
    wait_jobs(&c, 10, "retried and failed again", |j| {
        state_of(j, f) == JobState::Error
    })
    .await;

    // Remove B (paused, partial) with its files: only B's files go.
    std::fs::write(dl.join("unrelated.mp4.part"), b"keep").unwrap();
    assert!(files_in(&dl).iter().any(|f| f == "Fake [slowB].mp4.part"));
    c.remove(RemoveParams {
        ids: vec![b],
        delete_files: true,
        force: false,
    })
    .await
    .unwrap();
    let left = files_in(&dl);
    assert!(!left.iter().any(|f| f.contains("slowB")), "{left:?}");
    for keep in [
        "Fake [slowA].mp4",
        "unrelated.mp4.part",
        "Fake [split2].f1.mp4",
    ] {
        assert!(left.iter().any(|f| f == keep), "{keep} missing: {left:?}");
    }
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(state_of(&jobs, b), JobState::Removed);
    // Video jobs are paused/finished: nothing runs.
    assert_no_process("slow", root.path()).await;

    // Rename of a waiting video job is refused; options of finished jobs too.
    let p = c
        .add(AddParams {
            sources: vec![video("https://e.example/v/quick", VideoOptions::default())],
            options: JobOptions::default(),
            paused: true,
            force: false,
        })
        .await
        .unwrap();
    let p = added(&p)[0];
    let err = c.rename(p, "x.mp4", None).await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::Refused),
        "{err:?}"
    );
    c.resume(&[p]).await.unwrap();
    wait_jobs(&c, 10, "quick complete", |j| {
        state_of(j, p) == JobState::Complete
    })
    .await;
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
}

/// A daemon restart while a video downloads: yt-dlp is stopped with the
/// daemon, the job comes back queued and is respawned (continuing its .part).
#[tokio::test(flavor = "multi_thread")]
async fn video_job_survives_daemon_restart() {
    let root = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(root.path());
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        &ytdlp.display().to_string(),
        Some("/nonexistent/ffmpeg"),
    );
    let dl = root.path().join("dl");
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let id = add_one(
        &c,
        video("https://e.example/v/slowR", VideoOptions::default()),
        JobOptions::default(),
    )
    .await;
    wait_jobs(&c, 10, "active", |j| {
        state_of(j, id) == JobState::Active && job(j, id).completed_bytes > 5_000
    })
    .await;
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
    let part = std::fs::metadata(dl.join("Fake [slowR].mp4.part"))
        .unwrap()
        .len();
    assert!(part > 0 && part < 60_000, "{part}");

    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert!(
        matches!(state_of(&jobs, id), JobState::Waiting | JobState::Active),
        "{jobs:#?}"
    );
    let jobs = wait_jobs(&c, 20, "complete after restart", |j| {
        state_of(j, id) == JobState::Complete
    })
    .await;
    assert_eq!(job(&jobs, id).total_bytes, 60_000);
    assert_eq!(fake_args(root.path()).len(), 2);
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
}

/// A YouTube-like `yt-dlp -J` answer for the fake.
const FAKE_PROBE: &str = r#"{"id":"abc","title":"Demo","duration":100.0,"formats":[
 {"format_id":"139","ext":"m4a","vcodec":"none","acodec":"mp4a.40.5","tbr":49.6,"filesize":600000},
 {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","tbr":129.5,"filesize":1600000},
 {"format_id":"251","ext":"webm","vcodec":"none","acodec":"opus","tbr":135.0,"filesize":1700000},
 {"format_id":"18","ext":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","height":360,"fps":30,"filesize":6000000},
 {"format_id":"136","ext":"mp4","vcodec":"avc1.4d401f","acodec":"none","height":720,"fps":30,"filesize":18000000},
 {"format_id":"137","ext":"mp4","vcodec":"avc1.640028","acodec":"none","height":1080,"fps":30,"filesize":50000000},
 {"format_id":"313","ext":"webm","vcodec":"vp9","acodec":"none","height":2160,"fps":30,"filesize":180000000}]}"#;

/// The args of the (single) yt-dlp run for `.../name`.
fn args_for(root: &Path, name: &str) -> String {
    fake_args(root)
        .into_iter()
        .find(|l| l.ends_with(&format!("/{name}")))
        .unwrap_or_else(|| panic!("no yt-dlp run for {name}: {:?}", fake_args(root)))
}

async fn add_and_finish(c: &Client, name: &str, opts: VideoOptions) {
    let id = add_one(
        c,
        video(format!("https://e.example/v/{name}"), opts),
        JobOptions::default(),
    )
    .await;
    wait_jobs(c, 20, "complete", |j| state_of(j, id) == JobState::Complete).await;
}

/// `video.preferences.get/set`: saved to `[video]` with comments kept, applied
/// without a restart (new jobs use them), validated, and still there after a
/// restart. Adding without quality uses default quality + audio + container;
/// an explicit `VID+AUD` format id reaches yt-dlp as given.
#[tokio::test(flavor = "multi_thread")]
async fn video_preferences_persist_apply_and_drive_defaults() {
    let root = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(root.path());
    std::fs::write(root.path().join("bin/probe.json"), FAKE_PROBE).unwrap();
    let ffmpeg = fake_ffmpeg(root.path());
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        &ytdlp.display().to_string(),
        Some(&ffmpeg.display().to_string()),
    );
    let paths = Paths::with_root(root.path());
    let text = toml::to_string(&cfg)
        .unwrap()
        .replace("[video]\n", "[video]\n# keep me: video notes\n");
    std::fs::write(paths.config_file(), format!("# top comment\n{text}")).unwrap();

    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let p = c.video_preferences().await.unwrap();
    assert!(p.ask_quality);
    assert_eq!(
        (p.default_quality.as_str(), p.default_audio.as_str()),
        ("best", "best")
    );
    assert_eq!(p.default_container, None);

    // The probe reflects the defaults: ffmpeg present -> merged choices, the best
    // one suggested with the best audio.
    let probe = c
        .video_probe(VideoProbeParams {
            url: "https://e.example/v/probe".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(probe.needs_choice);
    assert_eq!(probe.video_choices[0].id, "313+251");
    assert_eq!(probe.suggested.video.as_deref(), Some("313+251"));
    assert_eq!(probe.audio_choices.len(), 3);

    // `video.plan` answers from the same code, without probing.
    let plan = c
        .video_plan(dman_proto::VideoPlanParams {
            url: "https://e.example/v/probe".into(),
            options: VideoOptions::default(),
        })
        .await
        .unwrap();
    assert_eq!(plan.selector, "bv*+ba/b");
    assert!(plan.ffmpeg && plan.asks_quality, "{plan:?}");

    add_and_finish(&c, "d1", VideoOptions::default()).await;
    assert!(
        args_for(root.path(), "d1").contains("-f bv*+ba/b "),
        "{}",
        args_for(root.path(), "d1")
    );

    // "Use as default" + "never ask again".
    let new = c
        .video_preferences_set(dman_proto::VideoPreferencesUpdate {
            ask_quality: Some(false),
            default_quality: Some("1080".into()),
            default_audio: Some("M4A".into()),
            default_container: Some("mkv".into()),
        })
        .await
        .unwrap();
    assert!(!new.ask_quality);
    assert_eq!(new.default_quality, "1080p");
    assert_eq!(new.default_audio, "m4a");
    assert_eq!(new.default_container.as_deref(), Some("mkv"));
    assert_eq!(c.video_preferences().await.unwrap(), new);

    let file = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(file.contains("# top comment") && file.contains("# keep me: video notes"));
    let saved = Config::load(&paths).unwrap().video;
    assert!(!saved.ask_quality);
    assert_eq!(saved.default_quality, "1080p");
    assert_eq!(saved.default_audio, "m4a");
    assert_eq!(saved.default_container.as_deref(), Some("mkv"));

    // Applied at once: no restart, no reload call.
    let plan = c
        .video_plan(dman_proto::VideoPlanParams {
            url: "https://e.example/v/probe".into(),
            options: VideoOptions::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        plan.selector,
        "bv*[height<=1080]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]"
    );
    assert_eq!(plan.container.as_deref(), Some("mkv"));
    assert!(!plan.asks_quality, "{plan:?}");
    add_and_finish(&c, "d2", VideoOptions::default()).await;
    let a = args_for(root.path(), "d2");
    assert!(
        a.contains("-f bv*[height<=1080]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080] "),
        "{a}"
    );
    assert!(a.contains("--merge-output-format mkv"), "{a}");
    // The suggestion follows the new defaults too.
    let probe = c
        .video_probe(VideoProbeParams {
            url: "https://e.example/v/probe".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(probe.suggested.video.as_deref(), Some("137+140"));

    // An explicit combined format id wins over the defaults.
    add_and_finish(
        &c,
        "d3",
        VideoOptions {
            format_id: Some("136+251".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(args_for(root.path(), "d3").contains("-f 136+251 "));

    // Invalid values change nothing (memory and file).
    let before = std::fs::read_to_string(paths.config_file()).unwrap();
    for bad in [
        dman_proto::VideoPreferencesUpdate {
            default_quality: Some("999p".into()),
            ..Default::default()
        },
        dman_proto::VideoPreferencesUpdate {
            default_audio: Some("flac".into()),
            ..Default::default()
        },
        dman_proto::VideoPreferencesUpdate {
            ask_quality: Some(true),
            default_container: Some("avi".into()),
            ..Default::default()
        },
    ] {
        let e = c.video_preferences_set(bad).await.unwrap_err();
        assert!(
            matches!(&e, ClientError::Rpc(e) if e.code == ErrorCode::InvalidInput),
            "{e:?}"
        );
    }
    assert_eq!(c.video_preferences().await.unwrap(), new);
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        before
    );

    // Settings turns asking back on and clears the container.
    let back = c
        .video_preferences_set(dman_proto::VideoPreferencesUpdate {
            ask_quality: Some(true),
            default_container: Some(String::new()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(back.ask_quality && back.default_container.is_none());
    assert_eq!(back.default_quality, "1080p");
    let file = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(!file.contains("default_container") && file.contains("# keep me"));
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;

    // Still there after a restart (the daemon starts from the saved file).
    let cfg = Config::load(&paths).unwrap();
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    assert_eq!(c.video_preferences().await.unwrap(), back);
    d.stop(&c).await;
}

// ----- tests with the real yt-dlp + ffmpeg --------------------------------------

/// 10 s 640x360 test video (about 2 MB).
fn make_video() -> Vec<u8> {
    let d = tempfile::tempdir().unwrap();
    let st = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-b:v",
            "1500k",
            "-c:a",
            "aac",
            "test.mp4",
        ])
        .current_dir(d.path())
        .status()
        .expect("ffmpeg");
    assert!(st.success());
    std::fs::read(d.path().join("test.mp4")).unwrap()
}

fn real_config(root: &Path, aria2: &str) -> Config {
    let mut c = config(root, aria2, "yt-dlp", None);
    c.video.ytdlp_path = None;
    c
}

/// Probe, add, progress, pause/resume (.part continued), completion with the
/// final path, JobFinished + hook exactly once, removal with delete_files.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn real_video_lifecycle() {
    use std::os::unix::fs::PermissionsExt as _;
    if !env_on("DMAN_TEST_YTDLP") {
        eprintln!("DMAN_TEST_YTDLP != 1; skipping");
        return;
    }
    let media = make_video();
    let size = media.len() as u64;
    let http = serve_files(HashMap::from([("test.mp4".to_owned(), media)])).await;
    let url = format!("http://{http}/test.mp4");
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("hook.log");
    let script = root.path().join("hook.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$DMAN_EVENT|$DMAN_ID|$DMAN_NAME|$DMAN_ENGINE|$DMAN_SIZE\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = real_config(root.path(), "/nonexistent/aria2c");
    cfg.notify.hook.push(dman_config::Hook {
        on: vec![dman_config::NotifyEvent::Completed],
        command: vec![script.display().to_string()],
        timeout_secs: 20,
    });
    let dl = root.path().join("dl");
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let status = c.engine_status().await.unwrap();
    let y = status.iter().find(|s| s.engine == Engine::YtDlp).unwrap();
    assert!(
        y.available && y.features.iter().any(|f| f == "merge"),
        "{y:?}"
    );

    let probe = c
        .video_probe(VideoProbeParams {
            url: url.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!probe.formats.is_empty(), "{probe:?}");
    assert_eq!(probe.plans.len(), 9);
    assert!(probe.ffmpeg);

    let mut events = c.subscribe().await.unwrap();
    let id = add_one(
        &c,
        video(&url, VideoOptions::default()),
        JobOptions {
            max_download_limit: Some(300_000),
            ..Default::default()
        },
    )
    .await;
    let jobs = wait_jobs(&c, 30, "progress", |j| {
        state_of(j, id) == JobState::Active && job(j, id).completed_bytes > 200_000
    })
    .await;
    assert_eq!(job(&jobs, id).total_bytes, size);
    assert!(!pids("yt-dlp", root.path()).is_empty());

    c.pause(&[id]).await.unwrap();
    wait_jobs(&c, 10, "paused", |j| state_of(j, id) == JobState::Paused).await;
    assert_no_process("yt-dlp", root.path()).await;
    let part: Vec<String> = files_in(&dl)
        .into_iter()
        .filter(|f| f.ends_with(".part"))
        .collect();
    assert_eq!(part.len(), 1, "{:?}", files_in(&dl));
    let part_len = std::fs::metadata(dl.join(&part[0])).unwrap().len();
    assert!(part_len > 0 && part_len < size);

    c.set_options(
        id,
        JobOptions {
            max_download_limit: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    c.resume(&[id]).await.unwrap();
    let ev = wait_event(
        &mut events,
        60,
        |e| matches!(e, Event::JobFinished { id: x, .. } if *x == id),
    )
    .await
    .expect("finished");
    assert!(
        matches!(
            &ev,
            Event::JobFinished {
                outcome: Outcome::Completed,
                ..
            }
        ),
        "{ev:?}"
    );
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(state_of(&jobs, id), JobState::Complete);
    let det = c.detail(id).await.unwrap();
    assert_eq!(det.files.len(), 1, "{det:?}");
    let out = PathBuf::from(&det.files[0].path);
    // ffmpeg embeds metadata ([video] embed_metadata = true): about the source size.
    let on_disk = std::fs::metadata(&out).unwrap().len();
    assert!(on_disk.abs_diff(size) < 4096, "{on_disk} vs {size}");
    assert_eq!(job(&jobs, id).total_bytes, on_disk);
    assert!(
        !files_in(&dl).iter().any(|f| f.ends_with(".part")),
        "{:?}",
        files_in(&dl)
    );
    for _ in 0..100 {
        if log.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let text = std::fs::read_to_string(&log).expect("hook did not run");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.starts_with(&format!("completed|{}|", id.0)), "{text}");
    assert!(
        text.trim_end().ends_with(&format!("|yt_dlp|{on_disk}")),
        "{text}"
    );
    assert!(
        wait_event(&mut events, 2, |e| matches!(e, Event::JobFinished { .. }))
            .await
            .is_none()
    );

    // Remove with delete_files: only this job's output goes.
    std::fs::write(dl.join("keep.txt"), b"k").unwrap();
    c.remove(RemoveParams {
        ids: vec![id],
        delete_files: true,
        force: false,
    })
    .await
    .unwrap();
    assert!(!out.exists());
    assert_eq!(files_in(&dl), ["keep.txt"]);
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
}

/// Daemon restart while a real yt-dlp download runs: respawned, completes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn real_video_restart_resumes() {
    if !env_on("DMAN_TEST_YTDLP") {
        eprintln!("DMAN_TEST_YTDLP != 1; skipping");
        return;
    }
    let media = make_video();
    let size = media.len() as u64;
    let http = serve_files(HashMap::from([("test.mp4".to_owned(), media)])).await;
    let url = format!("http://{http}/test.mp4");
    let root = tempfile::tempdir().unwrap();
    let cfg = real_config(root.path(), "/nonexistent/aria2c");
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let id = add_one(
        &c,
        video(&url, VideoOptions::default()),
        JobOptions {
            max_download_limit: Some(400_000),
            ..Default::default()
        },
    )
    .await;
    wait_jobs(&c, 30, "progress", |j| job(j, id).completed_bytes > 300_000).await;
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;

    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let jobs = wait_jobs(&c, 60, "complete after restart", |j| {
        state_of(j, id) == JobState::Complete
    })
    .await;
    let det = c.detail(id).await.unwrap();
    let on_disk = std::fs::metadata(&det.files[0].path).unwrap().len();
    assert!(on_disk.abs_diff(size) < 4096, "{on_disk} vs {size}");
    assert_eq!(job(&jobs, id).total_bytes, on_disk);
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
}

/// One queue for both engines: with max_concurrent = 1 an aria2 HTTP job and
/// a video job never run at the same time.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c, yt-dlp and ffmpeg; set DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1"]
async fn real_aria2_and_video_share_max_concurrent() {
    if !env_on("DMAN_TEST_YTDLP") || !env_on("DMAN_TEST_ARIA2") {
        eprintln!("DMAN_TEST_YTDLP / DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let media = make_video();
    let blob: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    let http = serve_files(HashMap::from([
        ("test.mp4".to_owned(), media),
        ("file.bin".to_owned(), blob),
    ]))
    .await;
    let root = tempfile::tempdir().unwrap();
    let cfg = real_config(root.path(), "aria2c");
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let limit = JobOptions {
        max_download_limit: Some(500_000),
        ..Default::default()
    };
    let h = add_one(
        &c,
        AddSource::Uris {
            uris: vec![format!("http://{http}/file.bin")],
        },
        limit.clone(),
    )
    .await;
    let v = add_one(
        &c,
        video(format!("http://{http}/test.mp4"), VideoOptions::default()),
        limit,
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let mut saw = (false, false);
    loop {
        let jobs = c.list(ListParams::default()).await.unwrap();
        let active: Vec<JobId> = jobs
            .iter()
            .filter(|j| j.state == JobState::Active)
            .map(|j| j.id)
            .collect();
        assert!(active.len() <= 1, "both running: {jobs:#?}");
        saw.0 |= active.contains(&h);
        saw.1 |= active.contains(&v);
        if state_of(&jobs, h) == JobState::Complete && state_of(&jobs, v) == JobState::Complete {
            break;
        }
        assert!(
            !matches!(state_of(&jobs, h), JobState::Error)
                && !matches!(state_of(&jobs, v), JobState::Error),
            "{jobs:#?}"
        );
        assert!(tokio::time::Instant::now() < deadline, "{jobs:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(saw.0 && saw.1);
    d.stop(&c).await;
    assert_no_process("yt-dlp", root.path()).await;
    assert_no_process("aria2c", root.path()).await;
}
