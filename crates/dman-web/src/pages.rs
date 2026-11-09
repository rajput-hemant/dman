//! Route handlers. Every state-changing handler returns a small fragment for htmx
//! callers (`HX-Request`) and redirects or renders a page for plain form posts.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{FromRequest, Multipart, Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use dman_proto::method::{
    self, EngineStatus, IdParams, IdsParams, LimitsUpdate, ListParams, RemoveParams, RenameParams,
    Reorder, ReorderParams, ScheduleStatus, SetOptionsParams,
};
use dman_proto::search::{
    self, ProviderInfo, ProviderNameParams, ProviderSetParams, ProviderTestReport, ResolvedMagnet,
    SearchHistoryEntry, SearchResolveParams, SearchSnapshot, SearchStartParams, SearchStarted,
};
use dman_proto::{
    AddOutcome, AddParams, AddSource, Engine, ErrorCode, GlobalStats, Job, JobDetail, JobId,
    JobOptions, Limits, RpcError, VideoOptions,
};
use maud::Markup;

use crate::auth::Ctx;
use crate::views::{self, Layout, Nav, SearchView, TableQuery};
use crate::{AppState, MAX_UPLOAD_BYTES, fmt};

type St = State<Arc<AppState>>;

// ------------------------------------------------------------------ helpers

/// Form body as ordered key/value pairs (repeated keys allowed).
pub struct Pairs(Vec<(String, String)>);

impl Pairs {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    pub fn all<'a>(&'a self, k: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    /// Non-empty, trimmed value.
    pub fn text(&self, k: &str) -> Option<&str> {
        self.get(k).map(str::trim).filter(|v| !v.is_empty())
    }

    pub fn flag(&self, k: &str) -> bool {
        self.get(k)
            .is_some_and(|v| !matches!(v, "" | "0" | "false" | "off"))
    }
}

impl<S: Send + Sync> FromRequest<S> for Pairs {
    type Rejection = Response;

    async fn from_request(req: Request, _: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
            .await
            .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response())?;
        Ok(Pairs(
            url::form_urlencoded::parse(&bytes)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        ))
    }
}

pub fn html(m: Markup) -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        m.into_string(),
    )
        .into_response()
}

