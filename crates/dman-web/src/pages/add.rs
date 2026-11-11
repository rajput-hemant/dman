//! Adding downloads: paste box, add page (links, files, options), magnet file
//! selection (`jobs.preview_magnet`) and the folder picker (`fs.list`).

use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::Engine as _;
use dman_proto::method::{
    self, EngineStatus, FsListParams, FsListing, ListParams, MagnetPreview, PreviewMagnetParams,
};
use dman_proto::{
    AddOutcome, AddParams, AddSource, Engine, Job, JobOptions, RpcError, VideoOptions,
};
use maud::{Markup, html};

use super::added_text;
use super::quality::{self, Gate};
use super::{
    Pairs, St, fail, full_page, html, hx_target, is_htmx, query_pairs, redirect, toast_markup,
    with_header,
};
use crate::auth::{Ctx, random_id};
use crate::views::{self, AddErrors, AddPage, AddValues, Nav, TableQuery};
use crate::{AppState, fmt};

/// A fetched magnet file list, kept until the add form comes back.
pub struct StoredPreview {
    at: Instant,
    uri: String,
    preview: MagnetPreview,
}

const PREVIEW_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_PREVIEWS: usize = 20;
const PREVIEW_TIMEOUT_SECS: u32 = 120;

/// How long the yt-dlp availability read from `engine.status` is reused.
const VIDEO_CACHE: Duration = Duration::from_secs(10);

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
        *c = Some((Instant::now(), available));
    }
    available
}

async fn default_dir(st: &AppState) -> Option<String> {
    st.call::<FsListing>(
        method::FS_LIST,
        FsListParams {
            path: None,
            dirs_only: true,
        },
    )
    .await
    .ok()
    .map(|l| l.path)
}

fn values_of(p: &Pairs) -> AddValues {
    AddValues {
        uris: p.get("uris").unwrap_or("").trim().to_string(),
        dir: p.str("dir").to_string(),
        download_limit: p.str("download_limit").to_string(),
        paused: p.flag("paused"),
        seed: p.flag("seed"),
        as_video: p.flag("as_video"),
        quality: p.str("quality").to_string(),
        playlist: p.flag("playlist"),
    }
}

/// Renders the add page with what the user typed.
async fn render_add(
    st: &AppState,
    ctx: &Ctx,
    values: &AddValues,
    errors: &AddErrors,
    preview: Option<Markup>,
    note: Option<String>,
    status: StatusCode,
) -> Response {
    let video = video_available(st).await;
    let dir = default_dir(st).await;
    let page = AddPage {
        video,
        default_dir: dir.as_deref(),
        values,
        errors,
        preview,
        note,
    };
    let mut r = full_page(
        st,
        ctx,
        Nav::Add,
        "Add downloads",
        "",
        Vec::new(),
        views::add_page(&st.base, &ctx.csrf, &page),
    )
    .await;
    *r.status_mut() = status;
    r
}

pub async fn add_form(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let p = query_pairs(&raw);
    let values = AddValues {
        uris: p.str("uri").to_string(),
        ..AddValues::default()
    };
    render_add(
        &st,
        &ctx,
        &values,
        &AddErrors::default(),
        None,
        None,
        StatusCode::OK,
    )
    .await
}

/// Links the add form accepts: magnets and the schemes the engines download. Anything
/// else (`file:`, `javascript:`, ...) is refused before it reaches the daemon.
fn looks_like_link(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("magnet:?")
        || ["http://", "https://", "ftp://", "sftp://"]
            .iter()
            .any(|p| l.starts_with(p))
}

/// A parsed add request: sources with a label each (for error messages).
struct Parsed {
    values: AddValues,
    errors: AddErrors,
    sources: Vec<(String, AddSource)>,
    options: JobOptions,
    preview_id: Option<String>,
}

