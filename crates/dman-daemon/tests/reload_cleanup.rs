//! `config.reload` and `jobs.cleanup_unselected`, without aria2 (stored records
//! and a daemon whose engine is unavailable).

use std::path::Path;
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, RemoveUnselected, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::{Event, Level};
use dman_proto::{
    Engine, ErrorCode, JobId, JobOptions, JobState, RpcError, SourceKind, method::LimitsUpdate,
};
use dman_store::{Desired, JobRecord, Store};
use secrecy::SecretString;
use tokio::task::JoinHandle;
use url::Url;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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
    c.aria2.path = "/nonexistent/aria2c".into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 100;
    c
}

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
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

fn rpc(e: ClientError) -> RpcError {
    match e {
        ClientError::Rpc(r) => r,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

/// A finished 3-file torrent record with only file 1 selected.
fn record(id: u64, dir: &Path, files: &[&Path]) -> JobRecord {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "files".into(),
        serde_json::json!(
            files
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
        ),
    );
    extra.insert("details_final".into(), true.into());
    JobRecord {
        id: JobId(id),
        engine: Engine::Aria2,
        kind: SourceKind::Torrent,
        name: "Album".into(),
        dir: dir.display().to_string(),
        source: "Album.torrent".into(),
        options: JobOptions {
            select_files: Some(vec![1]),
            ..Default::default()
        },
        desired: Desired::Stopped,
        state: JobState::Complete,
        gid: None,
        info_hash: None,
        total_bytes: 10,
        completed_bytes: 10,
        uploaded_base: 0,
        uploaded_session: 0,
        added_at: 1,
        completed_at: Some(2),
        queue_key: id,
        error: None,
        has_blob: false,
        extra,
    }
}

fn preload(root: &Path, recs: &[JobRecord]) {
    let paths = Paths::with_root(root);
    std::fs::create_dir_all(paths.data_dir()).unwrap();
    let store = Store::open(paths.data_dir().join("local.redb")).unwrap();
    store.put_jobs(recs).unwrap();
}

async fn next_event(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    f: impl Fn(&Event) -> bool,
) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(e)) if f(&e) => return e,
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("expected event not seen: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_unselected_asks_then_deletes_only_unselected_files() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("dl");
    let album = dir.join("Album");
    std::fs::create_dir_all(album.join("sub")).unwrap();
    let (f1, f2, f3) = (
        album.join("01.bin"),
        album.join("sub/02.bin"),
        album.join("03.bin"),
    );
    std::fs::write(&f1, "0123456789").unwrap();
    std::fs::write(&f2, "abcde").unwrap();
    std::fs::write(&f3, "xyz").unwrap();
    let other = album.join("not-in-the-torrent.txt");
    std::fs::write(&other, "mine").unwrap();
    preload(root.path(), &[record(1, &dir, &[&f1, &f2, &f3])]);

    let mut cfg = config(root.path());
    cfg.torrents.remove_unselected = RemoveUnselected::Ask;
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut events = c.subscribe().await.expect("subscribe");

    // The daemon flags the job and tells us once; nothing is deleted yet.
    let e = next_event(&mut events, |e| matches!(e, Event::CleanupPending { .. })).await;
    let Event::CleanupPending {
        id, count, bytes, ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!((id, count, bytes), (JobId(1), 2, 8));
    assert!(f2.exists() && f3.exists());

    let dry = c.cleanup_unselected(JobId(1), true).await.unwrap();
    assert!(dry.dry_run && dry.deleted == 0 && dry.bytes == 8, "{dry:?}");
    let names: Vec<&str> = dry.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["Album/sub/02.bin", "Album/03.bin"]);
    assert!(f2.exists() && f3.exists(), "a dry run deletes nothing");

    let done = c.cleanup_unselected(JobId(1), false).await.unwrap();
    assert_eq!((done.deleted, done.bytes), (2, 8));
    assert!(f1.exists(), "selected files are never deleted");
    assert!(
        other.exists(),
        "files outside the torrent are never deleted"
    );
    assert!(!f2.exists() && !f3.exists());
    assert!(!album.join("sub").exists(), "emptied directory is pruned");
    assert!(album.exists());
    // Nothing left: a second call is a no-op.
    let again = c.cleanup_unselected(JobId(1), false).await.unwrap();
    assert_eq!((again.deleted, again.files.len()), (0, 0));

    let e = rpc(c.cleanup_unselected(JobId(99), true).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::NotFound);
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_is_off_by_default_and_refused_while_running() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("dl");
    std::fs::create_dir_all(&dir).unwrap();
    let (f1, f2) = (dir.join("a"), dir.join("b"));
    std::fs::write(&f1, "a").unwrap();
    std::fs::write(&f2, "b").unwrap();
    let mut running = record(2, &dir, &[&f1, &f2]);
    running.state = JobState::Paused;
    running.desired = Desired::Paused;
    preload(root.path(), &[record(1, &dir, &[&f1, &f2]), running]);
    let d = Daemon::start(root.path(), config(root.path())).await; // "never"
    let c = d.client().await;
    let mut events = c.subscribe().await.unwrap();
    // No flag with the default setting.
    let seen = tokio::time::timeout(
        Duration::from_millis(800),
        next_event(&mut events, |e| matches!(e, Event::CleanupPending { .. })),
    )
    .await;
    assert!(
        seen.is_err(),
        "no CleanupPending when remove_unselected = never"
    );
    // An unfinished torrent is refused.
    let e = rpc(c.cleanup_unselected(JobId(2), false).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e}");
    assert!(f2.exists());
    // An explicit request still works (the user is asking), for the finished one.
    let r = c.cleanup_unselected(JobId(1), false).await.unwrap();
    assert_eq!(r.deleted, 1);
    assert!(f1.exists() && !f2.exists());
    d.stop(&c).await;
}

