use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dman_proto::method::{BatchResult, Event};
use dman_proto::search::ProviderInfo;
use dman_proto::{
    AddOutcome, Engine, FileInfo, GlobalStats, Job, JobDetail, JobId, JobState, Limits, RpcError,
    SourceKind, TrackerInfo, TrackerStatus,
};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

use crate::{Backend, WebConfig, router};

const EVIL: &str = "<script>alert(1)</script>";

fn job(id: u64, name: &str, state: JobState, completed: u64) -> Job {
    Job {
        id: JobId(id),
        engine: Engine::Aria2,
        kind: SourceKind::Http,
        name: name.into(),
        state,
        dir: "/data".into(),
        source: "http://example.org/f".into(),
        total_bytes: 1000,
        completed_bytes: completed,
        uploaded_bytes: 0,
        download_speed: 100,
        upload_speed: 0,
        connections: 2,
        seeders: None,
        info_hash: None,
        added_at: 1_700_000_000,
        completed_at: None,
        queue_position: None,
        error: None,
    }
}

struct Fake {
    calls: Mutex<Vec<(String, Value)>>,
    tx: broadcast::Sender<Event>,
    jobs: Vec<Job>,
    video: bool,
    /// `video.probe` answer (`Null` = not configured; `{"error": ".."}` = fails).
    probe: Mutex<Value>,
    /// `video.preferences.get` answer (`Null` = not configured).
    prefs: Mutex<Value>,
    /// `jobs.remove` fails with a refusal.
    refuse_remove: Mutex<bool>,
}

