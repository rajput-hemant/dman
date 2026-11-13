//! Seeding limits against a real aria2c: a torrent is downloaded from a
//! local seeder, then dman seeds it to a second (leecher) aria2c; uploaded
//! bytes are counted, the ratio limit ends seeding once, and a daemon restart
//! in the middle neither resets the counter nor the ratio.
//!
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`
//! (needs aria2c and python3).

#![allow(dead_code)]

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::{Event, ListParams, Outcome};
use dman_proto::{AddOutcome, AddParams, AddSource, Job, JobId, JobOptions, JobState};
use secrecy::SecretString;
use tokio::task::JoinHandle;
use url::Url;

fn aria2_enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

mod support;
use support::free_port;

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
        tls: None,
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

/// The python fixture: local tracker + aria2c seeder (+ optional leecher).
struct Seeder {
    child: std::process::Child,
    out: std::io::BufReader<std::process::ChildStdout>,
    info: serde_json::Value,
}

impl Seeder {
    fn start(work: &Path) -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seeder.py");
        let mut child = std::process::Command::new("python3")
            .arg("-I")
            .arg(script)
            .arg(work)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("python3 is needed for the seeder fixture");
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        let info =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("seeder said {line:?}: {e}"));
        Self { child, out, info }
    }

    /// Sends one command line and returns the JSON answer.
    fn cmd(&mut self, line: &str) -> serde_json::Value {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        let mut answer = String::new();
        self.out.read_line(&mut answer).unwrap();
        let v: serde_json::Value = serde_json::from_str(&answer)
            .unwrap_or_else(|e| panic!("fixture answered {answer:?}: {e}"));
        assert!(v.get("error").is_none(), "`{line}` failed: {v}");
        v
    }
}

impl Drop for Seeder {
    fn drop(&mut self) {
        drop(self.child.stdin.take()); // the script shuts its aria2c processes down on EOF
        for _ in 0..80 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

type Finished = Arc<Mutex<Vec<(JobId, Outcome)>>>;

/// Collects every `JobFinished` the client sees.
async fn collect_finished(c: &Client, into: Finished) {
    let mut rx = c.subscribe().await.expect("subscribe");
    tokio::spawn(async move {
        while let Ok(e) = rx.recv().await {
            if let Event::JobFinished { id, outcome, .. } = e {
                into.lock().unwrap().push((id, outcome));
            }
        }
    });
}

async fn job_now(c: &Client, id: JobId) -> Job {
    c.get(id).await.expect("get")
}

const SIZE: u64 = 100_000;
const RATIO: f64 = 0.8;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn seeds_to_a_leecher_stops_at_ratio_and_survives_a_restart() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut seeder = Seeder::start(&root.path().join("seeder"));
    let magnet = seeder.info["magnet"].as_str().unwrap().to_owned();
    let mut cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    // Seeding is off by default: opt in explicitly.
    cfg.torrents.seed = true;
    cfg.torrents.seed_ratio = RATIO;
    let finished: Finished = Arc::default();

    // ---- first run: download from the seeder, then seed to leecher 1 ----
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    collect_finished(&c, finished.clone()).await;
    // Resolve the magnet to a .torrent first: dman stores its bytes, which a
    // restored seeding job needs (nobody else may be able to serve the metadata).
    let preview = c.preview_magnet(&magnet, Some(40)).await.unwrap();
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Torrent {
                name: "Album.torrent".into(),
                data_base64: preview.torrent_base64.clone(),
            }],
            options: JobOptions {
                // Slow enough that the restart happens mid-seeding.
                max_upload_limit: Some(40 * 1024),
                ..Default::default()
            },
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    let id = added(&out)[0];
    wait_jobs(&c, 90, "seeding", |j| job(j, id).state == JobState::Seeding).await;
    assert_eq!(job_now(&c, id).await.total_bytes, SIZE);
    // From now on dman is the only seed.
    seeder.cmd("stop-seeder");
    seeder.cmd(&format!("leecher {}", root.path().join("leech1").display()));
    let jobs = wait_jobs(&c, 60, "some upload", |j| {
        job(j, id).uploaded_bytes >= 25_000
    })
    .await;
    let before = job(&jobs, id).clone();
    assert_eq!(before.state, JobState::Seeding, "{before:?}");
    assert!(
        before.uploaded_bytes < (RATIO * SIZE as f64) as u64,
        "ratio reached before the restart; the test needs a slower upload: {before:?}"
    );
    let u1 = before.uploaded_bytes;
    d.stop(&c).await;

    // ---- second run: counter and ratio continue from the stored values ----
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    collect_finished(&c, finished.clone()).await;
    let after = job_now(&c, id).await;
    assert!(
        after.uploaded_bytes >= u1,
        "uploaded bytes were reset by the restart: {u1} -> {}",
        after.uploaded_bytes
    );
    // A fresh leecher (the old one lost its connection) announces to the tracker.
    seeder.cmd(&format!("leecher {}", root.path().join("leech2").display()));
    let limit = (RATIO * SIZE as f64) as u64;
    let jobs = wait_jobs(&c, 90, "seeding to end at the ratio", |j| {
        let j = job(j, id);
        // dman checks the ratio on every poll, counting the bytes of the first
        // run too, and ends seeding in that same poll: a job still seeding
        // never shows the ratio reached. A ratio restarted from zero would keep
        // it seeding up to `u1 + limit`.
        assert!(
            j.state != JobState::Seeding || j.uploaded_bytes < limit,
            "ratio was restarted from zero: before {u1}, still seeding at {}",
            j.uploaded_bytes
        );
        j.state == JobState::Complete
    })
    .await;
    let done = job(&jobs, id);
    assert!(done.uploaded_bytes >= limit, "{done:?}");
    // The final figure has no upper bound below a whole torrent: aria2's
    // upload limit is an average over several seconds, and after an idle
    // spell it answers all of a peer's queued block requests at once, so the
    // rest of the torrent can go out between two polls whatever the limit.
    assert!(done.uploaded_bytes <= u1 + SIZE, "{done:?}");
    assert!(done.ratio() >= RATIO, "{}", done.ratio());

    // Exactly one Completed and one SeedCompleted over both runs.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let fin = finished.lock().unwrap().clone();
    let n = |o: &Outcome| fin.iter().filter(|(i, x)| *i == id && x == o).count();
    assert_eq!(n(&Outcome::Completed), 1, "{fin:?}");
    assert_eq!(n(&Outcome::SeedCompleted), 1, "{fin:?}");
    // And nothing resumes seeding afterwards.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(job_now(&c, id).await.state, JobState::Complete);
    // Seeding after a restart leaves no control file or metadata copy behind.
    let mut left = Vec::new();
    let mut todo = vec![root.path().join("dl")];
    while let Some(dir) = todo.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if p
                .extension()
                .is_some_and(|x| x == "aria2" || x == "torrent")
            {
                left.push(p);
            }
        }
    }
    assert!(left.is_empty(), "engine files left behind: {left:?}");

    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}

/// Adds the local seeder's torrent to a daemon running `cfg`; returns the id.
async fn add_from_seeder(c: &Client, seeder: &mut Seeder, options: JobOptions) -> JobId {
    let magnet = seeder.info["magnet"].as_str().unwrap().to_owned();
    let preview = c.preview_magnet(&magnet, Some(40)).await.unwrap();
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Torrent {
                name: "Album.torrent".into(),
                data_base64: preview.torrent_base64.clone(),
            }],
            options,
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    added(&out)[0]
}

/// Waits for `id` to complete, failing if it is ever seen seeding.
async fn complete_without_seeding(c: &Client, id: JobId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let j = job_now(c, id).await;
        assert_ne!(j.state, JobState::Seeding, "{j:?}");
        assert_eq!(j.upload_speed, 0, "{j:?}");
        if j.state == JobState::Complete {
            assert_eq!(j.total_bytes, SIZE);
            assert_eq!(j.uploaded_bytes, 0, "{j:?}");
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {j:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn does_not_seed_by_default() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut seeder = Seeder::start(&root.path().join("seeder"));
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    assert!(!cfg.torrents.seed);
    let finished: Finished = Arc::default();
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    collect_finished(&c, finished.clone()).await;
    let id = add_from_seeder(&c, &mut seeder, JobOptions::default()).await;
    complete_without_seeding(&c, id).await;
    // A leecher asks dman's torrent for data; nothing may come from dman.
    seeder.cmd("stop-seeder");
    seeder.cmd(&format!("leecher {}", root.path().join("leech1").display()));
    tokio::time::sleep(Duration::from_secs(3)).await;
    let j = job_now(&c, id).await;
    assert_eq!(j.state, JobState::Complete, "{j:?}");
    assert_eq!(j.uploaded_bytes, 0, "{j:?}");
    let fin = finished.lock().unwrap().clone();
    let n = |o: &Outcome| fin.iter().filter(|(i, x)| *i == id && x == o).count();
    assert_eq!(n(&Outcome::Completed), 1, "{fin:?}");
    assert_eq!(n(&Outcome::SeedCompleted), 0, "{fin:?}");
    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}

/// A per-job `seed = false` beats `[torrents] seed = true`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn per_job_seed_false_overrides_config() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut seeder = Seeder::start(&root.path().join("seeder"));
    let mut cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    cfg.torrents.seed = true;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let id = add_from_seeder(
        &c,
        &mut seeder,
        JobOptions {
            seed: Some(false),
            ..Default::default()
        },
    )
    .await;
    complete_without_seeding(&c, id).await;
    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}

/// A job added from a magnet link keeps the metadata aria2 fetched: after a
/// daemon restart, with no other peer left to serve the metadata, it comes
/// back seeding from the stored `.torrent`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn magnet_job_survives_a_restart_from_its_stored_metadata() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut seeder = Seeder::start(&root.path().join("seeder"));
    let magnet = seeder.info["magnet"].as_str().unwrap().to_owned();
    let mut cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    cfg.torrents.seed = true;
    cfg.torrents.seed_ratio = 0.0; // seed until stopped

    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Uris {
                uris: vec![magnet.clone()],
            }],
            options: JobOptions::default(),
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    let id = added(&out)[0];
    wait_jobs(&c, 90, "seeding", |j| job(j, id).state == JobState::Seeding).await;
    // The metadata is captured on the tick after the follow-up; give it time
    // and make sure nothing else can serve it from now on.
    tokio::time::sleep(Duration::from_secs(2)).await;
    seeder.cmd("stop-seeder");
    d.stop(&c).await;

    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let jobs = wait_jobs(&c, 60, "seeding again after the restart", |j| {
        job(j, id).state == JobState::Seeding
    })
    .await;
    let j = job(&jobs, id);
    assert_eq!(j.total_bytes, SIZE, "{j:?}");
    assert_eq!(j.completed_bytes, SIZE, "{j:?}");
    assert_eq!(j.name, "Album", "{j:?}");
    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}

/// Live (non-zombie) aria2c processes whose command line mentions `root`.
fn aria2_pids(root: &Path) -> Vec<u32> {
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

fn assert_no_aria2(root: &Path) {
    let pids = aria2_pids(root);
    assert!(pids.is_empty(), "aria2c still running: {pids:?}");
}