fn write_config(root: &Path, cfg: &Config) {
    let paths = Paths::with_root(root);
    std::fs::write(paths.config_file(), toml::to_string(cfg).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn config_reload_applies_live_settings_and_rejects_bad_files() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    write_config(root.path(), &cfg);
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    assert_eq!(c.limits_get().await.unwrap().max_concurrent, 3);
    let mut events = c.subscribe().await.unwrap();

    // Nothing changed.
    let r = c.config_reload().await.unwrap();
    assert!(
        r.applied.is_empty() && r.restart_required.is_empty(),
        "{r:?}"
    );

    // Limits, schedule, torrents and notify change live; aria2 and web need a restart.
    let mut new = cfg.clone();
    new.limits.max_concurrent = 7;
    new.limits.download = dman_config::ByteLimit(5 << 20);
    new.torrents.remove_unselected = RemoveUnselected::Ask;
    new.notify.desktop_on_daemon = false;
    new.notify.on = vec![dman_config::NotifyEvent::Failed];
    new.aria2.rpc_port = free_port();
    new.web.port = 7999;
    new.schedule = toml::from_str::<Wrap>(
        r#"
[[schedule]]
name = "always"
start = "00:00"
end = "23:59"
upload_limit = "1MiB"
"#,
    )
    .unwrap()
    .schedule;
    write_config(root.path(), &new);
    let r = c.config_reload().await.unwrap();
    assert_eq!(
        r.applied,
        ["limits", "schedule", "torrents", "notify"],
        "{r:?}"
    );
    assert_eq!(r.restart_required.len(), 2, "{r:?}");
    assert!(r.restart_required[0].starts_with("aria2"));
    assert!(r.restart_required[1].starts_with("web"));
    let l = c.limits_get().await.unwrap();
    assert_eq!(
        (l.max_concurrent, l.download, l.upload),
        (7, 5 << 20, 1 << 20)
    );
    let s = c.schedule().await.unwrap();
    assert_eq!(s.active.as_deref(), Some("always"));
    assert_eq!(s.base.as_ref().map(|b| b.upload), Some(0));
    next_event(&mut events, |e| {
        matches!(e, Event::Message { level: Level::Warn, text } if text.contains("restart required"))
    })
    .await;

    // Reloading the same file again is a no-op, restart-only items keep being reported.
    let r = c.config_reload().await.unwrap();
    assert!(r.applied.is_empty(), "{r:?}");
    assert_eq!(r.restart_required.len(), 2);

    // A broken file is refused and the old configuration stays.
    std::fs::write(
        Paths::with_root(root.path()).config_file(),
        "limits = [oops",
    )
    .unwrap();
    let e = rpc(c.config_reload().await.unwrap_err());
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e}");
    let mut bad = new.clone();
    bad.schedule[0].name = String::new();
    write_config(root.path(), &bad);
    let e = rpc(c.config_reload().await.unwrap_err());
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e}");
    let l = c.limits_get().await.unwrap();
    assert_eq!(l.max_concurrent, 7);

    // Removing the schedule restores the base limits.
    let mut back = new.clone();
    back.schedule.clear();
    write_config(root.path(), &back);
    let r = c.config_reload().await.unwrap();
    assert!(r.applied.contains(&"schedule".to_owned()));
    let l = c.limits_get().await.unwrap();
    assert_eq!((l.upload, l.download), (0, 5 << 20));
    assert_eq!(c.schedule().await.unwrap().active, None);
    let _ = LimitsUpdate::default();
    d.stop(&c).await;
}

#[derive(serde::Deserialize)]
struct Wrap {
    schedule: Vec<dman_config::ScheduleWindow>,
}
