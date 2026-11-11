//! Duplicate detection on add, tracker merging for known torrents, http(s)
//! `.torrent` URLs added as torrents, and `jobs.set_options` on a paused job.
//!
//! Needs a real `aria2c`:
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-daemon --test dedupe -- --include-ignored --test-threads=1`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::{ListParams, RemoveParams};
use dman_proto::{
    AddOutcome, AddParams, AddSource, DuplicateInfo, ErrorCode, JobId, JobOptions, JobState,
    SourceKind,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use url::Url;

mod support;
use support::free_port;

fn aria2_enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

/// Serves fixed files with `200 OK` (no ranges needed here).
async fn serve_files(files: HashMap<String, Vec<u8>>) -> SocketAddr {
    let files = Arc::new(files);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(answer(s, files.clone()));
        }
    });
    addr
}

async fn answer(mut s: TcpStream, files: Arc<HashMap<String, Vec<u8>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().trim_start_matches('/');
    let path = target.split('?').next().unwrap_or_default();
    let Some(data) = files.get(path) else {
        let _ = s
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };
    let h = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        data.len()
    );
    if s.write_all(h.as_bytes()).await.is_err() {
        return;
    }
    if method != "HEAD" {
        let _ = s.write_all(data).await;
    }
    let _ = s.shutdown().await;
}

/// A one-file torrent with one tracker.
fn torrent(name: &str, tracker: &str) -> Vec<u8> {
    let mut info = format!(
        "d6:lengthi5e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
        name.len()
    )
    .into_bytes();
    info.extend_from_slice(&[7u8; 20]);
    info.push(b'e');
    let mut t = format!("d8:announce{}:{tracker}4:info", tracker.len()).into_bytes();
    t.extend_from_slice(&info);
    t.push(b'e');
    t
}

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
}

fn config(root: &Path) -> Config {
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
    c.aria2.path = "aria2c".into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c
}

impl Daemon {
    async fn start(root: &Path) -> Self {
        let config = config(root);
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

    async fn try_connect(&self) -> Result<Client, ClientError> {
        let token = Secrets::load(&self.paths)
            .ok()
            .and_then(|s| s.get("daemon"))
            .unwrap_or_else(|| secrecy::SecretString::from(String::new()));
        Client::connect(&self.url, token, "dman-test").await
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

async fn add(c: &Client, src: AddSource, force: bool) -> AddOutcome {
    let mut out = c
        .add(AddParams {
            sources: vec![src],
            options: JobOptions::default(),
            paused: true,
            force,
        })
        .await
        .expect("add call");
    assert_eq!(out.len(), 1);
    out.remove(0)
}

fn added(o: &AddOutcome) -> JobId {
    match o {
        AddOutcome::Added { id, .. } => *id,
        AddOutcome::Failed { error } => panic!("add failed: {error:?}"),
    }
}

fn duplicate(o: &AddOutcome) -> (DuplicateInfo, String) {
    match o {
        AddOutcome::Failed { error } => {
            assert_eq!(error.code, ErrorCode::Refused, "{error:?}");
            let info = DuplicateInfo::from_error(error).expect("duplicate data");
            (info, error.message.clone())
        }
        AddOutcome::Added { id, .. } => panic!("added {id} although it is a duplicate"),
    }
}

fn uris(u: &str) -> AddSource {
    AddSource::Uris {
        uris: vec![u.to_owned()],
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn duplicates_trackers_and_torrent_urls_with_real_aria2() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let t_bytes = torrent("x.bin", "udp://t1.invalid:1");
    let t_hash = dman_torrent::parse_torrent(&t_bytes).unwrap().info_hash;
    let http = serve_files(HashMap::from([
        ("f.bin".to_owned(), vec![1u8; 1000]),
        ("files/x.torrent".to_owned(), t_bytes.clone()),
        ("page.torrent".to_owned(), b"<html>login</html>".to_vec()),
    ]))
    .await;
    let root = tempfile::tempdir().unwrap();
    let d = Daemon::start(root.path()).await;
    let c = d.try_connect().await.unwrap();

    // The same URL twice: refused, naming the job; --force adds it anyway.
    let file = format!("http://{http}/f.bin");
    let a = added(&add(&c, uris(&file), false).await);
    let (info, msg) = duplicate(&add(&c, uris(&file), false).await);
    assert_eq!(
        (info.id, info.state, info.trackers_added),
        (a, JobState::Paused, 0)
    );
    assert_eq!(
        msg,
        format!("already in your downloads as job {a} (paused)")
    );
    let forced = added(&add(&c, uris(&file), true).await);
    assert_ne!(forced, a);

    // A magnet twice: the second one's new tracker joins the first.
    let magnet = format!(
        "magnet:?xt=urn:btih:{}&tr=udp%3A%2F%2Fa.invalid%3A1",
        "ab".repeat(20)
    );
    let m = added(&add(&c, uris(&magnet), false).await);
    let again = format!(
        "magnet:?xt=urn:btih:{}&tr=udp://a.invalid:1&tr=udp://b.invalid:2",
        "AB".repeat(20)
    );
    let (info, msg) = duplicate(&add(&c, uris(&again), false).await);
    assert_eq!((info.id, info.trackers_added), (m, 1), "{msg}");
    assert!(msg.contains("added 1 new tracker"), "{msg}");
    let det = c.detail(m).await.unwrap();
    assert_eq!(
        det.options
            .engine_options
            .get("bt-tracker")
            .map(String::as_str),
        Some("udp://b.invalid:2")
    );
    // Nothing new the second time.
    let (info, _) = duplicate(&add(&c, uris(&again), false).await);
    assert_eq!(info.trackers_added, 0);

    // An http(s) .torrent URL becomes a torrent job with its file list.
    let turl = format!("http://{http}/files/x.torrent?key=1");
    let pv = c.preview_magnet(&turl, Some(30)).await.unwrap();
    assert_eq!(pv.info_hash, t_hash);
    assert_eq!(pv.files.len(), 1);
    let t = added(&add(&c, uris(&turl), false).await);
    let job = c.get(t).await.unwrap();
    assert_eq!(job.kind, SourceKind::Torrent, "{job:?}");
    assert_eq!(job.source, "x.torrent");
    // The same torrent again, by URL or by magnet: a duplicate of that job.
    let (info, _) = duplicate(&add(&c, uris(&turl), false).await);
    assert_eq!(info.id, t);
    let (info, _) =
        duplicate(&add(&c, uris(&format!("magnet:?xt=urn:btih:{t_hash}")), false).await);
    assert_eq!(info.id, t);
    // A URL that does not serve a torrent fails clearly and adds nothing.
    let before = c.list(ListParams::default()).await.unwrap().len();
    match add(&c, uris(&format!("http://{http}/page.torrent")), false).await {
        AddOutcome::Failed { error } => {
            assert!(
                error.message.contains("not return a valid .torrent"),
                "{error:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(c.list(ListParams::default()).await.unwrap().len(), before);
    // No preview download is left behind as a job.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(c.list(ListParams::default()).await.unwrap().len(), before);

    // Changing the folder of a paused job works.
    let new_dir = root.path().join("elsewhere");
    c.set_options(
        a,
        JobOptions {
            dir: Some(new_dir.display().to_string()),
            max_download_limit: Some(1024),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let det = c.detail(a).await.unwrap();
    assert_eq!(det.job.dir, new_dir.display().to_string());
    assert_eq!(det.options.max_download_limit, Some(1024));

    // Removed jobs do not count.
    c.remove(RemoveParams {
        ids: vec![a, forced],
        delete_files: false,
        force: false,
    })
    .await
    .unwrap();
    added(&add(&c, uris(&file), false).await);

    d.stop(&c).await;
}
