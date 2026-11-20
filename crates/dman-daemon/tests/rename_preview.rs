//! Rename and magnet-preview tests against a real aria2c (plus a python
//! seeder fixture for the preview).
//!
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Secrets};
use dman_proto::method::{ListParams, RemoveParams};
use dman_proto::{AddParams, AddSource, ErrorCode, JobId, JobOptions, JobState};

mod support;
use support::{
    Daemon, added, aria2_enabled, assert_no_aria2, content, free_port, job, serve_files, wait_jobs,
};

// ----- daemon harness ---------------------------------------------------------

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

// ----- helpers ----------------------------------------------------------------

fn rpc_err(e: ClientError) -> dman_proto::RpcError {
    match e {
        ClientError::Rpc(r) => r,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

async fn aria2_client(d: &Daemon, rpc_port: u16) -> dman_aria2::Aria2Client {
    let secret = Secrets::load(&d.paths)
        .unwrap()
        .get("aria2-local")
        .expect("aria2 secret");
    dman_aria2::Aria2Client::connect(format!("127.0.0.1:{rpc_port}").parse().unwrap(), secret)
        .await
        .expect("connect to aria2")
}

/// Gids aria2 knows in any list.
async fn aria2_gids(a: &dman_aria2::Aria2Client) -> Vec<String> {
    let mut out = Vec::new();
    for st in a.tell_active(&["gid"]).await.unwrap() {
        out.push(st.gid.unwrap().as_str().to_owned());
    }
    for st in a.tell_waiting(0, 100, &["gid"]).await.unwrap() {
        out.push(st.gid.unwrap().as_str().to_owned());
    }
    for st in a.tell_stopped(0, 100, &["gid"]).await.unwrap() {
        out.push(st.gid.unwrap().as_str().to_owned());
    }
    out
}

fn metadata_entries(d: &Daemon) -> Vec<PathBuf> {
    let dir = d.paths.state_dir().join("metadata");
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn uri_source(u: String) -> AddSource {
    AddSource::Uris { uris: vec![u] }
}

async fn add_one(c: &Client, src: AddSource, options: JobOptions, paused: bool) -> JobId {
    let out = c
        .add(AddParams {
            sources: vec![src],
            options,
            paused,
            force: false,
        })
        .await
        .unwrap();
    added(&out)[0]
}

/// No aria2: preview reports EngineUnavailable; renaming an unknown job is NotFound.
#[tokio::test(flavor = "multi_thread")]
async fn preview_without_engine_is_engine_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(
        root.path(),
        "/nonexistent/aria2c",
        DiskThreshold::Absolute(0),
    );
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let e = rpc_err(c.preview_magnet(magnet, Some(2)).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::EngineUnavailable, "{e}");
    let e = rpc_err(c.rename(JobId(1), "x", None).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::NotFound);
    d.stop(&c).await;
}

// ----- rename -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn rename_waiting_complete_active_and_existing() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    const SIZE: usize = 200 * 1024;
    let files: HashMap<String, Vec<u8>> = ["a", "b", "c"]
        .iter()
        .enumerate()
        .map(|(i, n)| (format!("{n}.bin"), content(i as u8, SIZE)))
        .collect();
    let http = serve_files(files.clone()).await;
    let uri = |n: &str| format!("http://{http}/{n}.bin");
    let root = tempfile::tempdir().unwrap();
    let dl = root.path().join("dl");
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;

    // Paused: `out` is changed, validation refuses bad names.
    let a = add_one(&c, uri_source(uri("a")), JobOptions::default(), true).await;
    for bad in ["", "x/y", "..", ".", "a\nb"] {
        let e = rpc_err(c.rename(a, bad, None).await.unwrap_err());
        assert_eq!(e.code, ErrorCode::InvalidInput, "{bad:?}: {e}");
    }
    let e = rpc_err(c.rename(a, "x.bin", Some(2)).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e}");
    let e = rpc_err(c.rename(JobId(999), "x.bin", None).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::NotFound);
    c.rename(a, "renamed.bin", None).await.unwrap();
    assert_eq!(c.get(a).await.unwrap().name, "renamed.bin");
    let det = c.detail(a).await.unwrap();
    assert_eq!(det.options.out.as_deref(), Some("renamed.bin"));
    assert!(det.renames.is_empty(), "paused renames are not recorded");
    c.resume(&[a]).await.unwrap();
    wait_jobs(&c, 20, "a complete", |j| {
        job(j, a).state == JobState::Complete
    })
    .await;
    assert_eq!(
        std::fs::read(dl.join("renamed.bin")).unwrap(),
        files["a.bin"]
    );
    assert!(!dl.join("a.bin").exists());

    // Complete: moved on disk and recorded; never overwrites.
    let mut renamed = false;
    for _ in 0..30 {
        match c.rename(a, "final.bin", None).await {
            Ok(()) => {
                renamed = true;
                break;
            }
            Err(e) => {
                // File list is recorded a tick after completion.
                eprintln!("rename retry: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    assert!(renamed, "completed rename never succeeded");
    assert!(!dl.join("renamed.bin").exists());
    assert_eq!(std::fs::read(dl.join("final.bin")).unwrap(), files["a.bin"]);
    assert_eq!(c.get(a).await.unwrap().name, "final.bin");
    let det = c.detail(a).await.unwrap();
    assert_eq!(det.renames.len(), 1, "{:?}", det.renames);
    assert_eq!(det.renames[0].from, "renamed.bin");
    assert_eq!(det.renames[0].to, "final.bin");
    assert!(
        det.files.iter().any(|f| f.path.ends_with("final.bin")),
        "{:?}",
        det.files
    );

    // Existing target: refused, nothing overwritten.
    let b = add_one(&c, uri_source(uri("b")), JobOptions::default(), false).await;
    wait_jobs(&c, 20, "b complete", |j| {
        job(j, b).state == JobState::Complete
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let e = rpc_err(c.rename(b, "final.bin", None).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::Refused, "{e}");
    assert_eq!(std::fs::read(dl.join("final.bin")).unwrap(), files["a.bin"]);
    assert_eq!(std::fs::read(dl.join("b.bin")).unwrap(), files["b.bin"]);
    assert!(c.detail(b).await.unwrap().renames.is_empty());

    // Active: refused with the reason.
    let cc = add_one(
        &c,
        uri_source(uri("c")),
        JobOptions {
            max_download_limit: Some(20 * 1024),
            ..Default::default()
        },
        false,
    )
    .await;
    wait_jobs(&c, 10, "c active", |j| job(j, cc).state == JobState::Active).await;
    let e = rpc_err(c.rename(cc, "nope.bin", None).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::Refused);
    assert!(e.message.contains("pause it first"), "{e}");
    c.remove(RemoveParams {
        ids: vec![cc],
        delete_files: false,
        force: true,
    })
    .await
    .unwrap();
    wait_jobs(&c, 10, "c removed", |j| {
        job(j, cc).state == JobState::Removed
    })
    .await;
    let e = rpc_err(c.rename(cc, "nope.bin", None).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::Refused);

    d.stop(&c).await;
    assert_no_aria2(root.path());
}

// ----- magnet preview ---------------------------------------------------------

/// Unreachable magnet: times out with an error, leaves nothing behind (no
/// aria2 download, no job, no file), also when the client disconnects.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn magnet_preview_timeout_and_cancel_leave_nothing_behind() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    let rpc_port = cfg.aria2.rpc_port;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let a = aria2_client(&d, rpc_port).await;
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=nothing";

    // Not a magnet / bad timeout.
    let e = rpc_err(c.preview_magnet("http://x/a", None).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::InvalidInput);
    let e = rpc_err(c.preview_magnet(magnet, Some(0)).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::InvalidInput);

    // Timeout. The manager stays responsive while it waits.
    let c2 = d.client().await;
    let started = std::time::Instant::now();
    let slow = tokio::spawn(async move { c2.preview_magnet(magnet, Some(6)).await });
    // Wait for the preview to run in aria2 (polling: no fixed delay that a
    // busy machine could exceed).
    for _ in 0..30 {
        if aria2_gids(&a).await.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        aria2_gids(&a).await.len() == 1,
        "preview not running in aria2"
    );
    let t = std::time::Instant::now();
    c.list(ListParams::default()).await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(2), "manager blocked");
    let e = rpc_err(slow.await.unwrap().unwrap_err());
    assert!(e.message.contains("timed out"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(
        aria2_gids(&a).await.is_empty(),
        "{:?}",
        aria2_gids(&a).await
    );
    assert!(c.list(ListParams::default()).await.unwrap().is_empty());
    assert!(
        metadata_entries(&d).is_empty(),
        "{:?}",
        metadata_entries(&d)
    );

    // Client disconnects while waiting: force-removed and cleaned up.
    let c3 = d.client().await;
    let long = tokio::spawn(async move { c3.preview_magnet(magnet, Some(60)).await });
    for _ in 0..30 {
        if aria2_gids(&a).await.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(aria2_gids(&a).await.len(), 1);
    long.abort();
    let _ = long.await;
    let mut clean = false;
    for _ in 0..50 {
        if aria2_gids(&a).await.is_empty() && metadata_entries(&d).is_empty() {
            clean = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        clean,
        "gids {:?} files {:?}",
        aria2_gids(&a).await,
        metadata_entries(&d)
    );
    assert!(c.list(ListParams::default()).await.unwrap().is_empty());

    d.stop(&c).await;
    assert_no_aria2(root.path());
}

struct Seeder {
    child: std::process::Child,
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
        let out = child.stdout.take().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(out), &mut line).unwrap();
        let info =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("seeder said {line:?}: {e}"));
        Self { child, info }
    }
}

impl Drop for Seeder {
    fn drop(&mut self) {
        drop(self.child.stdin.take()); // the script shuts its aria2c down on EOF
        for _ in 0..50 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Full flow with a local tracker and seeder: preview a magnet, add the
/// returned torrent with one file selected, then rename in the finished job.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c and python3; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn magnet_preview_with_local_seeder_then_select_and_rename() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let seeder = Seeder::start(&root.path().join("seeder"));
    let magnet = seeder.info["magnet"].as_str().unwrap().to_owned();
    let info_hash = seeder.info["info_hash"].as_str().unwrap().to_owned();
    let cfg = config(root.path(), "aria2c", DiskThreshold::Absolute(0));
    let rpc_port = cfg.aria2.rpc_port;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let a = aria2_client(&d, rpc_port).await;

    let p = c.preview_magnet(&magnet, Some(40)).await.unwrap();
    assert_eq!(p.info_hash, info_hash);
    assert_eq!(p.name, "Album");
    assert_eq!(p.total_bytes, 100_000);
    let listed: Vec<(u32, &str, u64)> = p
        .files
        .iter()
        .map(|f| (f.index, f.path.as_str(), f.length))
        .collect();
    assert_eq!(
        listed,
        [
            (1, "01-intro.bin", 30000),
            (2, "sub/02-main.bin", 50000),
            (3, "03-end.bin", 20000)
        ]
    );
    assert!(p.files.iter().all(|f| f.selected));
    // Nothing is left behind and no job was created.
    assert!(
        aria2_gids(&a).await.is_empty(),
        "{:?}",
        aria2_gids(&a).await
    );
    assert!(metadata_entries(&d).is_empty());
    assert!(c.list(ListParams::default()).await.unwrap().is_empty());

    // Add the returned torrent with file 2 only.
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&p.torrent_base64)
        .unwrap();
    assert_eq!(&bytes[..1], b"d");
    let id = add_one(
        &c,
        AddSource::Torrent {
            name: "Album.torrent".into(),
            data_base64: p.torrent_base64.clone(),
        },
        JobOptions {
            select_files: Some(vec![2]),
            seed_time: Some(0),
            ..Default::default()
        },
        false,
    )
    .await;
    wait_jobs(&c, 60, "torrent complete", |j| {
        job(j, id).state == JobState::Complete
    })
    .await;
    let album = root.path().join("dl/Album");
    assert_eq!(
        std::fs::metadata(album.join("sub/02-main.bin"))
            .unwrap()
            .len(),
        50000
    );
    // (aria2 may touch unselected files that share a piece with a selected one.)

    // Rename the file, then the top-level directory (retry until the file list is recorded).
    async fn retry(c: &Client, id: JobId, name: &str, idx: Option<u32>) {
        for _ in 0..30 {
            match c.rename(id, name, idx).await {
                Ok(()) => return,
                Err(e) => {
                    eprintln!("rename retry: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
        panic!("rename {name} never succeeded");
    }
    // A multi-file job needs the file index or targets the directory.
    retry(&c, id, "main.bin", Some(2)).await;
    assert!(album.join("sub/main.bin").exists() && !album.join("sub/02-main.bin").exists());
    retry(&c, id, "Album2", None).await;
    assert!(root.path().join("dl/Album2/sub/main.bin").exists());
    assert!(!album.exists());
    let det = c.detail(id).await.unwrap();
    let r: Vec<(&str, &str)> = det
        .renames
        .iter()
        .map(|r| (r.from.as_str(), r.to.as_str()))
        .collect();
    assert_eq!(
        r,
        [
            ("Album/sub/02-main.bin", "Album/sub/main.bin"),
            ("Album", "Album2")
        ]
    );
    assert_eq!(c.get(id).await.unwrap().name, "Album2");

    d.stop(&c).await;
    drop(seeder);
    assert_no_aria2(root.path());
}
