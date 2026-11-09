//! Remote operation: built-in TLS on the API and web listeners, `wss://` clients with a
//! custom CA, and reconnecting after a daemon restart. No aria2 is needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dman_client::{Client, ClientError, ConnState, ReconnectingClient, TlsOptions};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets, TlsFiles};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::Event;
use dman_proto::{AddParams, AddSource};
use secrecy::{ExposeSecret, SecretString};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use url::Url;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A self-signed certificate for localhost/127.0.0.1 written to `dir`.
struct Pki {
    cert: PathBuf,
    key: PathBuf,
    der: Vec<u8>,
}

fn make_pki(dir: &Path) -> Pki {
    let ck =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(&cert, ck.cert.pem()).unwrap();
    std::fs::write(&key, ck.signing_key.serialize_pem()).unwrap();
    Pki {
        cert,
        key,
        der: ck.cert.der().to_vec(),
    }
}

fn tls_files(p: &Pki) -> TlsFiles {
    TlsFiles {
        cert: p.cert.display().to_string(),
        key: p.key.display().to_string(),
    }
}

fn config(root: &Path, port: u16) -> Config {
    let mut c = Config::default();
    let p = c.profiles.get_mut("local").unwrap();
    p.daemon = DaemonTarget::Local {
        listen: format!("127.0.0.1:{port}"),
        secret_ref: "daemon".into(),
        tls: None,
    };
    p.download_dir = root.join("dl").display().to_string();
    p.min_free = DiskThreshold::Absolute(0);
    p.warn_free = DiskThreshold::Absolute(0);
    c.aria2.path = "/nonexistent/aria2c".into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c
}

fn with_api_tls(mut c: Config, p: &Pki) -> Config {
    let DaemonTarget::Local { tls, .. } = &mut c.profiles.get_mut("local").unwrap().daemon else {
        unreachable!()
    };
    *tls = Some(tls_files(p));
    c
}

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    fn spawn(root: &Path, config: Config) -> Self {
        let DaemonTarget::Local { listen, tls, .. } = &config.profiles["local"].daemon else {
            unreachable!()
        };
        let scheme = if tls.is_some() { "wss" } else { "ws" };
        let url = Url::parse(&format!("{scheme}://{listen}/api/v1/ws")).unwrap();
        let paths = Paths::with_root(root);
        let task = tokio::spawn(run(DaemonOptions {
            paths: paths.clone(),
            config,
            profile: "local".into(),
        }));
        Self { paths, url, task }
    }

    fn token(&self) -> SecretString {
        Secrets::load(&self.paths)
            .ok()
            .and_then(|s| s.get("daemon"))
            .unwrap_or_else(|| SecretString::from(String::new()))
    }

    async fn start(root: &Path, config: Config, tls: &TlsOptions) -> (Self, Client) {
        let d = Self::spawn(root, config);
        for _ in 0..200 {
            if d.task.is_finished() {
                break;
            }
            if let Ok(c) = Client::connect_tls(&d.url, d.token(), "dman-test", tls).await {
                return (d, c);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("daemon did not come up: {:?}", d.task.await);
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

#[tokio::test(flavor = "multi_thread")]
async fn api_over_tls_with_custom_ca() {
    let root = tempfile::tempdir().unwrap();
    let pki = make_pki(root.path());
    let cfg = with_api_tls(config(root.path(), free_port()), &pki);
    cfg.validate().expect("tls files validate");
    let ca = TlsOptions::new().with_ca_file(&pki.cert).unwrap();
    let (d, client) = Daemon::start(root.path(), cfg, &ca).await;
    assert!(!client.daemon_version().is_empty(), "hello over wss");

    // Default roots do not know the self-signed certificate.
    let err = Client::connect(&d.url, d.token(), "t").await.unwrap_err();
    match &err {
        ClientError::Connect(m) => assert!(m.starts_with("certificate not trusted"), "{m}"),
        other => panic!("unexpected {other:?}"),
    }
    // Explicitly skipping verification works (with a warning in the log).
    let insecure = TlsOptions::new().insecure_skip_verify(true);
    Client::connect_tls(&d.url, d.token(), "t", &insecure)
        .await
        .expect("insecure connect");
    // A plaintext client cannot talk to the TLS port.
    let plain = Url::parse(&d.url.as_str().replacen("wss://", "ws://", 1)).unwrap();
    assert!(Client::connect(&plain, d.token(), "t").await.is_err());
    // Wrong token is still rejected over TLS.
    let bad = Client::connect_tls(&d.url, SecretString::from("nope".to_owned()), "t", &ca).await;
    assert_eq!(bad.unwrap_err(), ClientError::Unauthorized);

    d.stop(&client).await;
}

#[test]
fn config_rejects_unreadable_tls_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Config::default();
    let DaemonTarget::Local { tls, .. } = &mut c.profiles.get_mut("local").unwrap().daemon else {
        unreachable!()
    };
    *tls = Some(TlsFiles {
        cert: dir.path().join("missing.pem").display().to_string(),
        key: dir.path().join("missing.key").display().to_string(),
    });
    let e = c.validate().unwrap_err().to_string();
    assert!(e.contains("daemon.tls.cert"), "{e}");
    let mut c = Config::default();
    c.web.tls = Some(TlsFiles {
        cert: "/nonexistent/c.pem".into(),
        key: "/nonexistent/k.pem".into(),
    });
    assert!(c.validate().unwrap_err().to_string().contains("web.tls"));
}

// ----- web over https ---------------------------------------------------------

async fn https(pki: &Pki, port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(pki.der.clone()))
        .unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut s = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(name, tcp)
        .await
        .expect("TLS handshake with the pinned certificate");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text)
}