fn parse_add(st: &AppState, p: &Pairs, video_ok: bool) -> Parsed {
    let values = values_of(p);
    let mut errors = AddErrors::default();
    let mut sources = Vec::new();
    if let Some(e) = &p.error {
        errors.general.push(e.clone());
    }
    // A magnet whose file list was fetched: add its .torrent with the selection.
    let mut skip_uri = None;
    let preview_id = p.text("preview_id").map(str::to_string);
    if let Some(id) = &preview_id {
        let stored = st
            .previews
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|s| (s.uri.clone(), s.preview.clone())));
        match stored {
            Some((uri, pv)) => {
                let picked: Vec<u32> = p.all("select").filter_map(|v| v.parse().ok()).collect();
                if picked.is_empty() {
                    errors.general.push(
                        "Tick at least one file of the magnet, or remove the magnet link.".into(),
                    );
                }
                skip_uri = Some(uri);
                sources.push((
                    pv.name.clone(),
                    AddSource::Torrent {
                        name: format!("{}.torrent", pv.name),
                        data_base64: pv.torrent_base64.clone(),
                    },
                ));
            }
            None => errors.general.push(
                "The file list has expired. Press \u{201c}Choose files\u{2026}\u{201d} again."
                    .into(),
            ),
        }
    }
    let as_video = values.as_video && video_ok;
    // The paste box has no "video" checkbox: known video sites (an offline
    // heuristic, as in the CLI and TUI) go to yt-dlp.
    let detect = video_ok && !as_video && p.str("from") == "downloads";
    for line in values.uris.split_whitespace().take(200) {
        if skip_uri.as_deref() == Some(line) {
            continue;
        }
        if !looks_like_link(line) {
            errors.uris = Some(format!(
                "\u{201c}{line}\u{201d} is not a link. Links start with magnet:?, http://, https://, ftp:// or sftp://."
            ));
            continue;
        }
        let is_video = as_video
            || (detect
                && url::Url::parse(line).is_ok_and(|u| dman_ytdlp::is_probably_video_url(&u)));
        let src = if is_video {
            AddSource::Video {
                url: line.to_string(),
                video: Box::new(VideoOptions {
                    quality: Some(values.quality.clone()).filter(|q| !q.is_empty()),
                    playlist: values.playlist,
                    ..VideoOptions::default()
                }),
            }
        } else {
            AddSource::Uris {
                uris: vec![line.to_string()],
            }
        };
        sources.push((line.to_string(), src));
    }
    // Older form field: one video page URL.
    if let Some(url) = p.text("video_url") {
        if video_ok {
            let video = VideoOptions {
                quality: p.text("quality").map(str::to_string),
                playlist: p.flag("playlist"),
                ..VideoOptions::default()
            };
            sources.push((
                url.to_string(),
                AddSource::Video {
                    url: url.to_string(),
                    video: Box::new(video),
                },
            ));
        } else {
            errors
                .general
                .push("This dman has no video engine (yt-dlp is not installed).".into());
        }
    }
    for f in &p.files {
        let lower = f.file_name.to_ascii_lowercase();
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&f.data);
        let name = f.file_name.clone();
        if lower.ends_with(".torrent") {
            sources.push((name.clone(), AddSource::Torrent { name, data_base64 }));
        } else if lower.ends_with(".metalink") || lower.ends_with(".meta4") {
            sources.push((name.clone(), AddSource::Metalink { name, data_base64 }));
        } else {
            errors.general.push(format!(
                "{name}: only .torrent and .metalink files can be added."
            ));
        }
    }
    if sources.is_empty() && !errors.any() {
        errors.uris = Some("Paste at least one link or choose a .torrent file.".into());
    }
    let mut options = JobOptions {
        dir: Some(values.dir.clone()).filter(|d| !d.is_empty()),
        ..JobOptions::default()
    };
    if !values.download_limit.is_empty() {
        match fmt::parse_rate(&values.download_limit) {
            Some(n) => options.max_download_limit = Some(n),
            None => {
                errors.limit = Some(format!(
                    "\u{201c}{}\u{201d} is not a speed. Use a number with K, M or G, e.g. 500K.",
                    values.download_limit
                ));
            }
        }
    }
    if let Some(sel) = preview_id.as_ref().and_then(|_| {
        let picked: Vec<u32> = p.all("select").filter_map(|v| v.parse().ok()).collect();
        (!picked.is_empty()).then_some(picked)
    }) {
        options.select_files = Some(sel);
    }
    if values.seed {
        options.seed = Some(true);
    }
    Parsed {
        values,
        errors,
        sources,
        options,
        preview_id,
    }
}