impl Fake {
    fn new(video: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            tx: broadcast::channel(64).0,
            jobs: vec![
                job(1, "ubuntu.iso", JobState::Active, 250),
                job(2, EVIL, JobState::Paused, 0),
                job(3, "gone.bin", JobState::Removed, 1000),
            ],
            video,
            probe: Mutex::new(Value::Null),
            prefs: Mutex::new(Value::Null),
            refuse_remove: Mutex::new(false),
        })
    }

    fn set_probe(&self, v: impl serde::Serialize) {
        *self.probe.lock().unwrap() = serde_json::to_value(v).unwrap();
    }

    fn set_prefs(&self, ask: bool) {
        *self.prefs.lock().unwrap() = json!({
            "ask_quality": ask, "default_quality": "best", "default_audio": "best"
        });
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        let c = self.calls.lock().unwrap();
        c.iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

fn ok<T: serde::Serialize>(v: T) -> Result<Value, RpcError> {
    Ok(serde_json::to_value(v).unwrap())
}

impl Backend for Fake {
    fn call(&self, method: &str, params: Value) -> BoxFuture<'static, Result<Value, RpcError>> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        let jobs = self.jobs.clone();
        let video = self.video;
        let probe = self.probe.lock().unwrap().clone();
        let prefs = self.prefs.lock().unwrap().clone();
        let refuse_remove = *self.refuse_remove.lock().unwrap();
        let method = method.to_string();
        Box::pin(async move {
            match method.as_str() {
                "jobs.list" => ok(jobs),
                "jobs.get" => {
                    let id = params["id"].as_u64().unwrap();
                    jobs.iter()
                        .find(|j| j.id.0 == id)
                        .map_or_else(|| Err(RpcError::not_found("job")), ok)
                }
                "jobs.detail" if params["id"] != 1 && params["id"] != 4 => {
                    Err(RpcError::not_found("job"))
                }
                // Job 4: a finished torrent with a deselected file.
                "jobs.detail" if params["id"] == 4 => ok(JobDetail {
                    job: Job {
                        id: JobId(4),
                        kind: SourceKind::Torrent,
                        state: JobState::Complete,
                        completed_bytes: 1000,
                        ..jobs[0].clone()
                    },
                    files: vec![
                        FileInfo {
                            index: 1,
                            path: "keep.bin".into(),
                            length: 10,
                            completed: 10,
                            selected: true,
                        },
                        FileInfo {
                            index: 2,
                            path: EVIL.into(),
                            length: 10,
                            completed: 3,
                            selected: false,
                        },
                    ],
                    peers: vec![],
                    trackers: vec![],
                    pieces: None,
                    info_hash: None,
                    comment: None,
                    options: Default::default(),
                    speed_history: vec![],
                    renames: vec![],
                }),
                "jobs.cleanup_unselected" => ok(json!({
                    "files": [{"path": EVIL, "bytes": 3}],
                    "bytes": 3,
                    "deleted": if params["dry_run"] == true { 0 } else { 1 },
                    "dry_run": params["dry_run"],
                })),
                "config.reload" => ok(json!({
                    "applied": ["limits", "schedule"],
                    "restart_required": ["web.port"],
                })),
                "jobs.detail" => ok(JobDetail {
                    job: Job {
                        kind: SourceKind::Magnet,
                        ..jobs[0].clone()
                    },
                    files: vec![FileInfo {
                        index: 1,
                        path: "a/b.bin".into(),
                        length: 10,
                        completed: 5,
                        selected: true,
                    }],
                    peers: vec![],
                    trackers: vec![TrackerInfo {
                        url: "http://t/announce".into(),
                        tier: 0,
                        status: TrackerStatus::Unknown,
                    }],
                    pieces: None,
                    info_hash: None,
                    comment: None,
                    options: Default::default(),
                    speed_history: vec![],
                    renames: vec![],
                }),
                "stats.get" => ok(GlobalStats {
                    download_speed: 2048,
                    num_active: 1,
                    ..Default::default()
                }),
                "limits.get" => ok(Limits {
                    download: 0,
                    upload: 0,
                    max_concurrent: 3,
                }),
                "providers.list" => ok(Vec::<ProviderInfo>::new()),
                "search.history" => ok(Vec::<Value>::new()),
                "engine.status" => ok(json!([
                    {"engine":"aria2","available":true},
                    {"engine":"yt_dlp","available":video}
                ])),
                "jobs.add" => ok(vec![AddOutcome::Added {
                    id: JobId(99),
                    extra_ids: vec![],
                }]),
                "jobs.preview_magnet" => ok(json!({
                    "info_hash": "ab",
                    "name": "Big <b>Pack</b>",
                    "total_bytes": 30,
                    "files": [
                        {"index":1,"path":"a.mkv","length":10,"completed":0,"selected":true},
                        {"index":2,"path":EVIL,"length":20,"completed":0,"selected":true}
                    ],
                    "torrent_base64": "ZDQ6aW5mb2U="
                })),
                "fs.list" if params["path"] == "/nope" => {
                    Err(RpcError::not_found("no such folder"))
                }
                "fs.list" => ok(json!({
                    "path": params["path"].as_str().unwrap_or("/data"),
                    "parent": "/",
                    "entries": [
                        {"name":"movies","is_dir":true},
                        {"name":EVIL,"is_dir":true},
                        {"name":"file.txt","is_dir":false,"size":3}
                    ]
                })),
                "video.preferences.get" | "video.preferences.set" => Ok(prefs),
                "video.probe" if probe["error"].is_string() => Err(RpcError::new(
                    dman_proto::ErrorCode::General,
                    probe["error"].as_str().unwrap(),
                )),
                "video.probe" => Ok(probe),
                "jobs.remove" if refuse_remove => Err(RpcError::new(
                    dman_proto::ErrorCode::Refused,
                    "the folder is read-only",
                )),
                "jobs.pause" | "jobs.resume" | "jobs.remove" | "jobs.purge" | "jobs.restore" => {
                    ok(BatchResult {
                        ok: vec![JobId(1)],
                        failed: vec![],
                    })
                }
                _ => Ok(Value::Null),
            }
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

fn app_with(fake: &Arc<Fake>, token: Option<&str>, base: &str) -> Router {
    router(
        fake.clone(),
        WebConfig {
            base_path: base.into(),
            token: token.map(|t| SecretString::from(t.to_string())),
            title: "dman".into(),
            exposed: false,
            tls: false,
            ..WebConfig::default()
        },
    )
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let r = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = r.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 24).await.unwrap();
    Resp {
        status: parts.status,
        headers: parts.headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::HOST, "localhost")
        .body(Body::empty())
        .unwrap()
}

fn post_form(uri: &str, csrf: &str, form: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("x-csrf-token", csrf)
        .body(Body::from(form.to_string()))
        .unwrap()
}

/// CSRF token embedded in a page (`hx-headers` attribute).
fn csrf_of(body: &str) -> String {
    let key = "X-CSRF-Token&quot;:&quot;";
    let i = body.find(key).expect("csrf token in page") + key.len();
    body[i..i + body[i..].find("&quot;").unwrap()].to_string()
}

async fn csrf(app: &Router) -> String {
    csrf_of(&send(app, get("/")).await.body)
}

#[tokio::test]
async fn auth_required_when_token_set() {
    let app = app_with(&Fake::new(true), Some("s3cret"), "/");
    let r = send(&app, get("/")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/login");
    assert_eq!(
        send(&app, get("/events")).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, get("/jobs/1")).await.status,
        StatusCode::SEE_OTHER
    );
    let r = send(
        &app,
        Request::post("/queue/pause-all")
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // The login page and static files stay reachable.
    let r = send(&app, get("/login")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("type=\"password\""));
    assert_eq!(
        send(&app, get("/static/app.css")).await.status,
        StatusCode::OK
    );
}

fn login_req(token: &str) -> Request<Body> {
    Request::post("/login")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("token={token}")))
        .unwrap()
}

#[tokio::test]
async fn login_sets_cookie_and_grants_access() {
    let app = app_with(&Fake::new(true), Some("s3cret"), "/");
    let r = send(&app, login_req("s3cret")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let cookie = r.headers[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(cookie.starts_with("dman_session="), "{cookie}");
    for flag in ["HttpOnly", "SameSite=Strict", "Max-Age=604800", "Path=/"] {
        assert!(cookie.contains(flag), "{flag} in {cookie}");
    }
    assert!(!cookie.contains("Secure"));
    assert!(!cookie.contains("s3cret"));
    let pair = cookie.split(';').next().unwrap();
    let r = send(
        &app,
        Request::get("/")
            .header(header::HOST, "localhost")
            .header(header::COOKIE, pair)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Log out"));
    // A forged session id is not accepted.
    let r = send(
        &app,
        Request::get("/")
            .header(header::HOST, "localhost")
            .header(header::COOKIE, "dman_session=nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn login_cookie_is_secure_behind_https_proxy() {
    let app = app_with(&Fake::new(true), Some("s3cret"), "/");
    let mut req = login_req("s3cret");
    req.headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    let r = send(&app, req).await;
    assert!(
        r.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
}

#[tokio::test]
async fn login_cookie_is_secure_when_listener_has_tls() {
    let app = router(
        Fake::new(true),
        WebConfig {
            token: Some(SecretString::from("s3cret".to_string())),
            tls: true,
            ..WebConfig::default()
        },
    );
    let r = send(&app, login_req("s3cret")).await;
    assert!(
        r.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
}

#[tokio::test]
async fn wrong_token_is_rejected() {
    let app = app_with(&Fake::new(true), Some("s3cret"), "/");
    let r = send(&app, login_req("guess")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.headers.get(header::SET_COOKIE).is_none());
    assert!(r.body.contains("Wrong token"));
    assert!(!r.body.contains("s3cret"));
}

#[tokio::test]
async fn bearer_header_authenticates_without_csrf() {
    let fake = Fake::new(true);
    let app = app_with(&fake, Some("s3cret"), "/");
    let bearer = |t: &str| format!("Bearer {t}");
    let r = send(
        &app,
        Request::get("/")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, bearer("s3cret"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &app,
        Request::get("/")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, bearer("wrong"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = send(
        &app,
        Request::post("/jobs/1/pause")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, bearer("s3cret"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(fake.calls("jobs.pause"), vec![json!({"ids":[1]})]);
}

#[tokio::test]
async fn cross_origin_and_tokenless_posts_are_refused() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let token = csrf(&app).await;
    let mut evil = post_form("/queue/pause-all", &token, "");
    evil.headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    assert_eq!(send(&app, evil).await.status, StatusCode::FORBIDDEN);
    let mut null = post_form("/queue/pause-all", &token, "");
    null.headers_mut()
        .insert(header::ORIGIN, "null".parse().unwrap());
    assert_eq!(send(&app, null).await.status, StatusCode::FORBIDDEN);
    let mut site = post_form("/queue/pause-all", &token, "");
    site.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    assert_eq!(send(&app, site).await.status, StatusCode::FORBIDDEN);
    // Right origin, wrong or missing CSRF token.
    assert_eq!(
        send(&app, post_form("/queue/pause-all", "bad", ""))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let no_token = Request::post("/queue/pause-all")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, no_token).await.status, StatusCode::FORBIDDEN);
    assert!(fake.calls("queue.pause_all").is_empty());
    // Same origin with the token goes through (also via the form field).
    assert_eq!(
        send(&app, post_form("/queue/pause-all", &token, ""))
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    let field = Request::post("/search/history/clear")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={token}")))
        .unwrap();
    assert_eq!(send(&app, field).await.status, StatusCode::SEE_OTHER);
    assert_eq!(fake.calls("queue.pause_all").len(), 1);
}

#[tokio::test]
async fn base_path_prefixes_everything() {
    let app = app_with(&Fake::new(true), None, "/dman/");
    for uri in ["/dman/", "/dman"] {
        let r = send(&app, get(uri)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        for want in [
            "href=\"/dman/search\"",
            "href=\"/dman/static/app.css?v=",
            "src=\"/dman/static/htmx.min.js?v=",
            "sse-connect=\"/dman/events?rows=1\"",
            "hx-get=\"/dman/downloads/table\"",
            "href=\"/dman/jobs/1\"",
            "hx-post=\"/dman/jobs/1/pause\"",
        ] {
            assert!(r.body.contains(want), "{want} missing in {uri}");
        }
        assert!(!r.body.contains("href=\"/search\""));
    }
    assert_eq!(send(&app, get("/")).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        send(&app, get("/dman/static/app.css")).await.status,
        StatusCode::OK
    );
    // Login redirect and cookie path honour the prefix too.
    let app = app_with(&Fake::new(true), Some("t"), "/dman");
    let r = send(&app, get("/dman/")).await;
    assert_eq!(r.headers[header::LOCATION], "/dman/login");
    let mut req = login_req("t");
    *req.uri_mut() = "/dman/login".parse().unwrap();
    let r = send(&app, req).await;
    assert!(
        r.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Path=/dman;")
    );
}

#[tokio::test]
async fn downloads_page_lists_jobs_and_escapes_titles() {
    let app = app_with(&Fake::new(true), None, "/");
    let r = send(&app, get("/")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("ubuntu.iso"));
    assert!(r.body.contains("25.0%"));
    assert!(r.body.contains("id=\"job-1\"") && r.body.contains("id=\"job-2\""));
    assert!(r.body.contains(">Downloading<") && r.body.contains(">Paused<"));
    assert!(!r.body.contains(EVIL), "raw script tag leaked");
    assert!(r.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    // Filter and sort via the table fragment.
    let r = send(&app, get("/downloads/table?q=ubuntu&sort=name&dir=desc")).await;
    assert!(r.body.contains("ubuntu.iso") && !r.body.contains("alert(1)"));
    assert!(!r.body.contains("<html"));
}

#[tokio::test]
async fn detail_page_shows_trackers_files_and_escapes() {
    let app = app_with(&Fake::new(true), None, "/");
    let r = send(&app, get("/jobs/1")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("a/b.bin"));
    assert!(r.body.contains("http://t/announce") && r.body.contains("unknown"));
    assert!(r.body.contains("name=\"_csrf\""));
    assert_eq!(
        send(&app, get("/jobs/77")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn actions_call_the_backend() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let mut hx = post_form("/jobs/1/pause", &t, "q=ubuntu");
    hx.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    hx.headers_mut()
        .insert("hx-target", "jobs".parse().unwrap());
    let r = send(&app, hx).await;
    assert_eq!(r.status, StatusCode::OK);
    // The table comes back with the filter applied, plus a toast out of band.
    assert!(r.body.contains("id=\"jobs\"") && r.body.contains("id=\"job-1\""));
    assert!(!r.body.contains("id=\"job-2\""), "filter kept");
    assert!(r.body.contains("hx-swap-oob") && r.body.contains("Paused \u{201c}ubuntu.iso\u{201d}"));
    assert_eq!(fake.calls("jobs.pause"), vec![json!({"ids":[1]})]);

    send(&app, post_form("/jobs/2/resume", &t, "")).await;
    assert_eq!(fake.calls("jobs.resume"), vec![json!({"ids":[2]})]);
    send(&app, post_form("/jobs/2/purge", &t, "")).await;
    assert_eq!(fake.calls("jobs.purge"), vec![json!({"ids":[2]})]);
    send(&app, post_form("/jobs/2/reorder", &t, "how=up")).await;
    assert_eq!(
        fake.calls("jobs.reorder"),
        vec![json!({"id":2,"to":{"how":"by","value":-1}})]
    );
    send(&app, post_form("/jobs/2/reorder", &t, "how=bottom")).await;
    assert_eq!(
        fake.calls("jobs.reorder")[1],
        json!({"id":2,"to":{"how":"bottom"}})
    );
    send(&app, post_form("/jobs/2/rename", &t, "name=new.iso")).await;
    assert_eq!(
        fake.calls("jobs.rename"),
        vec![json!({"id":2,"name":"new.iso"})]
    );
    send(
        &app,
        post_form("/jobs/2/limits", &t, "download=500K&upload=0"),
    )
    .await;
    assert_eq!(
        fake.calls("jobs.set_options"),
        vec![json!({"id":2,"options":{"max_download_limit":512000,"max_upload_limit":0}})]
    );
    let bad = send(&app, post_form("/jobs/2/limits", &t, "download=lots")).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    send(
        &app,
        post_form(
            "/settings/limits",
            &t,
            "download=2M&upload=&max_concurrent=4",
        ),
    )
    .await;
    assert_eq!(
        fake.calls("limits.set"),
        vec![json!({"download":2097152,"upload":null,"max_concurrent":4})]
    );
    send(&app, post_form("/queue/resume-all", &t, "")).await;
    assert_eq!(fake.calls("queue.resume_all").len(), 1);
}

#[tokio::test]
async fn deleting_files_needs_the_explicit_flag_of_the_confirmation() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    // The plain remove confirmation never carries the delete flag ...
    let r = send(&app, get("/jobs/1/remove")).await;
    assert!(r.body.contains("from the list?") && r.body.contains("stay on disk"));
    assert!(!r.body.contains("name=\"delete_files\""));
    // ... the "Delete files" confirmation does, once, with no extra checkbox or word.
    let r = send(&app, get("/jobs/1/remove?delete=1")).await;
    assert!(r.body.contains("permanently delete the files of"));
    assert!(r.body.contains("name=\"delete_files\" value=\"true\""));
    assert!(
        !r.body.contains("name=\"confirm\"")
            && !r.body.contains("type=\"checkbox\" name=\"delete_files\"")
    );

    // Plain removal keeps the files, whatever else is sent.
    for form in [
        "",
        "confirm=DELETE",
        "delete_files=1",
        "delete_files=on&confirm=DELETE",
    ] {
        let r = send(&app, post_form("/jobs/1/remove", &t, form)).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{form}");
        assert_eq!(
            r.headers[header::LOCATION],
            "/?done=removed&undo=1",
            "{form}"
        );
    }
    let calls = fake.calls("jobs.remove");
    assert_eq!(calls.len(), 4);
    assert!(calls.iter().all(|c| c["delete_files"] == false));
    assert_eq!(
        calls[0],
        json!({"ids":[1],"delete_files":false,"force":false})
    );
    // Only the confirmation's explicit flag deletes.
    let r = send(&app, post_form("/jobs/1/remove", &t, "delete_files=true")).await;
    assert_eq!(r.headers[header::LOCATION], "/?done=removed-files");
    assert_eq!(
        fake.calls("jobs.remove")[4],
        json!({"ids":[1],"delete_files":true,"force":false})
    );
    // Still CSRF-protected.
    let r = send(
        &app,
        post_form("/jobs/1/remove", "forged", "delete_files=true"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(fake.calls("jobs.remove").len(), 5);
}

#[tokio::test]
async fn add_posts_one_source_per_line() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let form = "uris=magnet%3A%3Fxt%3Durn%3Abtih%3Aabc%0D%0Ahttps%3A%2F%2Fe.org%2Fa.iso&paused=1&dir=%2Fsrv";
    let r = send(&app, post_form("/add", &t, form)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/?added=99");
    assert_eq!(
        fake.calls("jobs.add"),
        vec![json!({
            "sources":[{"type":"uris","uris":["magnet:?xt=urn:btih:abc"]},{"type":"uris","uris":["https://e.org/a.iso"]}],
            "options":{"dir":"/srv"},
            "paused":true
        })]
    );
    // A video URL without a video engine is refused.
    let r = send(
        &app,
        post_form("/add", &t, "video_url=https%3A%2F%2Fv.example%2Fw"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(fake.calls("jobs.add").len(), 1);
}

#[tokio::test]
async fn video_add_uses_video_source() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    send(
        &app,
        post_form(
            "/add",
            &t,
            "video_url=https%3A%2F%2Fv.example%2Fw&quality=720p",
        ),
    )
    .await;
    assert_eq!(
        fake.calls("jobs.add"),
        vec![
            json!({"sources":[{"type":"video","url":"https://v.example/w","video":{"quality":"720p"}}],"options":{},"paused":false})
        ]
    );
}

#[tokio::test]
async fn torrent_upload_becomes_torrent_source() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let body = "--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\nd4:infoe\r\n--XX--\r\n";
    let req = Request::post("/add/upload")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header("x-csrf-token", &t)
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XX")
        .body(Body::from(body))
        .unwrap();
    let r = send(&app, req).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body.contains("Added 1"));
    let calls = fake.calls("jobs.add");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["sources"][0]["type"], "torrent");
    assert_eq!(calls[0]["sources"][0]["name"], "a.torrent");
    // base64("d4:infoe")
    assert_eq!(calls[0]["sources"][0]["data_base64"], "ZDQ6aW5mb2U=");

    // Wrong extension is rejected without calling the daemon.
    let body = body.replace("a.torrent", "evil.exe");
    let req = Request::post("/add/upload")
        .header(header::HOST, "localhost")
        .header("x-csrf-token", &t)
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XX")
        .body(Body::from(body))
        .unwrap();
    assert_eq!(send(&app, req).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(fake.calls("jobs.add").len(), 1);
}

#[tokio::test]
async fn oversized_upload_is_refused() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let mut body =
        b"--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.torrent\"\r\n\r\n"
            .to_vec();
    body.extend(std::iter::repeat_n(b'x', crate::MAX_UPLOAD_BYTES + 200_000));
    body.extend(b"\r\n--XX--\r\n");
    let req = Request::post("/add/upload")
        .header(header::HOST, "localhost")
        .header("x-csrf-token", &t)
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XX")
        .body(Body::from(body))
        .unwrap();
    assert_eq!(send(&app, req).await.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(fake.calls("jobs.add").is_empty());
}

#[tokio::test]
async fn video_options_follow_engine_status() {
    let app = app_with(&Fake::new(false), None, "/");
    let r = send(&app, get("/add")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("name=\"uris\""));
    assert!(!r.body.contains("as_video") && !r.body.contains("Quality"));
    let app = app_with(&Fake::new(true), None, "/");
    let r = send(&app, get("/add")).await;
    assert!(r.body.contains("name=\"as_video\"") && r.body.contains("Quality"));
}

#[tokio::test]
async fn search_start_and_add_from_results() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    // The fake answers search.start with null, which is not a SearchStarted: error page, not a crash.
    let r = send(
        &app,
        post_form("/search", &t, "q=linux&providers=a&providers=b"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        fake.calls("search.start"),
        vec![json!({"query":"linux","providers":["a","b"]})]
    );
    let r = send(
        &app,
        post_form("/search/add", &t, "magnet=magnet%3A%3Fxt%3Dabc"),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/?added=99");
    assert_eq!(
        fake.calls("jobs.add")[0]["sources"][0],
        json!({"type":"uris","uris":["magnet:?xt=abc"]})
    );
}

async fn next_frame_containing(
    stream: &mut (impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
    needle: &str,
) -> String {
    let mut seen = String::new();
    let fut = async {
        while let Some(chunk) = stream.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if seen.contains(needle) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), fut)
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for {needle:?}; got {seen:?}"));
    seen
}

#[tokio::test]
async fn sse_pushes_changed_rows_and_layout_changes() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let r = app.clone().oneshot(get("/events?rows=1")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut stream = r.into_body().into_data_stream();
    // Initial state: stats and the live row.
    let seen = next_frame_containing(&mut stream, "event: job-1").await;
    assert!(seen.contains("event: stats"));

    // Progress of a known job: only its row is sent.
    fake.tx
        .send(Event::JobsUpdated {
            jobs: vec![job(1, "ubuntu.iso", JobState::Active, 555)],
        })
        .unwrap();
    let seen = next_frame_containing(&mut stream, "55.5%").await;
    assert!(seen.contains("event: job-1") && seen.contains("id=\"live-1\""));
    assert!(seen.contains("<progress id=\"pb-1\""));
    assert!(!seen.contains("jobs-changed"));

    // A new job changes the layout: the table is told to reload.
    fake.tx
        .send(Event::JobsUpdated {
            jobs: vec![job(5, "new", JobState::Waiting, 0)],
        })
        .unwrap();
    next_frame_containing(&mut stream, "event: jobs-changed").await;
    // Messages end up in the toast area.
    fake.tx
        .send(Event::Message {
            level: dman_proto::method::Level::Warn,
            text: EVIL.into(),
        })
        .unwrap();
    let seen = next_frame_containing(&mut stream, "event: message").await;
    assert!(!seen.contains(EVIL));
}

#[tokio::test]
async fn sse_coalesces_bursts() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let r = app.clone().oneshot(get("/events?rows=1")).await.unwrap();
    let mut stream = r.into_body().into_data_stream();
    next_frame_containing(&mut stream, "event: job-1").await;
    for i in 1..=20u64 {
        fake.tx
            .send(Event::JobsUpdated {
                jobs: vec![job(1, "ubuntu.iso", JobState::Active, 250 + i)],
            })
            .unwrap();
    }
    let seen = next_frame_containing(&mut stream, "event: job-1").await;
    // The burst arrives as one row carrying the latest value.
    assert_eq!(seen.matches("event: job-1").count(), 1);
    assert!(seen.contains("27.0%"), "{seen}");
}

#[tokio::test]
async fn sse_streams_search_results() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let r = app.clone().oneshot(get("/events?search=7")).await.unwrap();
    let mut stream = r.into_body().into_data_stream();
    let result = dman_proto::search::SearchResultDto {
        title: EVIL.into(),
        magnet: Some("magnet:?xt=urn:btih:abc".into()),
        sources: vec!["tpb".into()],
        ..Default::default()
    };
    fake.tx
        .send(Event::SearchUpdate {
            search_id: 7,
            provider: "tpb".into(),
            status: dman_proto::search::ProviderStatus::Done {
                count: 1,
                elapsed_ms: 12,
            },
            results: vec![result],
        })
        .unwrap();
    let seen = next_frame_containing(&mut stream, "event: search").await;
    assert!(seen.contains("&lt;script&gt;") && !seen.contains(EVIL));
    assert!(seen.contains("hx-post") && seen.contains("1 results"));
}

#[tokio::test]
async fn csp_and_security_headers_present() {
    let app = app_with(&Fake::new(true), Some("t"), "/");
    for uri in ["/login", "/static/htmx.min.js", "/", "/events"] {
        let r = send(&app, get(uri)).await;
        let csp = r.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.starts_with("default-src 'self'"), "{uri}: {csp}");
        assert!(!csp.contains("unsafe-inline"));
        assert_eq!(
            r.headers[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "{uri}"
        );
    }
    // Pages never carry inline scripts or style attributes.
    let app = app_with(&Fake::new(true), None, "/");
    for uri in [
        "/",
        "/?removed=1",
        "/add",
        "/search",
        "/settings",
        "/jobs/1",
        "/jobs/1/remove",
        "/fs",
        "/?done=removed&undo=1",
    ] {
        let r = send(&app, get(uri)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        assert!(
            !r.body.contains(" style=") && !r.body.contains("onclick"),
            "{uri}"
        );
        assert!(!r.body.contains("<script>"), "{uri}");
    }
}

#[tokio::test]
async fn exposed_without_token_shows_banner() {
    let fake = Fake::new(true);
    let app = router(
        fake,
        WebConfig {
            exposed: true,
            ..WebConfig::default()
        },
    );
    assert!(
        send(&app, get("/"))
            .await
            .body
            .contains("reachable from the network")
    );
    let app = app_with(&Fake::new(true), None, "/");
    assert!(
        !send(&app, get("/"))
            .await
            .body
            .contains("reachable from the network")
    );
}

#[test]
fn asset_urls_carry_a_content_fingerprint() {
    let css = crate::assets::url("/dman", "app.css");
    assert!(css.starts_with("/dman/static/app.css?v="), "{css}");
    assert_eq!(css.len(), "/dman/static/app.css?v=".len() + 10, "{css}");
    // Unknown names fall back to the plain path.
    assert_eq!(crate::assets::url("", "nope.js"), "/static/nope.js");
}

#[test]
fn embedded_asset_sizes() {
    let list = crate::assets::listing();
    let mut total = 0;
    for (name, size) in &list {
        println!("asset {name}: {size} bytes");
        total += size;
    }
    println!("embedded assets total: {total} bytes");
    assert_eq!(total, crate::embedded_asset_bytes());
    assert!(list.iter().any(|(n, _)| n == "htmx.min.js"));
    assert!(total < 150_000, "assets grew to {total} bytes");
    // Our own stylesheet and script stay small (htmx and its SSE extension excluded).
    let own: usize = list
        .iter()
        .filter(|(n, _)| n == "app.css" || n == "app.js")
        .map(|(_, s)| s)
        .sum();
    println!("app.css + app.js: {own} bytes");
    // The 40 KiB goal of the redesign brief was not reachable with every requested
    // feature (command menu, sparkline, tabs, themes, link badges); 50 KiB is the cap.
    assert!(own <= 51_200, "app.css + app.js grew to {own} bytes");
}

#[tokio::test]
async fn add_accepts_a_download_limit() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    assert!(
        send(&app, get("/add"))
            .await
            .body
            .contains("name=\"download_limit\"")
    );
    let r = send(
        &app,
        post_form(
            "/add",
            &t,
            "uris=https%3A%2F%2Fe.org%2Fa&download_limit=500K",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fake.calls("jobs.add")[0]["options"],
        json!({"max_download_limit": 512_000})
    );
    let r = send(
        &app,
        post_form(
            "/add",
            &t,
            "uris=https%3A%2F%2Fe.org%2Fa&download_limit=fast",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // The form comes back with the value kept and the error next to the field.
    assert!(r.body.contains("is not a speed") && r.body.contains("value=\"fast\""));
    assert_eq!(fake.calls("jobs.add").len(), 1);
}

#[tokio::test]
async fn search_page_explains_when_no_provider_is_enabled() {
    let app = app_with(&Fake::new(false), None, "/");
    let r = send(&app, get("/search")).await;
    assert!(r.body.contains("Turn on a search provider first"));
}

fn hx(mut r: Request<Body>, target: &str) -> Request<Body> {
    r.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    if !target.is_empty() {
        r.headers_mut().insert("hx-target", target.parse().unwrap());
    }
    r
}

#[tokio::test]
async fn removed_jobs_are_hidden_with_a_toggle_and_can_be_cleared() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let r = send(&app, get("/")).await;
    assert!(
        !r.body.contains("gone.bin"),
        "removed job listed by default"
    );
    assert!(r.body.contains("Show removed (1)"));
    assert!(r.body.contains("Clear removed"));
    let r = send(&app, get("/downloads/table?removed=1")).await;
    assert!(r.body.contains("gone.bin") && r.body.contains("Hide removed"));
    let t = csrf(&app).await;
    let r = send(&app, hx(post_form("/jobs/clear-removed", &t, ""), "jobs")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(fake.calls("jobs.purge"), vec![json!({"ids":[3]})]);
    assert!(r.body.contains("Cleared 1 removed download from the list"));
}

#[tokio::test]
async fn remove_and_delete_are_distinct_and_say_what_happened() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let page = send(&app, get("/")).await.body;
    assert!(page.contains("Remove from list\u{2026}") && page.contains("Delete files\u{2026}"));
    let r = send(&app, hx(get("/jobs/1/remove"), "modal-body")).await;
    assert!(!r.body.contains("<html"), "modal gets a fragment");
    assert!(r.body.contains(">Remove from list<") && r.body.contains(">Cancel<"));
    let r = send(&app, hx(get("/jobs/1/remove?delete=1"), "modal-body")).await;
    assert!(r.body.contains("btn danger") && r.body.contains(">Delete files<"));

    let r = send(&app, hx(post_form("/jobs/1/remove", &t, ""), "")).await;
    assert!(
        r.body
            .contains("Removed \u{201c}ubuntu.iso\u{201d} (files kept)."),
        "{}",
        r.body
    );
    assert_eq!(r.headers["hx-trigger"], "dman-close, dman-clear-sel");
    let r = send(
        &app,
        hx(post_form("/jobs/1/remove", &t, "delete_files=true"), ""),
    )
    .await;
    assert!(
        r.body
            .contains("Removed \u{201c}ubuntu.iso\u{201d} and deleted its files.")
    );
    assert_eq!(fake.calls("jobs.remove")[1]["delete_files"], true);

    // From the detail page the browser goes back to the list with a notice.
    let r = send(&app, hx(post_form("/jobs/1/remove", &t, "back=detail"), "")).await;
    assert_eq!(r.headers["hx-redirect"], "/?done=removed&undo=1");
    let r = send(&app, get("/?done=removed")).await;
    assert!(r.body.contains("Removed (files kept on disk)."));
    let r = send(&app, get("/jobs/1")).await;
    assert!(r.body.contains("/jobs/1/remove?back=detail&amp;delete=1"));
}

#[tokio::test]
async fn bulk_actions_take_the_selected_ids() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let r = send(
        &app,
        hx(
            post_form("/jobs/bulk", &t, "action=pause&ids=1&ids=2"),
            "jobs",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(fake.calls("jobs.pause"), vec![json!({"ids":[1,2]})]);
    assert!(r.body.contains("Paused 1 download") || r.body.contains("Paused 2 downloads"));
    assert_eq!(r.headers["hx-trigger"], "dman-clear-sel");
    send(&app, post_form("/jobs/bulk", &t, "action=resume&ids=2")).await;
    assert_eq!(fake.calls("jobs.resume"), vec![json!({"ids":[2]})]);
    // Nothing ticked: a message, no call.
    let r = send(
        &app,
        hx(post_form("/jobs/bulk", &t, "action=pause"), "jobs"),
    )
    .await;
    assert!(r.body.contains("Tick one or more downloads first"));
    assert_eq!(fake.calls("jobs.pause").len(), 1);
    // Bulk remove: confirmation lists the (escaped) names, then removes all.
    let r = send(&app, hx(get("/jobs/bulk/remove?ids=1&ids=2"), "modal-body")).await;
    assert!(
        r.body.contains("Remove 2 downloads from the list?") && r.body.contains("&lt;script&gt;")
    );
    assert!(!r.body.contains(EVIL));
    let r = send(
        &app,
        hx(post_form("/jobs/bulk/remove", &t, "ids=1&ids=2"), ""),
    )
    .await;
    assert!(r.body.contains("Removed 2 downloads (files kept)."));
    assert_eq!(
        fake.calls("jobs.remove")[0],
        json!({"ids":[1,2],"delete_files":false,"force":false})
    );
}

#[tokio::test]
async fn paste_box_adds_and_returns_the_highlighted_table() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let page = send(&app, get("/")).await.body;
    assert!(page.contains("id=\"paste-in\"") && page.contains("Paste a magnet or link"));
    assert!(page.contains("Add download") && page.contains("aria-current=\"page\""));
    let r = send(
        &app,
        hx(
            post_form(
                "/add",
                &t,
                "from=downloads&uris=https%3A%2F%2Fe.org%2Fa.iso",
            ),
            "jobs",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("id=\"jobs\"") && r.body.contains("hx-swap-oob"));
    assert_eq!(r.headers["x-dman-added"], "99");
    assert!(
        r.headers["hx-trigger"]
            .to_str()
            .unwrap()
            .contains("dman-added")
    );
    // Not a link: an error toast, nothing sent.
    let r = send(&app, hx(post_form("/add", &t, "uris=ubuntu"), "jobs")).await;
    assert!(r.body.contains("is not a link"));
    assert_eq!(r.headers["hx-retarget"], "#toasts");
    assert_eq!(fake.calls("jobs.add").len(), 1);
    // The redirect target highlights the new job and toasts.
    let r = send(&app, get("/?added=1")).await;
    assert!(
        r.body.contains("job st-active fresh")
            && r.body.contains("Added \u{201c}ubuntu.iso\u{201d}.")
    );
}

#[tokio::test]
async fn seed_checkbox_sets_the_seed_option() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    assert!(send(&app, get("/add")).await.body.contains("name=\"seed\""));
    send(
        &app,
        post_form("/add", &t, "uris=magnet%3A%3Fxt%3Dabc&seed=1"),
    )
    .await;
    assert_eq!(fake.calls("jobs.add")[0]["options"], json!({"seed": true}));
}

#[tokio::test]
async fn magnet_file_selection_adds_the_torrent_with_selected_files() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    // More than one link: explained, no preview started.
    let r = send(
        &app,
        hx(
            post_form("/add/preview", &t, "uris=magnet%3A%3Fa+magnet%3A%3Fb"),
            "preview",
        ),
    )
    .await;
    assert!(r.body.contains("one magnet link at a time"));
    assert!(fake.calls("jobs.preview_magnet").is_empty());
    let r = send(
        &app,
        hx(
            post_form("/add/preview", &t, "uris=magnet%3A%3Fxt%3Dabc"),
            "preview",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        fake.calls("jobs.preview_magnet")[0]["uri"],
        "magnet:?xt=abc"
    );
    assert!(r.body.contains("Big &lt;b&gt;Pack&lt;/b&gt;") && !r.body.contains(EVIL));
    assert!(r.body.contains("name=\"select\" value=\"2\" checked"));
    let key = "name=\"preview_id\" value=\"";
    let i = r.body.find(key).unwrap() + key.len();
    let id = &r.body[i..i + r.body[i..].find('"').unwrap()];
    let form =
        format!("uris=magnet%3A%3Fxt%3Dabc%0Ahttps%3A%2F%2Fe.org%2Fb&preview_id={id}&select=2");
    let r = send(&app, post_form("/add", &t, &form)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let call = &fake.calls("jobs.add")[0];
    assert_eq!(call["sources"][0]["type"], "torrent");
    assert_eq!(call["sources"][0]["data_base64"], "ZDQ6aW5mb2U=");
    assert_eq!(
        call["sources"][1],
        json!({"type":"uris","uris":["https://e.org/b"]})
    );
    assert_eq!(
        call["sources"].as_array().unwrap().len(),
        2,
        "magnet not added twice"
    );
    assert_eq!(call["options"]["select_files"], json!([2]));
    // Used up: a second submit explains instead of guessing.
    let r = send(&app, post_form("/add", &t, &form)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("file list has expired"));
}

#[tokio::test]
async fn folder_picker_lists_daemon_folders() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let r = send(&app, hx(get("/fs?dir=%2Fsrv"), "fs-picker")).await;
    assert_eq!(
        fake.calls("fs.list")[0],
        json!({"path":"/srv","dirs_only":true})
    );
    assert!(r.body.contains("data-pick=\"/srv\"") && r.body.contains("movies/"));
    assert!(r.body.contains("hx-get=\"/fs?path=%2Fsrv%2Fmovies\""));
    assert!(!r.body.contains(EVIL) && !r.body.contains("file.txt"));
    // A folder that does not exist falls back to the default with a note.
    let r = send(&app, hx(get("/fs?dir=%2Fnope"), "fs-picker")).await;
    assert!(
        r.body.contains("Showing the default folder") && r.body.contains("data-pick=\"/data\"")
    );
}

#[tokio::test]
async fn multipart_form_carries_the_csrf_field() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let body = |tok: &str| {
        format!(
            "--XX\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{tok}\r\n--XX\r\nContent-Disposition: form-data; name=\"uris\"\r\n\r\nhttps://e.org/a\r\n--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"\"\r\nContent-Type: application/octet-stream\r\n\r\n\r\n--XX--\r\n"
        )
    };
    let req = |tok: &str| {
        Request::post("/add")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost")
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XX")
            .body(Body::from(body(tok)))
            .unwrap()
    };
    assert_eq!(
        send(&app, req("forged")).await.status,
        StatusCode::FORBIDDEN
    );
    assert!(fake.calls("jobs.add").is_empty());
    let r = send(&app, req(&t)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(
        fake.calls("jobs.add")[0]["sources"],
        json!([{"type":"uris","uris":["https://e.org/a"]}])
    );
}

#[tokio::test]
async fn search_download_toasts_with_the_escaped_title() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let form = format!(
        "magnet=magnet%3A%3Fxt%3Dabc&title={}",
        url::form_urlencoded::byte_serialize(EVIL.as_bytes()).collect::<String>()
    );
    let r = send(&app, hx(post_form("/search/add", &t, &form), "toasts")).await;
    assert!(r.body.contains("Started \u{201c}&lt;script&gt;"));
    assert!(!r.body.contains(EVIL) && r.body.contains("href=\"/jobs/99\""));
}

#[tokio::test]
async fn detail_is_live_over_sse() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let page = send(&app, get("/jobs/1")).await.body;
    assert!(page.contains("sse-connect=\"/events?job=1\""));
    assert!(page.contains("sse-swap=\"detail\"") && !page.contains("every 3s"));
    assert!(page.contains("Downloading \u{b7} 2 peers"));
    let r = app.clone().oneshot(get("/events?job=1")).await.unwrap();
    let mut stream = r.into_body().into_data_stream();
    let seen = next_frame_containing(&mut stream, "event: extra").await;
    assert!(seen.contains("event: detail") && seen.contains("id=\"dhead\""));
    fake.tx
        .send(Event::JobsUpdated {
            jobs: vec![job(1, "ubuntu.iso", JobState::Paused, 600)],
        })
        .unwrap();
    let seen = next_frame_containing(&mut stream, "60.0%").await;
    assert!(seen.contains("Paused \u{2014} press Resume"));
}

#[tokio::test]
async fn settings_limits_send_only_changes_and_explain_errors() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let r = send(&app, get("/settings")).await;
    for want in [
        "Speed limits",
        "Queue",
        "Schedule",
        "Search providers",
        "Web access",
        "Engines",
        "yt-dlp",
    ] {
        assert!(r.body.contains(want), "{want}");
    }
    // Unchanged values: nothing to set.
    send(
        &app,
        post_form(
            "/settings/limits",
            &t,
            "download=&o_download=&upload=&o_upload=",
        ),
    )
    .await;
    assert!(fake.calls("limits.set").is_empty());
    let r = send(
        &app,
        post_form(
            "/settings/limits",
            &t,
            "max_concurrent=5&o_max_concurrent=3",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fake.calls("limits.set"),
        vec![json!({"download":null,"upload":null,"max_concurrent":5})]
    );
    let r = send(
        &app,
        post_form("/settings/limits", &t, "download=fast&o_download="),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("is not a speed") && r.body.contains("value=\"fast\""));
    assert_eq!(fake.calls("limits.set").len(), 1);
}

// ----- video quality picker ----------------------------------------------------

fn choice(id: &str, stream: &str, label: &str, h: Option<u32>, kind: &str) -> Value {
    json!({
        "id": id, "stream_id": stream, "label": label, "height": h,
        "kind": kind, "available": true, "requires_ffmpeg": kind == "video_only"
    })
}

/// A probe with a video-only 1080p, a pre-merged 720p, an unavailable 4K and two audios.
fn picker_probe(title: &str) -> Value {
    let mut c4k = choice(
        "313+251",
        "313",
        "2160p \u{b7} VP9",
        Some(2160),
        "video_only",
    );
    c4k["available"] = json!(false);
    c4k["unavailable_reason"] = json!("needs ffmpeg <i>x</i>");
    let mut opus = choice("251", "251", "Opus 130k", None, "audio_only");
    opus["codec"] = json!("Opus");
    let mut aac = choice("140", "140", "AAC 128k", None, "audio_only");
    aac["codec"] = json!("AAC");
    let mut v1080 = choice(
        "137+251",
        "137",
        "1080p \u{b7} H.264",
        Some(1080),
        "video_only",
    );
    v1080["size_estimate"] = json!(50_000_000u64);
    json!({
        "url": "https://www.youtube.com/watch?v=abc",
        "title": title,
        "uploader": "Chan <b>nel</b>",
        "duration": 125.0,
        "ffmpeg": true,
        "needs_choice": true,
        "video_choices": [
            c4k,
            v1080,
            choice("22", "22", "720p \u{b7} H.264", Some(720), "muxed"),
            choice("18", "18", "360p \u{b7} H.264", Some(360), "muxed"),
        ],
        "audio_choices": [opus, aac],
        "suggested": {"video": "137+251", "audio": "251"}
    })
}

async fn picker_page(app: &Router, fake: &Fake, t: &str, form: &str) -> (String, String) {
    let _ = fake;
    let r = send(app, post_form("/add", t, form)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let to = r.headers[header::LOCATION].to_str().unwrap().to_string();
    assert!(to.starts_with("/add/quality/"), "{to}");
    let page = send(app, get(&to)).await;
    assert_eq!(page.status, StatusCode::OK);
    (to, page.body)
}

const VIDEO_FORM: &str =
    "from=add&as_video=1&quality=&uris=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3Dabc";

#[tokio::test]
async fn quality_picker_is_skipped_unless_asking_and_choosing() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let direct = json!({"sources":[{"type":"video","url":"https://www.youtube.com/watch?v=abc","video":{}}],"options":{},"paused":false});

    // Preferences unreadable: straight to the daemon defaults.
    let r = send(&app, post_form("/add", &t, VIDEO_FORM)).await;
    assert_eq!(r.headers[header::LOCATION], "/?added=99");
    assert_eq!(fake.calls("jobs.add"), vec![direct.clone()]);
    assert!(fake.calls("video.probe").is_empty());

    // ask_quality off: no probe either.
    fake.set_prefs(false);
    fake.set_probe(picker_probe("t"));
    send(&app, post_form("/add", &t, VIDEO_FORM)).await;
    assert_eq!(fake.calls("jobs.add").len(), 2);
    assert!(fake.calls("video.probe").is_empty());

    // Asking, but nothing to choose: probed, then added directly.
    fake.set_prefs(true);
    let mut single = picker_probe("t");
    single["needs_choice"] = json!(false);
    fake.set_probe(single);
    let r = send(&app, post_form("/add", &t, VIDEO_FORM)).await;
    assert_eq!(r.headers[header::LOCATION], "/?added=99");
    assert_eq!(fake.calls("video.probe").len(), 1);
    assert_eq!(fake.calls("jobs.add").len(), 3);
    // The probe request names the URL (and not a playlist).
    assert_eq!(
        fake.calls("video.probe")[0],
        json!({"url":"https://www.youtube.com/watch?v=abc","playlist":false})
    );

    // A playlist keeps the defaults.
    let mut pl = picker_probe("t");
    pl["playlist"] = json!(true);
    fake.set_probe(pl);
    send(&app, post_form("/add", &t, VIDEO_FORM)).await;
    assert_eq!(fake.calls("jobs.add").len(), 4);

    // An explicit quality, a playlist request or several links never ask.
    fake.set_probe(picker_probe("t"));
    for form in [
        "as_video=1&quality=720p&uris=https%3A%2F%2Fv.example%2Fa",
        "as_video=1&playlist=1&uris=https%3A%2F%2Fv.example%2Fa",
        "as_video=1&uris=https%3A%2F%2Fv.example%2Fa%0Ahttps%3A%2F%2Fv.example%2Fb",
        "uris=https%3A%2F%2Fe.org%2Ffile.iso",
    ] {
        let r = send(&app, post_form("/add", &t, form)).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{form}");
        assert!(
            r.headers[header::LOCATION]
                .to_str()
                .unwrap()
                .starts_with("/?added")
        );
    }
    assert_eq!(fake.calls("video.probe").len(), 2, "no probe for those");
    assert_eq!(fake.calls("jobs.add").len(), 8);

    // A probe that fails says so and adds nothing.
    fake.set_probe(json!({"error": "no such <b>video</b>"}));
    let r = send(&app, post_form("/add", &t, VIDEO_FORM)).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(r.body.contains("Could not check the available qualities"));
    assert!(r.body.contains("no such &lt;b&gt;video&lt;/b&gt;"));
    assert!(!r.body.contains("<b>video</b>"));
    assert_eq!(fake.calls("jobs.add").len(), 8);
}

#[tokio::test]
async fn quality_picker_page_escapes_and_marks_unavailable() {
    let fake = Fake::new(true);
    fake.set_prefs(true);
    fake.set_probe(picker_probe(EVIL));
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let (to, body) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    assert!(body.contains("Choose the quality"));
    // Untrusted text is escaped everywhere.
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !body.contains(EVIL));
    assert!(body.contains("Chan &lt;b&gt;nel") && !body.contains("<b>nel"));
    assert!(body.contains("needs ffmpeg &lt;i&gt;x&lt;/i&gt;") && !body.contains("<i>x</i>"));
    // Rows: video (4 + audio only), audio (2); size appended when the label has none.
    assert!(
        body.contains("1080p \u{b7} H.264 \u{b7} ~47.7 MiB"),
        "{body}"
    );
    assert!(body.contains("Audio only (no video)"));
    assert!(
        body.contains("needs ffmpeg</span>"),
        "badge on merged choices"
    );
    // The unavailable 4K is disabled with its reason; nothing else is.
    let off = body.find("2160p").unwrap();
    let li = &body[body[..off].rfind("<li").unwrap()..off];
    assert!(li.contains("qrow off") && li.contains("disabled"), "{li}");
    assert!(body.contains(">unavailable<"));
    assert_eq!(body.matches(" disabled").count(), 1, "only the 4K row");
    // Suggested (1080p video-only + Opus) is preselected.
    let checked: Vec<&str> = body
        .match_indices("checked")
        .map(|(i, _)| &body[i - 60..i])
        .collect();
    assert!(
        checked
            .iter()
            .any(|c| c.contains("name=\"video\" value=\"1\"")),
        "{checked:?}"
    );
    assert!(
        checked
            .iter()
            .any(|c| c.contains("name=\"audio\" value=\"0\""))
    );
    // Presets, both checkboxes, Download and Cancel; CSRF on the form.
    for p in ["Best", "1080p", "720p", "480p", "Audio only"] {
        assert!(body.contains(&format!(">{p}</button>")), "{p}");
    }
    assert!(body.contains("name=\"use_default\"") && body.contains("name=\"never_ask\""));
    assert!(body.contains("Use as my default") && body.contains("Never ask again"));
    assert!(body.contains(">Download</button>") && body.contains(">Cancel</a>"));
    assert!(body.contains("name=\"_csrf\""));
    assert!(body.contains(&format!("action=\"{to}\"")));
    // No inline script or style.
    assert!(!body.contains(" style=") && !body.contains("onclick") && !body.contains("<script>"));
    // Unknown or expired ids are a clear error.
    let r = send(&app, get("/add/quality/nope")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.body.contains("expired"));
    let r = send(&app, post_form("/add/quality/nope", &t, "video=0")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Forged posts are refused.
    let r = send(&app, post_form(&to, "forged", "video=1")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(fake.calls("jobs.add").is_empty());
}

#[tokio::test]
async fn quality_submit_computes_the_format_id_on_the_server() {
    let fake = Fake::new(true);
    fake.set_prefs(true);
    fake.set_probe(picker_probe("Clip"));
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let added = |n: usize| fake.calls("jobs.add")[n].clone();
    let fmt_of = |n: usize| added(n)["sources"][0]["video"]["format_id"].clone();

    // 1080p video-only + the other audio: stream id + chosen audio.
    let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    let r = send(&app, post_form(&to, &t, "act=download&video=1&audio=1")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/?added=99");
    assert_eq!(fmt_of(0), "137+140");
    assert!(fake.calls("video.preferences.set").is_empty());
    // The stored choice is used up.
    assert_eq!(send(&app, get(&to)).await.status, StatusCode::NOT_FOUND);

    // The audio the choice came with: its own id.
    let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    send(&app, post_form(&to, &t, "video=1&audio=0")).await;
    assert_eq!(fmt_of(1), "137+251");
    // A pre-merged choice ignores the audio list.
    let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    send(&app, post_form(&to, &t, "video=2&audio=1")).await;
    assert_eq!(fmt_of(2), "22");
    // Audio only: the audio format itself.
    let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    send(&app, post_form(&to, &t, "video=4&audio=1")).await;
    assert_eq!(fmt_of(3), "140");
    // Nothing is sent that the client invented; the rest of the add is kept.
    let (to, _) = picker_page(
        &app,
        &fake,
        &t,
        &format!("{VIDEO_FORM}&paused=1&dir=%2Fsrv"),
    )
    .await;
    send(&app, post_form(&to, &t, "video=2&format_id=evil&audio=0")).await;
    let last = added(4);
    assert_eq!(last["sources"][0]["video"], json!({"format_id": "22"}));
    assert_eq!(last["options"], json!({"dir": "/srv"}));
    assert_eq!(last["paused"], true);

    // The unavailable 4K, a row that does not exist: refused, nothing added, form kept.
    let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    let r = send(&app, post_form(&to, &t, "video=0&audio=0")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.body.contains("is not available: needs ffmpeg"),
        "{}",
        r.body
    );
    let r = send(&app, post_form(&to, &t, "video=9&audio=0")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(fake.calls("jobs.add").len(), 5);
}

#[tokio::test]
async fn quality_checkboxes_save_the_preferences() {
    let fake = Fake::new(true);
    fake.set_prefs(true);
    fake.set_probe(picker_probe("Clip"));
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let sets = || fake.calls("video.preferences.set");
    let go = |form: &'static str| {
        let (app, t) = (app.clone(), t.clone());
        let fake = fake.clone();
        async move {
            let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
            let r = send(&app, post_form(&to, &t, form)).await;
            assert_eq!(r.status, StatusCode::SEE_OTHER, "{form}");
        }
    };
    // Use as default: the preset of the height and the family of the audio codec.
    go("video=1&audio=0&use_default=1").await;
    assert_eq!(
        sets(),
        vec![json!({"default_quality":"1080p","default_audio":"opus"})]
    );
    // A pre-merged choice has no audio of its own: only the quality.
    go("video=2&audio=0&use_default=1").await;
    assert_eq!(sets()[1], json!({"default_quality":"720p"}));
    // No preset for 360p: best. Never ask: just that flag.
    go("video=3&audio=0&use_default=1&never_ask=1").await;
    assert_eq!(
        sets()[2],
        json!({"ask_quality":false,"default_quality":"best"})
    );
    go("video=1&audio=1&never_ask=1").await;
    assert_eq!(sets()[3], json!({"ask_quality":false}));
    // Audio only: audio:<family>.
    go("video=4&audio=1&use_default=1").await;
    assert_eq!(
        sets()[4],
        json!({"default_quality":"audio:m4a","default_audio":"m4a"})
    );
    // Nothing ticked: no call. Every add still happened.
    go("video=1&audio=1").await;
    assert_eq!(sets().len(), 5);
    assert_eq!(fake.calls("jobs.add").len(), 6);
}

#[tokio::test]
async fn quality_presets_select_rows_without_downloading() {
    let fake = Fake::new(true);
    fake.set_prefs(true);
    fake.set_probe(picker_probe("Clip"));
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let (to, _) = picker_page(&app, &fake, &t, VIDEO_FORM).await;
    let value_of = |body: &str, name: &str| {
        let i = body.match_indices("checked").find_map(|(i, _)| {
            let head = &body[i.saturating_sub(70)..i];
            head.contains(&format!("name=\"{name}\""))
                .then(|| head.to_string())
        });
        i.unwrap_or_default()
    };
    for (preset, video) in [
        ("720p", "2"),
        ("480p", "3"),
        ("best", "1"),
        ("1080p", "1"),
        ("audio", "4"),
    ] {
        let r = send(
            &app,
            post_form(&to, &t, &format!("video=1&audio=1&preset={preset}")),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{preset}");
        assert!(
            value_of(&r.body, "video").contains(&format!("value=\"{video}\"")),
            "{preset}: {}",
            value_of(&r.body, "video")
        );
    }
    // 720p: the pre-merged row; the audio selection follows the suggested track.
    let r = send(&app, post_form(&to, &t, "video=1&audio=1&preset=720p")).await;
    assert!(value_of(&r.body, "audio").contains("value=\"0\""));
    assert!(fake.calls("jobs.add").is_empty(), "presets never download");
    // The choice is still there afterwards.
    assert_eq!(send(&app, get(&to)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn paste_box_detects_video_sites_and_hands_over_to_the_picker() {
    let fake = Fake::new(true);
    fake.set_prefs(true);
    fake.set_probe(picker_probe("Clip"));
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let paste = "from=downloads&uris=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3Dabc";
    let r = send(&app, hx(post_form("/add", &t, paste), "jobs")).await;
    assert_eq!(r.status, StatusCode::OK);
    let to = r.headers["hx-redirect"].to_str().unwrap().to_string();
    assert!(to.starts_with("/add/quality/"), "{to}");
    // Cancel goes back to the downloads list.
    let page = send(&app, get(&to)).await;
    assert!(page.body.contains("href=\"/\">Cancel</a>"));
    // Without JavaScript the same post redirects.
    let r = send(&app, post_form("/add", &t, paste)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    // A plain download link stays an aria2 download; no probe.
    let before = fake.calls("video.probe").len();
    send(
        &app,
        hx(
            post_form(
                "/add",
                &t,
                "from=downloads&uris=https%3A%2F%2Fe.org%2Fa.iso",
            ),
            "jobs",
        ),
    )
    .await;
    assert_eq!(fake.calls("video.probe").len(), before);
    assert_eq!(
        fake.calls("jobs.add").last().unwrap()["sources"][0],
        json!({"type":"uris","uris":["https://e.org/a.iso"]})
    );
    // The spinner text exists in both forms.
    let page = send(&app, get("/")).await.body;
    assert!(
        page.contains("Checking available qualities")
            && page.contains("hx-indicator=\"#paste-ind\"")
    );
    let page = send(&app, get("/add")).await.body;
    assert!(
        page.contains("Checking available qualities") && page.contains("Ask me, or use my default")
    );
}

#[tokio::test]
async fn settings_video_section_reads_and_sets_preferences() {
    let fake = Fake::new(true);
    *fake.prefs.lock().unwrap() = json!({
        "ask_quality": true, "default_quality": "1080p", "default_audio": "opus",
        "default_container": "mkv"
    });
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let r = send(&app, get("/settings")).await;
    assert!(r.body.contains("id=\"video\"") && r.body.contains("Ask which quality to download"));
    assert!(r.body.contains("name=\"ask_quality\" value=\"1\" checked"));
    for (name, sel) in [
        ("default_quality", "1080p"),
        ("default_audio", "opus"),
        ("default_container", "mkv"),
    ] {
        let i = r.body.find(&format!("name=\"{name}\"")).unwrap();
        let sel_at = r.body[i..].find(" selected").unwrap();
        assert!(
            r.body[i..i + sel_at].ends_with(&format!("value=\"{sel}\"")),
            "{name}"
        );
    }
    assert!(!r.body.contains(" style=") && !r.body.contains("<script>"));
    let r = send(
        &app,
        post_form(
            "/settings/video",
            &t,
            "default_quality=720p&default_audio=m4a&default_container=",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/settings?done=video");
    assert_eq!(
        fake.calls("video.preferences.set"),
        vec![json!({
            "ask_quality": false, "default_quality": "720p", "default_audio": "m4a",
            "default_container": ""
        })]
    );
    let r = send(
        &app,
        post_form(
            "/settings/video",
            &t,
            "ask_quality=1&default_quality=best&default_audio=best&default_container=mp4",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(fake.calls("video.preferences.set")[1]["ask_quality"], true);
    assert_eq!(
        fake.calls("video.preferences.set")[1]["default_container"],
        "mp4"
    );
    assert!(
        send(&app, get("/settings?done=video"))
            .await
            .body
            .contains("Video settings saved.")
    );
    assert_eq!(
        send(
            &app,
            post_form("/settings/video", "forged", "ask_quality=1")
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // No video engine: no section.
    let app = app_with(&Fake::new(false), None, "/");
    assert!(
        !send(&app, get("/settings"))
            .await
            .body
            .contains("id=\"video\"")
    );
}

#[tokio::test]
async fn remove_from_list_offers_undo_but_deleting_files_does_not() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    // htmx: the toast has an Undo form with CSRF.
    let r = send(&app, hx(post_form("/jobs/2/remove", &t, ""), "")).await;
    assert!(r.body.contains(">Undo</button>"), "{}", r.body);
    assert!(
        r.body.contains("action=\"/jobs/restore\"") && r.body.contains("name=\"ids\" value=\"2\"")
    );
    assert!(r.body.contains(&format!("name=\"_csrf\" value=\"{t}\"")));
    assert!(
        r.body.contains("&lt;script&gt;") && !r.body.contains(EVIL),
        "names are escaped"
    );
    // Deleting files: no way back, no Undo.
    let r = send(
        &app,
        hx(post_form("/jobs/2/remove", &t, "delete_files=true"), ""),
    )
    .await;
    assert!(!r.body.contains("Undo") && !r.body.contains("jobs/restore"));
    let r = send(&app, post_form("/jobs/2/remove", &t, "delete_files=true")).await;
    assert_eq!(r.headers[header::LOCATION], "/?done=removed-files");
    // Bulk removal undoes all ids at once.
    let r = send(
        &app,
        hx(post_form("/jobs/bulk/remove", &t, "ids=1&ids=2"), ""),
    )
    .await;
    assert!(r.body.contains("name=\"ids\" value=\"1,2\""), "{}", r.body);
    // After a redirect the downloads page shows the Undo toast.
    let r = send(&app, get("/?done=removed&undo=1,2")).await;
    assert!(r.body.contains(">Undo</button>") && r.body.contains("name=\"ids\" value=\"1,2\""));
    // Garbage in `undo` is dropped.
    let r = send(&app, get("/?done=removed&undo=x%2C%22%3E%3Cb%3E")).await;
    assert!(!r.body.contains("Undo</button>") && !r.body.contains("x,\"><b>"));
}

#[tokio::test]
async fn restore_endpoint_calls_the_daemon_and_is_csrf_protected() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let r = send(&app, hx(post_form("/jobs/restore", &t, "ids=1%2C2"), "")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(fake.calls("jobs.restore"), vec![json!({"ids":[1,2]})]);
    assert!(r.body.contains("Restored 2 downloads."));
    assert_eq!(r.headers["hx-trigger"], "dman-restored");
    // Without JavaScript: a redirect with a notice.
    let r = send(&app, post_form("/jobs/restore", &t, "ids=1")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/?done=restored");
    assert!(
        send(&app, get("/?done=restored"))
            .await
            .body
            .contains("Restored.")
    );
    // Nothing to restore.
    let r = send(&app, post_form("/jobs/restore", &t, "")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(fake.calls("jobs.restore").len(), 2);
    // CSRF.
    let r = send(&app, post_form("/jobs/restore", "forged", "ids=1")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(fake.calls("jobs.restore").len(), 2);
}

// ------------------------------------------------------------- access model

fn with_host(mut r: Request<Body>, host: &str) -> Request<Body> {
    r.headers_mut().insert(header::HOST, host.parse().unwrap());
    r
}

#[tokio::test]
async fn loopback_opens_without_login_and_keeps_the_invisible_checks() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    // No cookie, no token: straight to the downloads; /login leads there too.
    let r = send(&app, get("/")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("<h1>Downloads</h1>") && !r.body.contains("Log out"));
    let r = send(&app, get("/login")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/");
    // Same origin + the CSRF token embedded in the page (what htmx sends) works.
    let t = csrf(&app).await;
    let r = send(&app, hx(post_form("/queue/pause-all", &t, ""), "")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(fake.calls("queue.pause_all").len(), 1);
    // Another site is refused, even with a valid token.
    let mut evil = post_form("/queue/pause-all", &t, "");
    evil.headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    assert_eq!(send(&app, evil).await.status, StatusCode::FORBIDDEN);
    assert_eq!(fake.calls("queue.pause_all").len(), 1);
}

#[tokio::test]
async fn host_header_is_checked_without_login() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    for host in [
        "localhost",
        "localhost:6811",
        "127.0.0.1:6811",
        "127.1.2.3",
        "[::1]:6811",
        "dman.localhost",
    ] {
        assert_eq!(
            send(&app, with_host(get("/"), host)).await.status,
            StatusCode::OK,
            "{host}"
        );
    }
    // DNS rebinding: another name pointing at this machine.
    for host in [
        "evil.example",
        "evil.example:6811",
        "192.168.1.5",
        "localhost.evil.example",
    ] {
        let r = send(&app, with_host(get("/"), host)).await;
        assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST, "{host}");
    }
    let t = csrf(&app).await;
    let r = send(
        &app,
        with_host(post_form("/queue/pause-all", &t, ""), "evil.example"),
    )
    .await;
    assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST);
    assert!(fake.calls("queue.pause_all").is_empty());
    let no_host = Request::get("/").body(Body::empty()).unwrap();
    assert_eq!(
        send(&app, no_host).await.status,
        StatusCode::MISDIRECTED_REQUEST
    );
    // A reverse proxy in front of a login-free loopback UI is refused with a hint.
    let mut proxied = get("/");
    proxied
        .headers_mut()
        .insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
    let r = send(&app, proxied).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.body.contains("require_login"));

    // Configured bind address and extra host names.
    let app = router(
        Fake::new(true),
        WebConfig {
            exposed: true,
            bind: Some("192.168.1.5".parse().unwrap()),
            allowed_hosts: vec!["NAS.local".into()],
            ..WebConfig::default()
        },
    );
    for host in ["192.168.1.5:6811", "nas.local:6811", "localhost"] {
        assert_eq!(
            send(&app, with_host(get("/"), host)).await.status,
            StatusCode::OK,
            "{host}"
        );
    }
    assert_eq!(
        send(&app, with_host(get("/"), "192.168.1.6")).await.status,
        StatusCode::MISDIRECTED_REQUEST
    );
    // Unspecified bind: any IP address, still no foreign names.
    let app = router(
        Fake::new(true),
        WebConfig {
            exposed: true,
            bind: Some("0.0.0.0".parse().unwrap()),
            ..WebConfig::default()
        },
    );
    assert_eq!(
        send(&app, with_host(get("/"), "10.0.0.7:6811"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, with_host(get("/"), "evil.example")).await.status,
        StatusCode::MISDIRECTED_REQUEST
    );

    // With a token (network bind) the login protects the UI; any host name works.
    let app = app_with(&Fake::new(true), Some("s3cret"), "/");
    let r = send(&app, with_host(get("/"), "dman.example.org")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/login");
}

// ------------------------------------------------------------ follow-up features

#[tokio::test]
async fn detail_offers_the_cleanup_of_unselected_files_with_one_confirmation() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let r = send(&app, get("/jobs/4")).await;
    assert_eq!(r.status, StatusCode::OK);
    // Listed (escaped) from a dry run; nothing deleted yet.
    assert!(r.body.contains("Leftover files") && r.body.contains("&lt;script&gt;"));
    assert!(!r.body.contains(EVIL));
    assert!(
        r.body.contains("action=\"/jobs/4/cleanup\"") && r.body.contains("Delete 1 leftover file")
    );
    assert_eq!(
        fake.calls("jobs.cleanup_unselected"),
        vec![json!({"id": 4, "dry_run": true})]
    );
    // A running torrent or one without deselected files shows no card.
    let r = send(&app, get("/jobs/1")).await;
    assert!(!r.body.contains("Leftover files"));
    // The button deletes; it needs the CSRF token like every state change.
    let t = csrf_of(&r.body);
    let r = send(&app, post_form("/jobs/4/cleanup", "forged", "")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send(&app, post_form("/jobs/4/cleanup", &t, "")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/jobs/4?done=cleaned");
    assert_eq!(
        fake.calls("jobs.cleanup_unselected").last(),
        Some(&json!({"id": 4, "dry_run": false}))
    );
    assert!(
        send(&app, get("/jobs/4?done=cleaned"))
            .await
            .body
            .contains("Deleted the leftover files")
    );
}

#[tokio::test]
async fn settings_reload_shows_applied_and_restart_required() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let page = send(&app, get("/settings")).await;
    assert!(page.body.contains("action=\"/settings/reload\""));
    let t = csrf_of(&page.body);
    let r = send(
        &app,
        hx(post_form("/settings/reload", &t, ""), "reload-result"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.body.contains("Applied now: limits, schedule"),
        "{}",
        r.body
    );
    assert!(r.body.contains("web.port") && r.body.contains("restart"));
    // Without JavaScript the whole page comes back with the result.
    let r = send(&app, post_form("/settings/reload", &t, "")).await;
    assert!(r.body.contains("<h1>Settings</h1>") && r.body.contains("Applied now"));
    assert_eq!(fake.calls("config.reload").len(), 2);
    let r = send(&app, post_form("/settings/reload", "forged", "")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(fake.calls("config.reload").len(), 2);
}

#[tokio::test]
async fn refused_removal_is_explained_inside_the_dialog() {
    let fake = Fake::new(false);
    *fake.refuse_remove.lock().unwrap() = true;
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let r = send(
        &app,
        hx(post_form("/jobs/2/remove", &t, "delete_files=true"), "x"),
    )
    .await;
    assert!(
        r.body.contains("class=\"confirm\"") && r.body.contains("role=\"alert\""),
        "{}",
        r.body
    );
    assert!(r.body.contains("folder is read-only"));
    assert!(
        !r.headers.contains_key("hx-retarget"),
        "stays in the dialog"
    );
}

#[tokio::test]
async fn add_refuses_links_of_unknown_schemes() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    for bad in [
        "file:///etc/passwd",
        "javascript://x%0Aalert(1)",
        "gopher://h/x",
    ] {
        let form = format!("uris={bad}");
        let r = send(&app, post_form("/add", &t, &form)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(fake.calls("jobs.add").is_empty());
}

#[tokio::test]
async fn resolve_is_not_reachable_from_another_site() {
    let app = app_with(&Fake::new(false), None, "/");
    let mut r = get("/search/resolve?detail_url=http%3A%2F%2Fx%2F");
    r.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    assert_eq!(send(&app, r).await.status, StatusCode::FORBIDDEN);
    let mut r = get("/search/resolve?detail_url=http%3A%2F%2Fx%2F");
    r.headers_mut()
        .insert(header::REFERER, "http://evil.example/".parse().unwrap());
    assert_eq!(send(&app, r).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn live_streams_are_capped() {
    let app = app_with(&Fake::new(false), None, "/");
    let mut open = Vec::new();
    for _ in 0..64 {
        let r = app.clone().oneshot(get("/events")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        open.push(r);
    }
    let r = send(&app, get("/events")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    // Closing one frees a slot.
    open.pop();
    let r = app.clone().oneshot(get("/events")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn multipart_outside_the_add_routes_is_not_buffered_up_to_the_upload_limit() {
    let app = app_with(&Fake::new(false), None, "/");
    let t = csrf(&app).await;
    let big = "x".repeat(2 * 1024 * 1024);
    let body = format!(
        "--b\r\nContent-Disposition: form-data; name=\"junk\"\r\n\r\n{big}\r\n--b\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{t}\r\n--b--\r\n"
    );
    let req = Request::post("/queue/pause-all")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=b")
        .body(Body::from(body))
        .unwrap();
    assert_eq!(send(&app, req).await.status, StatusCode::PAYLOAD_TOO_LARGE);
}