#[tokio::test(flavor = "multi_thread")]
async fn web_over_https_sets_secure_cookie() {
    let root = tempfile::tempdir().unwrap();
    let pki = make_pki(root.path());
    let mut cfg = config(root.path(), free_port());
    let web_port = free_port();
    cfg.web.enabled = true;
    cfg.web.port = web_port;
    cfg.web.tls = Some(tls_files(&pki));
    let (d, client) = Daemon::start(root.path(), cfg, &TlsOptions::new()).await;
    let token = Secrets::load(&d.paths)
        .unwrap()
        .get("web")
        .expect("web token")
        .expose_secret()
        .to_owned();

    let (status, text) = https(&pki, web_port, "GET", "/login", "").await;
    assert_eq!(status, 200, "{text}");
    assert!(text.to_lowercase().contains("token"), "login page: {text}");

    let (status, text) = https(&pki, web_port, "POST", "/login", &format!("token={token}")).await;
    assert_eq!(status, 303, "{text}");
    let cookie = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .expect("cookie");
    assert!(
        cookie.contains("; Secure") && cookie.contains("HttpOnly"),
        "{cookie}"
    );

    // Plain http to the https port gets no page.
    let mut s = TcpStream::connect(("127.0.0.1", web_port)).await.unwrap();
    s.write_all(b"GET /login HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut raw)).await;
    assert!(!String::from_utf8_lossy(&raw).contains("200 OK"));

    d.stop(&client).await;
}

// ----- reconnect --------------------------------------------------------------

async fn wait_state(
    rx: &mut tokio::sync::watch::Receiver<ConnState>,
    what: &str,
    f: impl Fn(&ConnState) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(30), rx.wait_for(|s| f(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_client_survives_daemon_restart_over_tls() {
    let root = tempfile::tempdir().unwrap();
    let pki = make_pki(root.path());
    let port = free_port();
    let mut cfg = with_api_tls(config(root.path(), port), &pki);
    // Adding a job needs a real aria2c; without one only the connection part is checked.
    let have_aria2 = std::process::Command::new("aria2c")
        .arg("--version")
        .output()
        .is_ok();
    if have_aria2 {
        cfg.aria2.path = "aria2c".into();
    }
    let expected = usize::from(have_aria2);
    let ca = TlsOptions::new().with_ca_file(&pki.cert).unwrap();
    let (d, client) = Daemon::start(root.path(), cfg.clone(), &ca).await;

    if have_aria2 {
        // A paused job, so it persists whatever the engine does.
        let outcome = client
            .add(AddParams {
                sources: vec![AddSource::Uris {
                    uris: vec!["http://127.0.0.1:1/file.bin".into()],
                }],
                options: Default::default(),
                paused: true,
            })
            .await
            .unwrap();
        assert!(
            matches!(outcome[0], dman_proto::AddOutcome::Added { .. }),
            "{outcome:?}"
        );
    }

    let rc = ReconnectingClient::spawn_tls(
        d.url.clone(),
        d.token(),
        "dman-test",
        Duration::from_millis(100),
        Duration::from_millis(500),
        ca,
    );
    let mut state = rc.state();
    let mut events = rc.subscribe();
    wait_state(&mut state, "connected", |s| {
        matches!(s, ConnState::Connected { .. })
    })
    .await;
    let first = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&first, Event::Snapshot { jobs, .. } if jobs.len() == expected),
        "{first:?}"
    );

    let token = d.token();
    d.stop(&client).await;
    wait_state(&mut state, "disconnected", |s| {
        matches!(s, ConnState::Disconnected { .. })
    })
    .await;
    let started = std::time::Instant::now();
    let err = rc.list(Default::default()).await.unwrap_err();
    assert_eq!(err, ClientError::Disconnected);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "calls fail fast"
    );

    // Same port, same directories: the persisted job comes back in a fresh snapshot.
    let d2 = Daemon::spawn(root.path(), cfg);
    assert_eq!(d2.token().expose_secret(), token.expose_secret());
    wait_state(&mut state, "reconnected", |s| {
        matches!(s, ConnState::Connected { .. })
    })
    .await;
    let snap = loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("snapshot after reconnect")
            .unwrap();
        if let Event::Snapshot { jobs, .. } = ev {
            break jobs;
        }
    };
    assert_eq!(snap.len(), expected, "persisted job in the new snapshot");
    assert_eq!(rc.list(Default::default()).await.unwrap().len(), expected);

    let c2 = rc.client().expect("connected");
    d2.stop(&c2).await;
}
