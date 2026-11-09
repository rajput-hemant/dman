use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dman_proto::method::{BatchResult, Event};
use dman_proto::search::ProviderInfo;
use dman_proto::{
    AddOutcome, Engine, ErrorCode, FileInfo, GlobalStats, Job, JobDetail, JobId, JobState, Limits,
    RpcError, SourceKind, TrackerInfo, TrackerStatus,
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
                    job: jobs[0].clone(),
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
                "engine.status" => ok(json!([{"engine":"yt_dlp","available":true}])),
                "video.probe" if video => Err(RpcError::invalid("invalid url")),
                "video.probe" => Err(RpcError::new(
                    ErrorCode::General,
                    "method `video.probe` not found",
                )),
                "jobs.add" => ok(vec![AddOutcome::Added {
                    id: JobId(99),
                    extra_ids: vec![],
                }]),
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
        StatusCode::OK
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
    assert!(r.body.contains("Active (1)") && r.body.contains("Paused (1)"));
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
    let mut hx = post_form("/jobs/1/pause", &t, "");
    hx.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    let r = send(&app, hx).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("<tr id=\"job-1\""));
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
async fn remove_requires_checkbox_and_typed_delete_for_files() {
    let fake = Fake::new(true);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let r = send(&app, get("/jobs/1/remove")).await;
    assert!(r.body.contains("name=\"delete_files\"") && r.body.contains("name=\"confirm\""));

    // Plain removal keeps the files.
    let r = send(&app, post_form("/jobs/1/remove", &t, "")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        fake.calls("jobs.remove"),
        vec![json!({"ids":[1],"delete_files":false,"force":false})]
    );
    // Checkbox alone, wrong word, or the word alone: nothing happens or files stay.
    for form in [
        "delete_files=1",
        "delete_files=1&confirm=delete",
        "delete_files=1&confirm=",
        "delete_files=on&confirm=DELETE%20ALL",
    ] {
        let r = send(&app, post_form("/jobs/1/remove", &t, form)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{form}");
    }
    assert_eq!(
        fake.calls("jobs.remove").len(),
        1,
        "refused requests must not reach the daemon"
    );
    let r = send(&app, post_form("/jobs/1/remove", &t, "confirm=DELETE")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(fake.calls("jobs.remove")[1]["delete_files"], false);
    // Both together delete.
    let r = send(
        &app,
        post_form("/jobs/1/remove", &t, "delete_files=1&confirm=DELETE"),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let calls = fake.calls("jobs.remove");
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[2],
        json!({"ids":[1],"delete_files":true,"force":false})
    );
}

#[tokio::test]
async fn add_posts_one_source_per_line() {
    let fake = Fake::new(false);
    let app = app_with(&fake, None, "/");
    let t = csrf(&app).await;
    let form = "uris=magnet%3A%3Fxt%3Durn%3Abtih%3Aabc%0D%0Ahttps%3A%2F%2Fe.org%2Fa.iso&paused=1&dir=%2Fsrv";
    let r = send(&app, post_form("/add", &t, form)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Added 1 download"));
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
async fn video_options_hide_when_method_missing() {
    let app = app_with(&Fake::new(false), None, "/");
    let r = send(&app, get("/add")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("name=\"uris\""));
    assert!(!r.body.contains("video_url") && !r.body.contains("Quality"));
    let app = app_with(&Fake::new(true), None, "/");
    let r = send(&app, get("/add")).await;
    assert!(r.body.contains("name=\"video_url\"") && r.body.contains("Quality"));
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
    assert_eq!(r.status, StatusCode::OK);
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
    assert!(seen.contains("event: job-1") && seen.contains("<tr id=\"job-1\""));
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
    for uri in ["/", "/add", "/search", "/settings", "/jobs/1"] {
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
}
