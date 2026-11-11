//! Secrets never reach the log: the daemon runs with every log level enabled
//! (TRACE, all targets) while it authenticates clients, rejects a wrong token,
//! serves the web UI with a login token, signs a webhook and (with
//! `DMAN_TEST_ARIA2=1`) drives a real aria2c through a download; afterwards
//! neither the captured log nor aria2's own log file contains the API token,
//! the aria2 RPC secret, the web token, the webhook secret or the webhook
//! URL's password.
//!
//! The subscriber is process-global, so this file holds exactly one test.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dman_client::Client;
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets, Webhook};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::ListParams;
use dman_proto::{AddOutcome, AddParams, AddSource, JobOptions, JobState};
use secrecy::{ExposeSecret, SecretString};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

fn aria2_enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

mod support;
use support::free_port;

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// One file over plain HTTP (no keep-alive, no ranges).
async fn serve_one(body: Vec<u8>) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let body = Arc::new(body);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                if !buf.starts_with(b"HEAD") {
                    let _ = s.write_all(&body).await;
                }
                let _ = s.shutdown().await;
            });
        }
    });
    port
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
    c.aria2.path = if aria2_enabled() {
        "aria2c".into()
    } else {
        root.join("no-aria2c").display().to_string()
    };
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c.web.enabled = true;
    c.web.require_login = true;
    c.web.port = free_port();
    c.notify.webhook = Some(Webhook {
        url: "http://hookuser:hook-url-password-0451@127.0.0.1:1/hook".into(),
        timeout_secs: 1,
        retries: 0,
        secret_ref: Some("hook".into()),
    });
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_appear_in_the_log() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(root.path());
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    // The webhook signing secret must exist before the daemon starts.
    Secrets::load(&paths).unwrap().ensure("hook").unwrap();
    let cfg = config(root.path());
    let web_ref = cfg.web.token_ref.clone();
    let DaemonTarget::Local { listen, .. } = &cfg.profiles["local"].daemon else {
        unreachable!()
    };
    let url = Url::parse(&format!("ws://{listen}/api/v1/ws")).unwrap();
    let task = tokio::spawn(run(DaemonOptions {
        paths: paths.clone(),
        config: cfg,
        profile: "local".into(),
    }));
    let secret = |name: &str| {
        Secrets::load(&paths)
            .ok()
            .and_then(|s| s.get(name))
            .map(|s| s.expose_secret().to_owned())
    };
    let mut client = None;
    for _ in 0..200 {
        assert!(!task.is_finished(), "daemon stopped: {}", captured.text());
        if let Some(t) = secret("daemon")
            && let Ok(c) = Client::connect(&url, SecretString::from(t), "secrets-test").await
        {
            client = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let c = client.expect("daemon did not come up");

    // A wrong token is rejected (and logged as such).
    let wrong = Client::connect(&url, SecretString::from("wrong-token"), "intruder").await;
    assert!(wrong.is_err());

    // Exercise the engine and the notifier (the webhook fails: nothing listens).
    let body = vec![7u8; 64 * 1024];
    let port = serve_one(body).await;
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Uris {
                uris: vec![format!("http://127.0.0.1:{port}/file.bin")],
            }],
            options: JobOptions::default(),
            paused: false,
            force: false,
        })
        .await;
    if aria2_enabled() {
        let id = match &out.expect("add")[0] {
            AddOutcome::Added { id, .. } => *id,
            AddOutcome::Failed { error } => panic!("add failed: {error}"),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let jobs = c.list(ListParams::default()).await.unwrap();
            if jobs
                .iter()
                .any(|j| j.id == id && j.state == JobState::Complete)
            {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "{jobs:#?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Let the webhook attempt fail and be reported.
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }

    c.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("daemon did not stop")
        .unwrap()
        .unwrap();

    let log = captured.text();
    assert!(
        log.contains("dman daemon starting"),
        "nothing captured: {log}"
    );
    assert!(log.contains("wrong token"), "{log}");
    let mut secrets = vec![
        ("API token", secret("daemon").unwrap()),
        ("aria2 secret", secret("aria2-local").unwrap()),
        ("web token", secret(&web_ref).expect("web token generated")),
        ("webhook secret", secret("hook").unwrap()),
    ];
    secrets.push(("webhook URL password", "hook-url-password-0451".into()));
    let aria2_log =
        std::fs::read_to_string(paths.state_dir().join("aria2-local.log")).unwrap_or_default();
    if aria2_enabled() {
        assert!(log.contains("job added"), "{log}");
        assert!(!aria2_log.is_empty(), "aria2 wrote no log");
    }
    for (what, value) in &secrets {
        assert!(value.len() >= 16, "{what} is suspiciously short");
        assert!(
            !log.contains(value.as_str()),
            "{what} appears in the daemon log"
        );
        assert!(
            !aria2_log.contains(value.as_str()),
            "{what} appears in aria2's log"
        );
    }
}
