//! Scheduling and limit persistence, end to end with an injectable clock.
//!
//! The first test needs no aria2. The second runs real downloads and needs
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dman_client::Client;
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, ScheduleWindow, Secrets};
use dman_daemon::schedule::Clock;
use dman_daemon::{DaemonOptions, run_with_clock};
use dman_proto::method::{Event, Level, LimitsUpdate, ListParams};
use dman_proto::{AddParams, AddSource, Job, JobId, JobOptions, JobState};
use jiff::Zoned;
use jiff::civil::{DateTime, date};
use jiff::tz::TimeZone;
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use url::Url;

const MIB: u64 = 1 << 20;

/// A clock the test moves by hand (UTC).
struct ManualClock(Mutex<Zoned>);

impl ManualClock {
    fn new(dt: DateTime) -> Arc<Self> {
        Arc::new(Self(Mutex::new(TimeZone::UTC.to_zoned(dt).unwrap())))
    }
    fn set(&self, dt: DateTime) {
        *self.0.lock().unwrap() = TimeZone::UTC.to_zoned(dt).unwrap();
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Zoned {
        self.0.lock().unwrap().clone()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn windows(toml: &str) -> Vec<ScheduleWindow> {
    #[derive(serde::Deserialize)]
    struct W {
        schedule: Vec<ScheduleWindow>,
    }
    toml::from_str::<W>(toml).unwrap().schedule
}

const WINDOWS: &str = r#"
[[schedule]]
name = "night"
start = "22:00"
end = "06:00"
action = "pause"

[[schedule]]
name = "day"
start = "08:00"
end = "18:00"
download_limit = "1MiB"
max_concurrent = 2
"#;

fn config(root: &Path, aria2_path: &str) -> Config {
    let mut c = Config::default();
    let p = c.profiles.get_mut("local").unwrap();
    p.daemon = DaemonTarget::Local {
        listen: format!("127.0.0.1:{}", free_port()),
        secret_ref: "daemon".into(),
    };
    p.download_dir = root.join("dl").display().to_string();
    p.min_free = DiskThreshold::Absolute(0);
    p.warn_free = DiskThreshold::Absolute(0);
    c.aria2.path = aria2_path.into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c.limits.max_concurrent = 1;
    c.schedule = windows(WINDOWS);
    c.validate().unwrap();
    c
}

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    async fn start(root: &Path, config: Config, clock: Arc<ManualClock>) -> Self {
        let paths = Paths::with_root(root);
        let DaemonTarget::Local { listen, .. } = &config.profiles["local"].daemon else {
            unreachable!()
        };
        let url = Url::parse(&format!("ws://{listen}/api/v1/ws")).unwrap();
        let task = tokio::spawn(run_with_clock(
            DaemonOptions {
                paths: paths.clone(),
                config,
                profile: "local".into(),
            },
            clock,
        ));
        let d = Self { paths, url, task };
        for _ in 0..200 {
            assert!(!d.task.is_finished(), "daemon stopped early");
            if d.try_connect().await.is_some() {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("daemon did not come up");
    }

    async fn try_connect(&self) -> Option<Client> {
        let token = Secrets::load(&self.paths)
            .ok()
            .and_then(|s| s.get("daemon"))
            .unwrap_or_else(|| SecretString::from(String::new()));
        Client::connect(&self.url, token, "dman-test").await.ok()
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

/// Polls until `f` holds (10 s).
async fn until<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn wait_message(rx: &mut broadcast::Receiver<Event>, level: Level, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(Event::Message { level: l, text })) if l == level && text.contains(needle) => {
                return text;
            }
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("no {level:?} message containing {needle:?}: {other:?}"),
        }
    }
}

fn ts(dt: DateTime) -> i64 {
    TimeZone::UTC.to_zoned(dt).unwrap().timestamp().as_second()
}

async fn limits_are(c: &Client, d: u64, u: u64, m: u32) -> bool {
    let l = c.limits_get().await.unwrap();
    (l.download, l.upload, l.max_concurrent) == (d, u, m)
}

#[tokio::test(flavor = "multi_thread")]
async fn windows_limits_overrides_and_persistence_without_engine() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(root.path());
    Config::write_default(&paths).unwrap();
    let clock = ManualClock::new(date(2027, 3, 24).at(12, 0, 0, 0)); // Wednesday
    let d = Daemon::start(
        root.path(),
        config(root.path(), "/nonexistent/aria2c"),
        clock.clone(),
    )
    .await;
    let c = d.client().await;
    let mut events = c.subscribe().await.unwrap();

    // Inside "day": its limits apply; the status names the window and the end.
    until("day limits", || async { limits_are(&c, MIB, 0, 2).await }).await;
    let s = c.schedule().await.unwrap();
    assert_eq!(s.windows.len(), 2);
    assert_eq!(s.windows[1].download_limit, Some(MIB));
    assert_eq!(s.windows[0].days.len(), 7);
    assert_eq!(s.active.as_deref(), Some("day"));
    assert_eq!(s.next_change, Some(ts(date(2027, 3, 24).at(18, 0, 0, 0))));
    assert_eq!(s.next_window, None);
    assert!(!s.paused && !s.override_active);

    // Changing a field the window sets is a temporary override: the config is
    // untouched and a message says so.
    let l = c
        .limits_set(LimitsUpdate {
            download: Some(5 * MIB),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(l.download, 5 * MIB);
    wait_message(
        &mut events,
        Level::Info,
        "until the schedule window 'day' ends",
    )
    .await;
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("download = \"0\""), "{text}");
    assert!(c.schedule().await.unwrap().override_active);

    // A field the window does not set changes the base limits and is saved,
    // keeping comments.
    c.limits_set(LimitsUpdate {
        upload: Some(2 * MIB),
        ..Default::default()
    })
    .await
    .unwrap();
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("upload = \"2MiB\""), "{text}");
    assert!(text.contains("# dman configuration."), "{text}");
    assert!(text.contains("# Bytes per second"), "{text}");
    assert!(text.contains("download = \"0\""), "{text}");

    // Leaving the window drops the override and restores the base limits.
    clock.set(date(2027, 3, 24).at(19, 0, 0, 0));
    wait_message(&mut events, Level::Info, "schedule 'day' ended").await;
    assert!(limits_are(&c, 0, 2 * MIB, 1).await);
    let s = c.schedule().await.unwrap();
    assert_eq!(s.active, None);
    assert!(!s.override_active);
    assert_eq!(s.next_change, Some(ts(date(2027, 3, 24).at(22, 0, 0, 0))));
    assert_eq!(s.next_window.as_deref(), Some("night"));

    // Outside windows every change is a base change and is saved.
    c.limits_set(LimitsUpdate {
        download: Some(3 * MIB),
        max_concurrent: Some(4),
        ..Default::default()
    })
    .await
    .unwrap();
    let saved = Config::load(&paths).unwrap();
    assert_eq!(saved.limits.download.0, 3 * MIB);
    assert_eq!(saved.limits.upload.0, 2 * MIB);
    assert_eq!(saved.limits.max_concurrent, 4);
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("# Bytes per second"), "{text}");

    // A pause window: announced, limits unchanged, status says paused.
    clock.set(date(2027, 3, 24).at(23, 0, 0, 0));
    let m = wait_message(&mut events, Level::Info, "schedule 'night' started").await;
    assert!(
        m.contains("downloads paused") && m.contains("until 06:00"),
        "{m}"
    );
    assert!(limits_are(&c, 3 * MIB, 2 * MIB, 4).await);
    let s = c.schedule().await.unwrap();
    assert_eq!((s.active.as_deref(), s.paused), (Some("night"), true));
    // Base changes inside a pause window are saved too.
    c.limits_set(LimitsUpdate {
        max_concurrent: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(Config::load(&paths).unwrap().limits.max_concurrent, 5);

    // Next day, "day" again: the window's limits, no leftover override.
    clock.set(date(2027, 3, 25).at(12, 0, 0, 0));
    wait_message(&mut events, Level::Info, "schedule 'day' started").await;
    assert!(limits_are(&c, MIB, 2 * MIB, 2).await);

    // An unwritable config keeps the runtime change and warns.
    clock.set(date(2027, 3, 25).at(19, 0, 0, 0));
    wait_message(&mut events, Level::Info, "schedule 'day' ended").await;
    std::fs::remove_file(paths.config_file()).unwrap();
    std::fs::create_dir(paths.config_file()).unwrap();
    let l = c
        .limits_set(LimitsUpdate {
            max_concurrent: Some(6),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(l.max_concurrent, 6);
    wait_message(&mut events, Level::Warn, "cannot save them to config.toml").await;
    assert!(limits_are(&c, 3 * MIB, 2 * MIB, 6).await);

    d.stop(&c).await;
}

// ----- real downloads ----------------------------------------------------------

async fn serve(files: HashMap<String, Vec<u8>>) -> SocketAddr {
    let files = Arc::new(files);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(http(s, files.clone()));
        }
    });
    addr
}

async fn http(mut s: TcpStream, files: Arc<HashMap<String, Vec<u8>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let first = head.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().trim_start_matches('/');
    let Some(data) = files.get(path) else {
        let _ = s
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };
    let range = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("range")
            .then(|| v.trim().to_owned())
    });
    let len = data.len();
    let (status, start) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        Some(r) => (
            "206 Partial Content",
            r.split('-').next().unwrap().parse().unwrap_or(0),
        ),
        None => ("200 OK", 0usize),
    };
    let body = &data[start..];
    let mut h = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        body.len()
    );
    if status.starts_with("206") {
        h.push_str(&format!(
            "Content-Range: bytes {start}-{}/{len}\r\n",
            len - 1
        ));
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

async fn jobs(c: &Client) -> Vec<Job> {
    c.list(ListParams::default()).await.unwrap()
}

fn count(j: &[Job], s: JobState) -> usize {
    j.iter().filter(|j| j.state == s).count()
}

fn state_of(j: &[Job], id: JobId) -> JobState {
    j.iter().find(|j| j.id == id).unwrap().state
}

async fn add(c: &Client, uri: String, paused: bool) -> JobId {
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Uris { uris: vec![uri] }],
            options: JobOptions {
                max_download_limit: Some(50 * 1024),
                ..Default::default()
            },
            paused,
        })
        .await
        .unwrap();
    match &out[0] {
        dman_proto::AddOutcome::Added { id, .. } => *id,
        o => panic!("add failed: {o:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn pause_window_stops_and_resumes_downloads_with_real_aria2() {
    if std::env::var("DMAN_TEST_ARIA2").as_deref() != Ok("1") {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let files: HashMap<String, Vec<u8>> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| (format!("{n}.bin"), vec![7u8; 3 << 20]))
        .collect();
    let http = serve(files).await;
    let uri = |n: &str| format!("http://{http}/{n}.bin");
    let root = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(date(2027, 3, 24).at(12, 0, 0, 0));
    let d = Daemon::start(root.path(), config(root.path(), "aria2c"), clock.clone()).await;
    let c = d.client().await;
    let mut events = c.subscribe().await.unwrap();

    // "day" window: max_concurrent = 2 beats the base limit of 1.
    until("day limits", || async { limits_are(&c, MIB, 0, 2).await }).await;
    let a = add(&c, uri("a"), false).await;
    add(&c, uri("b"), false).await;
    until("a and b active", || async {
        count(&jobs(&c).await, JobState::Active) == 2
    })
    .await;

    // Entering the pause window: running jobs stop (but stay queued), nothing starts.
    clock.set(date(2027, 3, 24).at(22, 30, 0, 0));
    wait_message(&mut events, Level::Info, "schedule 'night' started").await;
    until("everything held back", || async {
        let j = jobs(&c).await;
        count(&j, JobState::Active) == 0 && count(&j, JobState::Waiting) == 2
    })
    .await;
    let cc = add(&c, uri("c"), false).await;
    let dd = add(&c, uri("d"), true).await; // paused by the user
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let j = jobs(&c).await;
        assert_eq!(count(&j, JobState::Active), 0, "{j:#?}");
        assert_eq!(state_of(&j, cc), JobState::Waiting);
        assert_eq!(state_of(&j, dd), JobState::Paused);
    }

    // Leaving it (no window now, base max_concurrent = 1): one job resumes, the
    // user-paused one stays paused.
    clock.set(date(2027, 3, 25).at(7, 0, 0, 0));
    wait_message(&mut events, Level::Info, "downloads resume").await;
    until("one job running again", || async {
        let j = jobs(&c).await;
        count(&j, JobState::Active) == 1 && state_of(&j, a) == JobState::Active
    })
    .await;
    let j = jobs(&c).await;
    assert_eq!(count(&j, JobState::Waiting), 2, "{j:#?}");
    assert_eq!(state_of(&j, dd), JobState::Paused);
    assert!(limits_are(&c, 0, 0, 1).await);

    // Back in "day": two slots again.
    clock.set(date(2027, 3, 25).at(9, 0, 0, 0));
    until("two active", || async {
        count(&jobs(&c).await, JobState::Active) == 2
    })
    .await;
    assert_eq!(state_of(&jobs(&c).await, dd), JobState::Paused);

    d.stop(&c).await;
}