fn query_pairs(raw: &Option<String>) -> Vec<(String, String)> {
    raw.as_deref()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn table_query(raw: &Option<String>) -> TableQuery {
    let mut t = TableQuery::default();
    for (k, v) in query_pairs(raw) {
        match k.as_str() {
            "q" => t.q = v,
            "sort" => t.sort = v,
            "dir" => t.desc = v == "desc",
            "all" => t.all = v == "1",
            _ => {}
        }
    }
    t
}

fn is_htmx(h: &HeaderMap) -> bool {
    h.contains_key("hx-request")
}

fn hx_target(h: &HeaderMap) -> &str {
    h.get("hx-target")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

fn redirect(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// A status message for htmx: always swapped into `#toast`, whatever the caller targeted.
fn toast(level: &str, text: &str) -> Response {
    let mut r = html(views::toast(level, text));
    let h = r.headers_mut();
    h.insert("hx-retarget", HeaderValue::from_static("#toast"));
    h.insert("hx-reswap", HeaderValue::from_static("innerHTML"));
    r
}

async fn stats(st: &AppState) -> Option<GlobalStats> {
    st.call(method::STATS_GET, ()).await.ok()
}

fn layout<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    nav: Nav,
    stats: Option<&'a GlobalStats>,
    sse: Option<String>,
) -> Layout<'a> {
    Layout {
        base: &st.base,
        title: &st.title,
        csrf: &ctx.csrf,
        nav,
        exposed_warning: st.exposed && st.token.is_none(),
        show_logout: ctx.auth_enabled,
        stats,
        sse,
    }
}

async fn full_page(
    st: &AppState,
    ctx: &Ctx,
    nav: Nav,
    heading: &str,
    sse: Option<String>,
    content: Markup,
) -> Response {
    let s = stats(st).await;
    html(views::page(
        &layout(st, ctx, nav, s.as_ref(), sse),
        heading,
        content,
    ))
}

/// Error outcome of a state-changing request.
async fn fail(
    st: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    status: StatusCode,
    msg: &str,
) -> Response {
    if is_htmx(headers) {
        return toast("error", msg);
    }
    let mut r = full_page(
        st,
        ctx,
        Nav::None,
        "Error",
        None,
        views::error_page(&st.base, msg),
    )
    .await;
    *r.status_mut() = status;
    r
}

fn status_for(e: &RpcError) -> StatusCode {
    match e.code {
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::Refused => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
    }
}

async fn fail_rpc(st: &AppState, ctx: &Ctx, headers: &HeaderMap, e: &RpcError) -> Response {
    fail(st, ctx, headers, status_for(e), &e.message).await
}

fn home(st: &AppState) -> String {
    format!("{}/", st.base)
}

pub async fn not_found(State(st): St, Extension(ctx): Extension<Ctx>) -> Response {
    let mut r = full_page(
        &st,
        &ctx,
        Nav::None,
        "Not found",
        None,
        views::error_page(&st.base, "Page not found."),
    )
    .await;
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

// ---------------------------------------------------------------- downloads

pub async fn downloads(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let q = table_query(&raw);
    let jobs: Vec<Job> = match st.call(method::JOBS_LIST, ListParams::default()).await {
        Ok(j) => j,
        Err(e) => return fail_rpc(&st, &ctx, &HeaderMap::new(), &e).await,
    };
    let content = views::downloads_page(&st.base, &jobs, &q);
    full_page(
        &st,
        &ctx,
        Nav::Downloads,
        "Downloads",
        Some("rows=1".into()),
        content,
    )
    .await
}

pub async fn table(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let q = table_query(&raw);
    match st
        .call::<Vec<Job>>(method::JOBS_LIST, ListParams::default())
        .await
    {
        Ok(jobs) => html(views::jobs_table(&st.base, &jobs, &q)),
        Err(e) => fail_rpc(&st, &ctx, &hx_headers(), &e).await,
    }
}

fn hx_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("hx-request", HeaderValue::from_static("true"));
    h
}

/// Runs a batch method for one job and answers according to who asked.
async fn job_action(
    st: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    id: u64,
    method_name: &str,
    done: &str,
) -> Response {
    let r: Result<serde_json::Value, RpcError> = st
        .call(
            method_name,
            IdsParams {
                ids: vec![JobId(id)],
            },
        )
        .await;
    let v = match r {
        Ok(v) => v,
        Err(e) => return fail_rpc(st, ctx, headers, &e).await,
    };
    if let Some(f) = v
        .get("failed")
        .and_then(|f| f.as_array())
        .and_then(|f| f.first())
    {
        let msg = f
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("action failed");
        return fail(st, ctx, headers, StatusCode::CONFLICT, msg).await;
    }
    if !is_htmx(headers) {
        return redirect(&home(st));
    }
    if hx_target(headers) == "toast" {
        return toast("info", done);
    }
    if method_name == method::JOBS_PURGE {
        return html(maud::html! {}); // empty outerHTML swap removes the row
    }
    match st
        .call::<Job>(method::JOBS_GET, IdParams { id: JobId(id) })
        .await
    {
        Ok(j) => html(views::job_row(&st.base, &j)),
        Err(e) => fail_rpc(st, ctx, headers, &e).await,
    }
}

pub async fn pause(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    job_action(&st, &c, &h, id, method::JOBS_PAUSE, "Paused.").await
}

pub async fn resume(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    job_action(&st, &c, &h, id, method::JOBS_RESUME, "Resumed.").await
}

pub async fn purge(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    job_action(
        &st,
        &c,
        &h,
        id,
        method::JOBS_PURGE,
        "Purged from the list (files untouched).",
    )
    .await
}

pub async fn reorder(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    let to = match p.get("how") {
        Some("up") => Reorder::By(-1),
        Some("down") => Reorder::By(1),
        Some("top") => Reorder::Top,
        Some("bottom") => Reorder::Bottom,
        _ => {
            return fail(
                &st,
                &c,
                &h,
                StatusCode::BAD_REQUEST,
                "unknown reorder direction",
            )
            .await;
        }
    };
    if let Err(e) = st
        .call_unit(method::JOBS_REORDER, ReorderParams { id: JobId(id), to })
        .await
    {
        return fail_rpc(&st, &c, &h, &e).await;
    }
    if !is_htmx(&h) {
        return redirect(&home(&st));
    }
    match st
        .call::<Vec<Job>>(method::JOBS_LIST, ListParams::default())
        .await
    {
        Ok(jobs) => html(views::jobs_table(&st.base, &jobs, &TableQuery::default())),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn pause_all(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap) -> Response {
    match st.call_unit(method::QUEUE_PAUSE_ALL, ()).await {
        Ok(()) => toast("info", "All downloads paused."),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn resume_all(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap) -> Response {
    match st.call_unit(method::QUEUE_RESUME_ALL, ()).await {
        Ok(()) => toast("info", "All downloads resumed."),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

// ------------------------------------------------------------------- detail

pub async fn detail(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    Path(id): Path<u64>,
) -> Response {
    match st
        .call::<JobDetail>(method::JOBS_DETAIL, IdParams { id: JobId(id) })
        .await
    {
        Ok(d) => {
            let name = d.job.name.clone();
            full_page(
                &st,
                &ctx,
                Nav::Downloads,
                &name,
                None,
                views::detail_page(&st.base, &ctx.csrf, &d),
            )
            .await
        }
        Err(e) => fail_rpc(&st, &ctx, &HeaderMap::new(), &e).await,
    }
}

pub async fn detail_live(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    Path(id): Path<u64>,
) -> Response {
    match st
        .call::<JobDetail>(method::JOBS_DETAIL, IdParams { id: JobId(id) })
        .await
    {
        Ok(d) => html(views::detail_live(&st.base, &d)),
        Err(e) => fail_rpc(&st, &ctx, &hx_headers(), &e).await,
    }
}

pub async fn remove_form(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    Path(id): Path<u64>,
) -> Response {
    match st
        .call::<Job>(method::JOBS_GET, IdParams { id: JobId(id) })
        .await
    {
        Ok(j) => {
            full_page(
                &st,
                &ctx,
                Nav::Downloads,
                "Remove download",
                None,
                views::remove_page(&st.base, &ctx.csrf, &j),
            )
            .await
        }
        Err(e) => fail_rpc(&st, &ctx, &HeaderMap::new(), &e).await,
    }
}

/// Removal. Deleting files needs the checkbox and the typed word; anything less
/// that asks for deletion removes nothing.
pub async fn remove(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    let delete_files = p.flag("delete_files");
    if delete_files && p.get("confirm").map(str::trim) != Some("DELETE") {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::BAD_REQUEST,
            "To delete files, tick the box and type DELETE. Nothing was removed.",
        )
        .await;
    }
    let params = RemoveParams {
        ids: vec![JobId(id)],
        delete_files,
        force: false,
    };
    match st
        .call::<serde_json::Value>(method::JOBS_REMOVE, params)
        .await
    {
        Ok(v) => {
            if let Some(f) = v
                .get("failed")
                .and_then(|f| f.as_array())
                .and_then(|f| f.first())
            {
                let msg = f
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("remove failed");
                return fail(&st, &c, &h, StatusCode::CONFLICT, msg).await;
            }
            redirect(&home(&st))
        }
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn rename(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    let Some(name) = p.text("name") else {
        return fail(&st, &c, &h, StatusCode::BAD_REQUEST, "name is required").await;
    };
    let params = RenameParams {
        id: JobId(id),
        name: name.to_string(),
        file_index: p.text("file_index").and_then(|v| v.parse().ok()),
    };
    match st.call_unit(method::JOBS_RENAME, params).await {
        Ok(()) => redirect(&format!("{}/jobs/{id}", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn job_limits(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    let mut options = JobOptions::default();
    for (field, slot) in [
        ("download", &mut options.max_download_limit),
        ("upload", &mut options.max_upload_limit),
    ] {
        if let Some(v) = p.text(field) {
            match fmt::parse_rate(v) {
                Some(n) => *slot = Some(n),
                None => {
                    return fail(
                        &st,
                        &c,
                        &h,
                        StatusCode::BAD_REQUEST,
                        &format!("invalid {field} limit: {v}"),
                    )
                    .await;
                }
            }
        }
    }
    match st
        .call_unit(
            method::JOBS_SET_OPTIONS,
            SetOptionsParams {
                id: JobId(id),
                options,
            },
        )
        .await
    {
        Ok(()) => redirect(&format!("{}/jobs/{id}", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

// ---------------------------------------------------------------------- add

/// How long the yt-dlp availability read from `engine.status` is reused.
const VIDEO_CACHE: std::time::Duration = std::time::Duration::from_secs(10);

/// The video options are shown when `engine.status` lists an available yt-dlp
/// (cached for up to 10 s); an unreadable status hides them.
async fn video_available(st: &AppState) -> bool {
    if let Ok(c) = st.video_cache.lock()
        && let Some((at, v)) = *c
        && at.elapsed() < VIDEO_CACHE
    {
        return v;
    }
    let available = match st
        .call::<Vec<EngineStatus>>(method::ENGINE_STATUS, ())
        .await
    {
        Ok(list) => list
            .iter()
            .any(|s| s.engine == Engine::YtDlp && s.available),
        Err(_) => false,
    };
    if let Ok(mut c) = st.video_cache.lock() {
        *c = Some((std::time::Instant::now(), available));
    }
    available
}

pub async fn add_form(State(st): St, Extension(ctx): Extension<Ctx>) -> Response {
    let video = video_available(&st).await;
    full_page(
        &st,
        &ctx,
        Nav::Add,
        "Add",
        None,
        views::add_page(&st.base, &ctx.csrf, video),
    )
    .await
}

fn outcomes(v: serde_json::Value) -> Vec<AddOutcome> {
    serde_json::from_value::<Vec<AddOutcome>>(v.clone())
        .or_else(|_| serde_json::from_value::<AddOutcome>(v).map(|o| vec![o]))
        .unwrap_or_default()
}

/// Sends `jobs.add`; returns the new ids and the failure messages.
async fn add_sources(
    st: &AppState,
    sources: Vec<AddSource>,
    options: JobOptions,
    paused: bool,
) -> Result<(Vec<u64>, Vec<String>), RpcError> {
    let v: serde_json::Value = st
        .call(
            method::JOBS_ADD,
            AddParams {
                sources,
                options,
                paused,
            },
        )
        .await?;
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for o in outcomes(v) {
        match o {
            AddOutcome::Added { id, extra_ids } => {
                ok.push(id.0);
                ok.extend(extra_ids.iter().map(|i| i.0));
            }
            AddOutcome::Failed { error } => bad.push(error.message),
        }
    }
    Ok((ok, bad))
}

pub async fn add(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap, p: Pairs) -> Response {
    let mut sources: Vec<AddSource> = p
        .get("uris")
        .unwrap_or("")
        .split_whitespace()
        .take(200)
        .map(|u| AddSource::Uris {
            uris: vec![u.to_string()],
        })
        .collect();
    if let Some(url) = p.text("video_url") {
        if !video_available(&st).await {
            return fail(
                &st,
                &c,
                &h,
                StatusCode::BAD_REQUEST,
                "The daemon has no video engine.",
            )
            .await;
        }
        let video = VideoOptions {
            quality: p.text("quality").map(str::to_string),
            playlist: p.flag("playlist"),
            ..VideoOptions::default()
        };
        sources.push(AddSource::Video {
            url: url.to_string(),
            video: Box::new(video),
        });
    }
    if sources.is_empty() {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::BAD_REQUEST,
            "Enter at least one link.",
        )
        .await;
    }
    let mut options = JobOptions {
        dir: p.text("dir").map(str::to_string),
        ..JobOptions::default()
    };
    if let Some(v) = p.text("download_limit") {
        match fmt::parse_rate(v) {
            Some(n) => options.max_download_limit = Some(n),
            None => {
                return fail(
                    &st,
                    &c,
                    &h,
                    StatusCode::BAD_REQUEST,
                    &format!("invalid download limit: {v}"),
                )
                .await;
            }
        }
    }
    match add_sources(&st, sources, options, p.flag("paused")).await {
        Ok((ok, bad)) => {
            full_page(
                &st,
                &c,
                Nav::Add,
                "Add",
                None,
                views::add_result_page(&st.base, &ok, &bad),
            )
            .await
        }
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn upload(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    mut mp: Multipart,
) -> Response {
    let mut sources = Vec::new();
    let mut paused = false;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return fail(
                    &st,
                    &c,
                    &h,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("upload failed: {e}"),
                )
                .await;
            }
        };
        let fname = field.name().unwrap_or("").to_string();
        if fname == "paused" {
            paused = true;
            continue;
        }
        if fname != "file" {
            continue;
        }
        let file = field.file_name().unwrap_or("upload").to_string();
        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => {
                return fail(
                    &st,
                    &c,
                    &h,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("upload failed: {e}"),
                )
                .await;
            }
        };
        if data.len() > MAX_UPLOAD_BYTES {
            return fail(
                &st,
                &c,
                &h,
                StatusCode::PAYLOAD_TOO_LARGE,
                "file too large (10 MiB max)",
            )
            .await;
        }
        let lower = file.to_ascii_lowercase();
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&data);
        if lower.ends_with(".torrent") {
            sources.push(AddSource::Torrent {
                name: file,
                data_base64,
            });
        } else if lower.ends_with(".metalink") || lower.ends_with(".meta4") {
            sources.push(AddSource::Metalink {
                name: file,
                data_base64,
            });
        } else {
            return fail(
                &st,
                &c,
                &h,
                StatusCode::BAD_REQUEST,
                &format!("{file}: only .torrent and .metalink files are accepted"),
            )
            .await;
        }
    }
    if sources.is_empty() {
        return fail(&st, &c, &h, StatusCode::BAD_REQUEST, "Choose a file.").await;
    }
    match add_sources(&st, sources, JobOptions::default(), paused).await {
        Ok((ok, bad)) if bad.is_empty() => {
            if is_htmx(&h) {
                toast("info", &format!("Added {} download(s).", ok.len()))
            } else {
                redirect(&home(&st))
            }
        }
        Ok((_, bad)) => fail(&st, &c, &h, StatusCode::BAD_REQUEST, &bad.join("; ")).await,
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

// ------------------------------------------------------------------- search

pub async fn search(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let pairs = query_pairs(&raw);
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let id: Option<u64> = get("id").and_then(|v| v.parse().ok());
    let providers: Vec<ProviderInfo> = st
        .call(search::PROVIDERS_LIST, ())
        .await
        .unwrap_or_default();
    let history: Vec<SearchHistoryEntry> = st
        .call(search::SEARCH_HISTORY, ())
        .await
        .unwrap_or_default();
    let snap: Option<SearchSnapshot> = match id {
        Some(i) => st
            .call(
                search::SEARCH_RESULTS,
                search::SearchIdParams { search_id: i },
            )
            .await
            .ok(),
        None => None,
    };
    let query = snap
        .as_ref()
        .map_or_else(|| get("q").unwrap_or("").to_string(), |s| s.query.clone());
    let view = SearchView {
        search_id: id,
        finished: snap.as_ref().is_some_and(|s| s.finished),
        providers: snap.as_ref().map_or(&[], |s| s.providers.as_slice()),
        results: snap.as_ref().map_or(&[], |s| s.results.as_slice()),
    };
    let content = views::search_page(&st.base, &ctx.csrf, &query, &providers, &history, &view);
    let sse = id.map(|i| format!("search={i}"));
    full_page(&st, &ctx, Nav::Search, "Search", sse, content).await
}

pub async fn search_start(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let Some(q) = p.text("q") else {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::BAD_REQUEST,
            "Enter a search query.",
        )
        .await;
    };
    let providers: Vec<String> = p.all("providers").map(str::to_string).collect();
    let params = SearchStartParams {
        query: q.to_string(),
        providers: if providers.is_empty() {
            None
        } else {
            Some(providers)
        },
        category: None,
    };
    match st.call::<SearchStarted>(search::SEARCH_START, params).await {
        Ok(s) => {
            let qs = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("id", &s.search_id.to_string())
                .finish();
            redirect(&format!("{}/search?{qs}", st.base))
        }
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn search_add(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let uri = if let Some(m) = p.text("magnet") {
        m.to_string()
    } else if let Some(t) = p.text("torrent_url") {
        t.to_string()
    } else if let Some(d) = p.text("detail_url") {
        let params = SearchResolveParams {
            detail_url: Some(d.to_string()),
            provider: p.text("provider").map(str::to_string),
            ..SearchResolveParams::default()
        };
        match st
            .call::<ResolvedMagnet>(search::SEARCH_RESOLVE, params)
            .await
        {
            Ok(r) => r.magnet,
            Err(e) => return fail_rpc(&st, &c, &h, &e).await,
        }
    } else {
        return fail(&st, &c, &h, StatusCode::BAD_REQUEST, "nothing to add").await;
    };
    let src = vec![AddSource::Uris { uris: vec![uri] }];
    match add_sources(&st, src, JobOptions::default(), false).await {
        Ok((ok, bad)) if bad.is_empty() && !ok.is_empty() => toast("info", "Added to downloads."),
        Ok((_, bad)) => fail(&st, &c, &h, StatusCode::BAD_REQUEST, &bad.join("; ")).await,
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn search_history_clear(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
) -> Response {
    match st.call_unit(search::SEARCH_HISTORY_CLEAR, ()).await {
        Ok(()) => redirect(&format!("{}/search", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

// ----------------------------------------------------------------- settings

pub async fn settings(State(st): St, Extension(ctx): Extension<Ctx>) -> Response {
    let limits: Option<Limits> = st.call(method::LIMITS_GET, ()).await.ok();
    let providers: Vec<ProviderInfo> = st
        .call(search::PROVIDERS_LIST, ())
        .await
        .unwrap_or_default();
    let schedule: Option<ScheduleStatus> = st.call(method::SCHEDULE_GET, ()).await.ok();
    let content = views::settings_page(
        &st.base,
        &ctx.csrf,
        limits.as_ref(),
        &providers,
        schedule.as_ref(),
    );
    full_page(&st, &ctx, Nav::Settings, "Settings", None, content).await
}

pub async fn settings_limits(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let mut u = LimitsUpdate::default();
    for (field, slot) in [("download", &mut u.download), ("upload", &mut u.upload)] {
        if let Some(v) = p.text(field) {
            match fmt::parse_rate(v) {
                Some(n) => *slot = Some(n),
                None => {
                    return fail(
                        &st,
                        &c,
                        &h,
                        StatusCode::BAD_REQUEST,
                        &format!("invalid {field} limit: {v}"),
                    )
                    .await;
                }
            }
        }
    }
    if let Some(v) = p.text("max_concurrent") {
        match v.parse::<u32>() {
            Ok(n) if n >= 1 => u.max_concurrent = Some(n),
            _ => {
                return fail(
                    &st,
                    &c,
                    &h,
                    StatusCode::BAD_REQUEST,
                    "max concurrent must be a positive number",
                )
                .await;
            }
        }
    }
    match st.call_unit(method::LIMITS_SET, u).await {
        Ok(()) => redirect(&format!("{}/settings", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn provider_set(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let Some(name) = p.text("name") else {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::BAD_REQUEST,
            "provider name missing",
        )
        .await;
    };
    let params = ProviderSetParams {
        name: name.to_string(),
        enabled: Some(p.flag("enabled")),
        ..ProviderSetParams::default()
    };
    match st.call::<ProviderInfo>(search::PROVIDERS_SET, params).await {
        Ok(info) => {
            let idx = p.text("idx").and_then(|i| i.parse().ok()).unwrap_or(0);
            html(views::provider_row(&st.base, idx, &info))
        }
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn provider_test(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let Some(name) = p.text("name") else {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::BAD_REQUEST,
            "provider name missing",
        )
        .await;
    };
    match st
        .call::<ProviderTestReport>(
            search::PROVIDERS_TEST,
            ProviderNameParams {
                name: name.to_string(),
            },
        )
        .await
    {
        Ok(r) if r.ok => toast(
            "info",
            &format!(
                "{}: ok, {} results in {} ms",
                r.name, r.results, r.elapsed_ms
            ),
        ),
        Ok(r) => toast(
            "error",
            &format!("{}: {}", r.name, r.error.unwrap_or_else(|| "failed".into())),
        ),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}