/// Sends `jobs.add`; returns the new ids and `label: message` per failure.
pub(super) async fn add_sources(
    st: &AppState,
    sources: Vec<(String, AddSource)>,
    options: JobOptions,
    paused: bool,
) -> Result<(Vec<u64>, Vec<(String, String)>), RpcError> {
    let (labels, sources): (Vec<String>, Vec<AddSource>) = sources.into_iter().unzip();
    let v: serde_json::Value = st
        .call(
            method::JOBS_ADD,
            AddParams {
                sources,
                options,
                paused,
                force: false,
            },
        )
        .await?;
    let outcomes = serde_json::from_value::<Vec<AddOutcome>>(v.clone())
        .or_else(|_| serde_json::from_value::<AddOutcome>(v).map(|o| vec![o]))
        .unwrap_or_default();
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for (i, o) in outcomes.into_iter().enumerate() {
        match o {
            AddOutcome::Added { id, extra_ids } => {
                ok.push(id.0);
                ok.extend(extra_ids.iter().map(|i| i.0));
            }
            AddOutcome::Failed { error } => {
                bad.push((labels.get(i).cloned().unwrap_or_default(), error.message));
            }
        }
    }
    Ok((ok, bad))
}

/// `POST /add` (also `/add/upload`): links, files and options, from the paste box,
/// the add page, or a file dropped on any page.
pub async fn add(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap, p: Pairs) -> Response {
    let video_ok = video_available(&st).await;
    let mut parsed = parse_add(&st, &p, video_ok);
    let htmx = is_htmx(&h);
    if parsed.errors.any() {
        if htmx {
            let e = &parsed.errors;
            let msg = e
                .uris
                .clone()
                .or_else(|| e.limit.clone())
                .or_else(|| e.dir.clone())
                .unwrap_or_else(|| e.general.join(" "));
            return toast_markup(views::toast("error", &msg));
        }
        if p.error.is_some() && p.files.is_empty() && p.get("uris").is_none() {
            return fail(
                &st,
                &c,
                &h,
                StatusCode::PAYLOAD_TOO_LARGE,
                p.error.as_deref().unwrap_or(""),
            )
            .await;
        }
        return render_add(
            &st,
            &c,
            &parsed.values,
            &parsed.errors,
            None,
            None,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let sources = std::mem::take(&mut parsed.sources);
    if parsed.preview_id.is_none() {
        let srcs: Vec<AddSource> = sources.iter().map(|(_, s)| s.clone()).collect();
        let from = p.str("from");
        match quality::gate(&st, &srcs, &parsed.options, parsed.values.paused, from).await {
            Gate::Direct => {}
            Gate::Picker(id) => {
                let to = format!("{}/add/quality/{id}", st.base);
                return if htmx {
                    with_header(html(html! {}), "hx-redirect", &to)
                } else {
                    redirect(&to)
                };
            }
            Gate::Failed(msg) => {
                if htmx {
                    return toast_markup(views::toast("error", &msg));
                }
                parsed.errors.general.push(msg);
                parsed.sources = sources;
                return render_add(
                    &st,
                    &c,
                    &parsed.values,
                    &parsed.errors,
                    None,
                    None,
                    StatusCode::BAD_GATEWAY,
                )
                .await;
            }
        }
    }
    let (ok, bad) =
        match add_sources(&st, sources, parsed.options.clone(), parsed.values.paused).await {
            Ok(r) => r,
            Err(e) => {
                if htmx {
                    return toast_markup(views::toast(
                        "error",
                        &format!("Could not add: {}", e.message),
                    ));
                }
                parsed.errors.general.push(e.message);
                return render_add(
                    &st,
                    &c,
                    &parsed.values,
                    &parsed.errors,
                    None,
                    None,
                    StatusCode::BAD_GATEWAY,
                )
                .await;
            }
        };
    if !ok.is_empty()
        && let Some(id) = &parsed.preview_id
        && let Ok(mut m) = st.previews.lock()
    {
        m.remove(id);
    }
    let jobs: Vec<Job> = st
        .call(method::JOBS_LIST, ListParams::default())
        .await
        .unwrap_or_default();
    let ids = ok.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    if htmx {
        let mut toasts = Vec::new();
        if !ok.is_empty() {
            toasts.push(views::toast("ok", &added_text(&jobs, &ok)));
        }
        for (label, msg) in &bad {
            toasts.push(views::toast(
                "error",
                &format!("Could not add {label}: {msg}"),
            ));
        }
        let trigger = serde_json::json!({ "dman-added": { "ids": ok } }).to_string();
        let r = if hx_target(&h) == "jobs" {
            let mut q = TableQuery::from_pairs(p.iter());
            q.fresh = ok.clone();
            html(html! {
                (views::jobs_table(&st.base, &c.csrf, &jobs, &q))
                @for t in toasts { (views::oob_toast(t)) }
            })
        } else {
            toast_markup(html! { @for t in toasts { (t) } })
        };
        let r = if ok.is_empty() {
            r
        } else {
            with_header(r, "hx-trigger", &trigger)
        };
        return with_header(r, "x-dman-added", &ids);
    }
    if bad.is_empty() {
        return redirect(&format!("{}/?added={ids}", st.base));
    }
    // Keep the lines that failed in the form, with the reasons.
    let failed: Vec<&str> = bad.iter().map(|(l, _)| l.as_str()).collect();
    let mut values = parsed.values.clone();
    values.uris = values
        .uris
        .split_whitespace()
        .filter(|l| failed.contains(l))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = AddErrors {
        general: bad.iter().map(|(l, m)| format!("{l}: {m}")).collect(),
        ..AddErrors::default()
    };
    let note = (!ok.is_empty()).then(|| {
        format!(
            "{} The rest failed; fix the links below and try again.",
            added_text(&jobs, &ok)
        )
    });
    let status = if ok.is_empty() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    render_add(&st, &c, &values, &errors, None, note, status).await
}

/// `POST /add/preview`: fetch the file list of the single magnet (or http(s)
/// `.torrent` URL) in the form.
pub async fn preview(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let values = values_of(&p);
    let magnets: Vec<&str> = values
        .uris
        .split_whitespace()
        .filter(|l| l.to_ascii_lowercase().starts_with("magnet:?") || dman_proto::is_torrent_url(l))
        .collect();
    let panel = match magnets.as_slice() {
        [one] => {
            let params = PreviewMagnetParams {
                uri: (*one).to_string(),
                dir: None,
                timeout_secs: Some(PREVIEW_TIMEOUT_SECS),
            };
            match st
                .call::<MagnetPreview>(method::JOBS_PREVIEW_MAGNET, params)
                .await
            {
                Ok(pv) => {
                    let id = random_id();
                    let m = views::preview_panel(&id, &pv);
                    if let Ok(mut map) = st.previews.lock() {
                        map.retain(|_, s| s.at.elapsed() < PREVIEW_TTL);
                        if map.len() >= MAX_PREVIEWS
                            && let Some(old) =
                                map.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone())
                        {
                            map.remove(&old);
                        }
                        map.insert(
                            id,
                            StoredPreview {
                                at: Instant::now(),
                                uri: (*one).to_string(),
                                preview: pv,
                            },
                        );
                    }
                    m
                }
                Err(e) => views::preview_error(&format!(
                    "Could not fetch the file list: {}. You can still press Add to download every file.",
                    e.message
                )),
            }
        }
        [] => views::preview_error(
            "Paste a magnet link or the http(s) URL of a .torrent file first. Files can be chosen for those only; other URLs download everything.",
        ),
        _ => views::preview_error(
            "Choosing files works for one magnet link at a time. Remove the other links, or add them separately.",
        ),
    };
    if is_htmx(&h) {
        return html(panel);
    }
    render_add(
        &st,
        &c,
        &values,
        &AddErrors::default(),
        Some(panel),
        None,
        StatusCode::OK,
    )
    .await
}

/// `GET /fs?path=..` (or `dir=..`): the folder picker fragment.
pub async fn fs(State(st): St, RawQuery(raw): RawQuery) -> Response {
    let p = query_pairs(&raw);
    let path = p.text("path").or_else(|| p.text("dir")).map(str::to_string);
    let first = st
        .call::<FsListing>(
            method::FS_LIST,
            FsListParams {
                path: path.clone(),
                dirs_only: true,
            },
        )
        .await;
    match first {
        Ok(l) => html(views::fs_picker(&st.base, &l)),
        Err(e) if path.is_some() => {
            match st
                .call::<FsListing>(
                    method::FS_LIST,
                    FsListParams {
                        path: None,
                        dirs_only: true,
                    },
                )
                .await
            {
                Ok(l) => html(html! {
                    p.help { "\u{201c}" (path.unwrap_or_default()) "\u{201d}: " (e.message) ". Showing the default folder instead." }
                    (views::fs_picker(&st.base, &l))
                }),
                Err(e) => html(views::preview_error(&e.message)),
            }
        }
        Err(e) => html(views::preview_error(&format!(
            "Cannot list folders: {}",
            e.message
        ))),
    }
}
