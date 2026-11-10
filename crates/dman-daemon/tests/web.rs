//! The browser UI served by the daemon: no login on loopback, with the invisible
//! CSRF/Origin/Host checks, token login on network binds, SSE, runtime control, safety.
//! No aria2 is needed; a missing engine is fine.

use std::path::Path;
use std::time::Duration;

use dman_client::Client;
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::WebStartParams;
use secrecy::ExposeSecret;
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
    c.aria2.tick_ms = 200;
    c
}

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    fn spawn(root: &Path, config: Config) -> Self {
        let DaemonTarget::Local { listen, .. } = &config.profiles["local"].daemon else {
            unreachable!()
        };
        let url = Url::parse(&format!("ws://{listen}/api/v1/ws")).unwrap();
        let paths = Paths::with_root(root);
        let task = tokio::spawn(run(DaemonOptions {
            paths: paths.clone(),
            config,
            profile: "local".into(),
        }));
        Self { paths, url, task }
    }

    async fn start(root: &Path, config: Config) -> (Self, Client) {
        let d = Self::spawn(root, config);
        for _ in 0..200 {
            if d.task.is_finished() {
                break;
            }
            if let Ok(c) = d.try_connect().await {
                return (d, c);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("daemon did not come up: {:?}", d.task.await);
    }

    fn secret(&self, name: &str) -> Option<String> {
        Secrets::load(&self.paths)
            .ok()?
            .get(name)
            .map(|s| s.expose_secret().to_owned())
    }

    async fn try_connect(&self) -> Result<Client, dman_client::ClientError> {
        let token = self.secret("daemon").unwrap_or_default();
        Client::connect(&self.url, token.into(), "dman-test").await
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

// ----- a minimal HTTP/1.1 client ----------------------------------------------

struct Resp {
    status: u16,
    head: String,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }
}

fn parse(raw: &[u8]) -> Resp {
    let text = String::from_utf8_lossy(raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Resp {
        status,
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

async fn http(port: u16, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut raw))
        .await
        .expect("response timed out")
        .unwrap();
    parse(&raw)
}

async fn get(port: u16, path: &str, cookie: Option<&str>) -> Resp {
    let h: Vec<_> = cookie
        .map(|c| ("Cookie", c.to_owned()))
        .into_iter()
        .collect();
    http(port, "GET", path, &h, "").await
}

fn csrf_of(body: &str) -> String {
    let key = "X-CSRF-Token&quot;:&quot;";
    let i = body.find(key).expect("csrf token in page") + key.len();
    body[i..i + body[i..].find("&quot;").unwrap()].to_owned()
}

async fn login(port: u16, base: &str, token: &str) -> String {
    let r = http(
        port,
        "POST",
        &format!("{base}/login"),
        &[("Content-Type", "application/x-www-form-urlencoded".into())],
        &format!("token={token}"),
    )
    .await;
    assert_eq!(r.status, 303, "{}", r.head);
    let c = r.header("set-cookie").expect("session cookie");
    assert!(
        c.contains("HttpOnly") && c.contains("SameSite=Strict"),
        "{c}"
    );
    c.split(';').next().unwrap().to_owned()
}

fn web_config(root: &Path) -> (Config, u16) {
    let mut c = config(root);
    let port = free_port();
    c.web.enabled = true;
    c.web.port = port;
    (c, port)
}

// ----- tests ------------------------------------------------------------------

async fn sse_first_event(port: u16, cookie: Option<&str>) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let cookie = cookie
        .map(|c| format!("Cookie: {c}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "GET /events?rows=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{cookie}Accept: text/event-stream\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains("event: stats") {
        let n = tokio::time::timeout_at(deadline, s.read(&mut buf))
            .await
            .expect("no SSE event")
            .unwrap();
        assert!(n > 0, "stream closed: {seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_opens_without_login_but_refuses_other_sites() {
    let root = tempfile::tempdir().unwrap();
    let (cfg, port) = web_config(root.path());
    let (d, client) = Daemon::start(root.path(), cfg).await;
    assert!(d.secret("web").is_none(), "no token on loopback");
    let st = client.web_status().await.unwrap();
    assert!(st.enabled && !st.auth && !st.exposed);

    // No cookie, no login page: the downloads page right away.
    let r = get(port, "/", None).await;
    assert_eq!(r.status, 200, "{}", r.head);
    assert!(r.body.contains("<title>") && !r.body.contains("Log out"));
    let r = get(port, "/login", None).await;
    assert_eq!((r.status, r.header("location")), (303, Some("/")));
    let csrf = csrf_of(&get(port, "/", None).await.body);

    // Same origin + the token from the page (htmx sends it by itself): works.
    let origin = |o: &str| {
        vec![
            ("Origin", o.to_owned()),
            ("X-CSRF-Token", csrf.clone()),
            ("HX-Request", "true".to_owned()),
        ]
    };
    let me = format!("http://127.0.0.1:{port}");
    let r = http(port, "POST", "/queue/pause-all", &origin(&me), "").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("paused"), "{}", r.body);
    // Without the token, or from another site (even with it): refused.
    let r = http(
        port,
        "POST",
        "/queue/pause-all",
        &[("Origin", me.clone())],
        "",
    )
    .await;
    assert_eq!(r.status, 403);
    let r = http(
        port,
        "POST",
        "/queue/resume-all",
        &origin("http://evil.example"),
        "",
    )
    .await;
    assert_eq!(r.status, 403);
    assert!(r.body.contains("cross-origin"), "{}", r.body);
    let mut site = origin(&me);
    site.push(("Sec-Fetch-Site", "cross-site".to_owned()));
    assert_eq!(
        http(port, "POST", "/queue/resume-all", &site, "")
            .await
            .status,
        403
    );

    // DNS rebinding: a foreign name in Host is refused, for reads too.
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let r = parse(&raw);
    assert_eq!(r.status, 421, "{}", r.head);
    assert!(!r.body.contains(&csrf));

    // Live updates need no cookie either.
    let seen = sse_first_event(port, None).await;
    assert!(seen.starts_with("HTTP/1.1 200") && seen.contains("text/event-stream"));

    d.stop(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_bind_requires_the_token_login() {
    let root = tempfile::tempdir().unwrap();
    let (mut cfg, port) = web_config(root.path());
    cfg.web.bind = "0.0.0.0".into();
    Secrets::load(&Paths::with_root(root.path()))
        .unwrap()
        .ensure("web")
        .unwrap();
    let (d, client) = Daemon::start(root.path(), cfg).await;
    let token = d.secret("web").expect("web token");
    assert_ne!(Some(token.clone()), d.secret("daemon"), "separate secrets");

    // Not logged in: redirect to the login page, which is public.
    let r = get(port, "/", None).await;
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("/login"));
    let r = get(port, "/login", None).await;
    assert_eq!(r.status, 200);
    assert!(!r.body.contains(&token));
    assert_eq!(get(port, "/static/htmx.min.js", None).await.status, 200);
    assert_eq!(get(port, "/events", None).await.status, 401);

    // Wrong token, then the right one.
    let r = http(
        port,
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded".into())],
        "token=nope",
    )
    .await;
    assert_eq!(r.status, 401);
    let cookie = login(port, "", &token).await;
    let r = get(port, "/", Some(&cookie)).await;
    assert_eq!(r.status, 200);
    assert!(r.body.contains("Log out"), "{}", r.body);
    let csrf = csrf_of(&r.body);

    let hdr = |csrf: Option<&str>| {
        let mut h = vec![
            ("Cookie", cookie.clone()),
            ("Origin", format!("http://127.0.0.1:{port}")),
        ];
        if let Some(c) = csrf {
            h.push(("X-CSRF-Token", c.to_owned()));
        }
        h
    };
    assert_eq!(
        http(port, "POST", "/queue/pause-all", &hdr(None), "")
            .await
            .status,
        403
    );
    let r = http(port, "POST", "/queue/resume-all", &hdr(Some(&csrf)), "").await;
    assert_eq!(r.status, 303, "{}", r.body);
    let seen = sse_first_event(port, Some(&cookie)).await;
    assert!(seen.starts_with("HTTP/1.1 200"));

    // Log out invalidates the cookie.
    let r = http(port, "POST", "/logout", &hdr(Some(&csrf)), "").await;
    assert_eq!(r.status, 303);
    assert_eq!(get(port, "/", Some(&cookie)).await.status, 303);
    d.stop(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn require_login_on_loopback_generates_a_token() {
    let root = tempfile::tempdir().unwrap();
    let (mut cfg, port) = web_config(root.path());
    cfg.web.require_login = true;
    let (d, client) = Daemon::start(root.path(), cfg).await;
    let token = d.secret("web").expect("token generated");
    assert_eq!(get(port, "/", None).await.status, 303);
    let cookie = login(port, "", &token).await;
    assert_eq!(get(port, "/", Some(&cookie)).await.status, 200);
    assert!(client.web_status().await.unwrap().auth);
    d.stop(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn non_loopback_without_token_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (mut cfg, _) = web_config(root.path());
    cfg.web.bind = "0.0.0.0".into();
    let d = Daemon::spawn(root.path(), cfg);
    let r = tokio::time::timeout(Duration::from_secs(20), d.task)
        .await
        .expect("daemon should refuse quickly")
        .unwrap();
    let e = r.expect_err("must refuse").to_string();
    assert!(e.contains("refusing to serve the web UI"), "{e}");
    assert!(
        Secrets::load(&d.paths).unwrap().get("web").is_none(),
        "no token is invented for an exposed bind"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_loopback_with_token_or_explicit_opt_out() {
    // With a token: starts, requires login.
    let root = tempfile::tempdir().unwrap();
    let (mut cfg, port) = web_config(root.path());
    cfg.web.bind = "0.0.0.0".into();
    Secrets::load(&Paths::with_root(root.path()))
        .unwrap()
        .ensure("web")
        .unwrap();
    let (d, c) = Daemon::start(root.path(), cfg).await;
    assert_eq!(get(port, "/", None).await.status, 303);
    let st = c.web_status().await.unwrap();
    assert!(st.enabled && st.exposed && st.auth);
    d.stop(&c).await;

    // allow_unauthenticated: no login, but it is reported as exposed without auth.
    let root = tempfile::tempdir().unwrap();
    let (mut cfg, port) = web_config(root.path());
    cfg.web.bind = "0.0.0.0".into();
    cfg.web.allow_unauthenticated = true;
    let (d, c) = Daemon::start(root.path(), cfg).await;
    let r = get(port, "/", None).await;
    assert_eq!(r.status, 200);
    assert!(
        r.body.to_lowercase().contains("exposed") || r.body.to_lowercase().contains("no access"),
        "exposed banner expected"
    );
    let st = c.web_status().await.unwrap();
    assert!(st.exposed && !st.auth);
    assert!(d.secret("web").is_none());
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn web_start_stop_over_the_api() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let (d, c) = Daemon::start(root.path(), cfg).await;

    let st = c.web_status().await.unwrap();
    assert!(!st.enabled && st.url.is_none() && !st.exposed);

    // An exposed address without a token is refused.
    let e = c
        .web_start(WebStartParams {
            bind: Some("0.0.0.0".into()),
            port: Some(free_port()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("refusing"), "{e}");
    let e = c
        .web_start(WebStartParams {
            bind: Some("not-an-ip".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("bind"), "{e}");

    let port = free_port();
    let st = c
        .web_start(WebStartParams {
            port: Some(port),
            persist: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(st.enabled && !st.auth && !st.exposed);
    assert_eq!(
        st.url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/").as_str())
    );
    assert_eq!(get(port, "/", None).await.status, 200);
    assert!(d.secret("web").is_none(), "no token for loopback");
    let text = std::fs::read_to_string(d.paths.config_file()).unwrap();
    assert!(
        text.contains(&format!("port = {port}")) && text.contains("enabled = true"),
        "{text}"
    );
    assert_eq!(c.web_status().await.unwrap().url, st.url);

    // Restart on another port.
    let port2 = free_port();
    c.web_start(WebStartParams {
        port: Some(port2),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(get(port2, "/", None).await.status, 200);
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());

    let st = c.web_stop(true).await.unwrap();
    assert!(!st.enabled);
    assert!(TcpStream::connect(("127.0.0.1", port2)).await.is_err());
    let text = std::fs::read_to_string(d.paths.config_file()).unwrap();
    assert!(text.contains("enabled = false"), "{text}");
    let cfg = Config::load(&d.paths).unwrap();
    cfg.validate().unwrap();
    assert!(!cfg.web.enabled);
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn base_path_is_applied() {
    let root = tempfile::tempdir().unwrap();
    let (mut cfg, port) = web_config(root.path());
    cfg.web.base_path = "/dman".into();
    let (d, c) = Daemon::start(root.path(), cfg).await;

    let r = get(port, "/dman/", None).await;
    assert_eq!(r.status, 200);
    assert!(r.body.contains("/dman/"), "links carry the base path");
    assert_eq!(get(port, "/dman", None).await.status, 200);
    let r = get(port, "/dman/login", None).await;
    assert_eq!(r.header("location"), Some("/dman/"));
    assert_eq!(
        get(port, "/dman/static/htmx.min.js", None).await.status,
        200
    );
    assert_ne!(get(port, "/", None).await.status, 200);
    let st = c.web_status().await.unwrap();
    assert_eq!(
        st.url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/dman").as_str())
    );
    d.stop(&c).await;
}
