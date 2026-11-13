//! Leftovers of finished jobs against a real aria2c: once a job is complete
//! (and, when seeding, done seeding), the download directory holds exactly the
//! downloaded files: no `.aria2` control file and no `<info hash>.torrent`
//! copy. Unfinished jobs keep theirs and still resume after a restart.
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

// ----- a one-file HTTP server -----------------------------------------------

async fn serve_one(data: Vec<u8>) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let data = Arc::new(data);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let data = data.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let len = data.len();
                let (status, a, b) = match head.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                }) {
                    Some(r) => {
                        let (a, b) = r.split_once('-').unwrap_or((&r, ""));
                        let a: usize = a.parse().unwrap_or(0);
                        let b: usize = b.parse().unwrap_or(len - 1).min(len - 1);
                        ("206 Partial Content", a, b)
                    }
                    None => ("200 OK", 0, len - 1),
                };
                let body = &data[a..=b];
                let mut h = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
                    body.len()
                );
                if status.starts_with("206") {
                    h.push_str(&format!("Content-Range: bytes {a}-{b}/{len}\r\n"));
                }
                h.push_str("\r\n");
                let _ = s.write_all(h.as_bytes()).await;
                let _ = s.write_all(body).await;
            });
        }
    });
    addr
}

/// Every file under `dir`, relative, sorted.
fn listing(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The files of the fixture torrent as they appear under the download dir.
fn album_files() -> Vec<String> {
    vec![
        "Album/01-intro.bin".into(),
        "Album/03-end.bin".into(),
        "Album/sub/02-main.bin".into(),
    ]
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Magnet,
    Torrent,
    TorrentPartial,
    MagnetPartial,
    Http,
}

/// Adds one job of `case`, lets it finish (and seed to a leecher when
/// `seeding`), and returns the download directory listing a few ticks later.
async fn finish_case(case: Case, seeding: bool) -> Vec<String> {
    let root = tempfile::tempdir().unwrap();
    let mut seeder = Seeder::start(&root.path().join("seeder"));
    let magnet = seeder.info["magnet"].as_str().unwrap().to_owned();
    let mut cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    if seeding {
        cfg.torrents.seed = true;
        cfg.torrents.seed_ratio = 0.3;
    }
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let http = serve_one(payload).await;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let source = match case {
        Case::Magnet | Case::MagnetPartial => AddSource::Uris { uris: vec![magnet] },
        Case::Torrent | Case::TorrentPartial => {
            let preview = c.preview_magnet(&magnet, Some(40)).await.unwrap();
            AddSource::Torrent {
                name: "Album.torrent".into(),
                data_base64: preview.torrent_base64,
            }
        }
        Case::Http => AddSource::Uris {
            uris: vec![format!("http://{http}/file.bin")],
        },
    };
    let out = c
        .add(AddParams {
            sources: vec![source],
            options: JobOptions {
                select_files: matches!(case, Case::TorrentPartial | Case::MagnetPartial)
                    .then(|| vec![1]),
                ..Default::default()
            },
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    let id = added(&out)[0];
    let is_bt = !matches!(case, Case::Http);
    if seeding && is_bt {
        wait_jobs(&c, 90, "seeding", |j| job(j, id).state == JobState::Seeding).await;
        seeder.cmd("stop-seeder");
        seeder.cmd(&format!("leecher {}", root.path().join("leech").display()));
    }
    wait_jobs(&c, 90, "complete", |j| {
        job(j, id).state == JobState::Complete
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let dl = root.path().join("dl");
    let files = listing(&dl);
    eprintln!("{case:?} seeding={seeding}: {files:?}");
    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
    files
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn finished_jobs_leave_only_the_downloaded_files() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let mut bad = Vec::new();
    for seeding in [false, true] {
        for case in [
            Case::Magnet,
            Case::Torrent,
            Case::TorrentPartial,
            Case::MagnetPartial,
            Case::Http,
        ] {
            let files = finish_case(case, seeding).await;
            let leftovers: Vec<&String> = files
                .iter()
                .filter(|f| f.ends_with(".aria2") || f.ends_with(".torrent"))
                .collect();
            let ok = match case {
                Case::Http => files == ["file.bin"],
                Case::Magnet | Case::Torrent => files == album_files(),
                // Only the selected file is wanted; aria2 may write pieces of
                // its neighbours too, which are not engine artefacts.
                Case::TorrentPartial | Case::MagnetPartial => {
                    leftovers.is_empty() && files.iter().any(|f| f == "Album/01-intro.bin")
                }
            };
            if !ok {
                bad.push(format!("{case:?} seeding={seeding}: {files:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "leftovers: {bad:#?}");
}

/// A `.torrent` that was in the directory before the job was added, or that
/// belongs to another torrent, is the user's and stays.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn user_torrent_files_in_the_directory_stay() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let seeder = Seeder::start(&root.path().join("seeder"));
    let hash = seeder.info["info_hash"].as_str().unwrap().to_owned();
    let dl = root.path().join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let same = dl.join(format!("{hash}.torrent"));
    let other = dl.join(format!("{}.torrent", "a".repeat(40)));
    std::fs::write(&same, b"mine").unwrap();
    std::fs::write(&other, b"also mine").unwrap();
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let preview = c
        .preview_magnet(seeder.info["magnet"].as_str().unwrap(), Some(40))
        .await
        .unwrap();
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Torrent {
                name: "Album.torrent".into(),
                data_base64: preview.torrent_base64,
            }],
            options: JobOptions::default(),
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    let id = added(&out)[0];
    wait_jobs(&c, 90, "complete", |j| {
        job(j, id).state == JobState::Complete
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(std::fs::read(&same).unwrap(), b"mine");
    assert_eq!(std::fs::read(&other).unwrap(), b"also mine");
    let mut want = album_files();
    want.push(format!("{hash}.torrent"));
    want.push(format!("{}.torrent", "a".repeat(40)));
    want.sort();
    assert_eq!(listing(&dl), want);
    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}

/// An unfinished job keeps its control file and resumes after a daemon
/// restart (re-added from the stored `.torrent`), then ends clean.
async fn resume_after_restart(case: Case) {
    let root = tempfile::tempdir().unwrap();
    let seeder = Seeder::start(&root.path().join("seeder"));
    let magnet = seeder.info["magnet"].as_str().unwrap().to_owned();
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    let dl = root.path().join("dl");
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let source = match case {
        Case::Torrent => {
            let preview = c.preview_magnet(&magnet, Some(40)).await.unwrap();
            AddSource::Torrent {
                name: "Album.torrent".into(),
                data_base64: preview.torrent_base64,
            }
        }
        _ => AddSource::Uris { uris: vec![magnet] },
    };
    let out = c
        .add(AddParams {
            sources: vec![source],
            options: JobOptions {
                max_download_limit: Some(12_000),
                ..Default::default()
            },
            paused: false,
            force: false,
        })
        .await
        .unwrap();
    let id = added(&out)[0];
    wait_jobs(&c, 90, "some data", |j| {
        job(j, id).completed_bytes >= 20_000
    })
    .await;
    // Let the metadata of a magnet job be stored, and the session saved.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_ne!(job_now(&c, id).await.state, JobState::Complete);
    let files = listing(&dl);
    assert!(
        files.iter().any(|f| f.ends_with(".aria2")),
        "the control file of an unfinished job must stay: {files:?}"
    );
    d.stop(&c).await;

    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    wait_jobs(&c, 120, "complete after the restart", |j| {
        job(j, id).state == JobState::Complete
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let j = job_now(&c, id).await;
    assert_eq!(j.completed_bytes, SIZE, "{j:?}");
    assert_eq!(listing(&dl), album_files());
    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn unfinished_torrent_resumes_after_a_restart() {
    if aria2_enabled() {
        resume_after_restart(Case::Torrent).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn unfinished_magnet_resumes_after_a_restart() {
    if aria2_enabled() {
        resume_after_restart(Case::Magnet).await;
    }
}
