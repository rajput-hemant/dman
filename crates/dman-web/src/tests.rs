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
        })
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
                "jobs.detail" if params["id"] != 1 => Err(RpcError::not_found("job")),
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
                "jobs.pause" | "jobs.resume" | "jobs.remove" | "jobs.purge" => ok(BatchResult {
                    ok: vec![JobId(1)],
                    failed: vec![],
                }),
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
            "href=\"/dman/static/app.css\"",
            "src=\"/dman/static/htmx.min.js\"",
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
        assert_eq!(r.headers[header::LOCATION], "/?done=removed", "{form}");
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
    assert!(own < 25_000, "app.css + app.js grew to {own} bytes");
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
    assert_eq!(r.headers["hx-redirect"], "/?done=removed");
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
    assert!(
        page.contains("id=\"paste-in\"")
            && page.contains("Paste a magnet, link or drop a .torrent file")
    );
    assert!(page.contains("+ Add download") && page.contains("aria-current=\"page\""));
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
