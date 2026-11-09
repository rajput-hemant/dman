//! Downloads list, job actions (single and bulk), detail, remove, rename, options.

use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use dman_proto::method::{
    self, IdParams, IdsParams, ListParams, RemoveParams, RenameParams, Reorder, ReorderParams,
    SetOptionsParams,
};
use dman_proto::{Job, JobDetail, JobId, JobOptions, RpcError};
use maud::html;

use super::{
    Pairs, St, fail, fail_rpc, flash, full_page, html, hx_target, is_htmx, query_pairs, redirect,
    toast, toast_markup, with_header,
};
use crate::auth::Ctx;
use crate::views::{self, Nav, Note, OptValues, RemoveTarget, TableQuery};
use crate::{AppState, fmt};

async fn list(st: &AppState) -> Result<Vec<Job>, RpcError> {
    st.call(method::JOBS_LIST, ListParams::default()).await
}

fn ids_of(p: &Pairs) -> Vec<JobId> {
    p.all("ids")
        .flat_map(|v| v.split(','))
        .filter_map(|v| v.trim().parse().ok())
        .map(JobId)
        .collect()
}

fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

// ---------------------------------------------------------------- downloads

pub async fn downloads(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let p = query_pairs(&raw);
    let mut q = TableQuery::from_pairs(p.iter());
    q.fresh = p
        .str("added")
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .take(50)
        .collect();
    let jobs = match list(&st).await {
        Ok(j) => j,
        Err(e) => return fail_rpc(&st, &ctx, &HeaderMap::new(), &e).await,
    };
    let mut toasts = flash(&p);
    if !q.fresh.is_empty() {
        toasts.push(views::toast("ok", &added_text(&jobs, &q.fresh)));
    }
    let content = views::downloads_page(&st.base, &ctx.csrf, &jobs, &q);
    full_page(
        &st,
        &ctx,
        Nav::Downloads,
        "Downloads",
        "rows=1",
        toasts,
        content,
    )
    .await
}

/// "Added “x”." or "Added 3 downloads."
pub fn added_text(jobs: &[Job], ids: &[u64]) -> String {
    match ids {
        [one] => match jobs.iter().find(|j| j.id.0 == *one) {
            Some(j) => format!("Added {}.", quoted(views::display_name(j))),
            None => "Added 1 download.".into(),
        },
        many => format!("Added {} downloads.", many.len()),
    }
}

pub async fn table(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let q = TableQuery::from_pairs(query_pairs(&raw).iter());
    match list(&st).await {
        Ok(jobs) => html(views::jobs_table(&st.base, &ctx.csrf, &jobs, &q)),
        Err(e) => toast("error", &e.message),
    }
}

/// The re-rendered table (current filter from the request) plus out-of-band toasts.
async fn table_with(st: &AppState, ctx: &Ctx, p: &Pairs, toasts: Vec<maud::Markup>) -> Response {
    let q = TableQuery::from_pairs(p.iter());
    match list(st).await {
        Ok(jobs) => html(html! {
            (views::jobs_table(&st.base, &ctx.csrf, &jobs, &q))
            @for t in toasts { (views::oob_toast(t)) }
        }),
        Err(e) => toast("error", &e.message),
    }
}

/// First failure message of a batch result.
fn batch_failure(v: &serde_json::Value) -> Option<String> {
    let f = v.get("failed")?.as_array()?.first()?;
    Some(
        f.pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("action failed")
            .to_string(),
    )
}

/// Runs a batch method for one job and answers according to who asked: the table
/// (`#jobs`), the detail header (`#dhead`) or a toast.
async fn job_action(
    st: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    id: u64,
    p: &Pairs,
    method_name: &str,
    done: (&str, &str),
) -> Response {
    let name = st
        .call::<Job>(method::JOBS_GET, IdParams { id: JobId(id) })
        .await
        .map(|j| views::display_name(&j).to_string())
        .unwrap_or_else(|_| format!("download {id}"));
    let r: Result<serde_json::Value, RpcError> = st
        .call(
            method_name,
            IdsParams {
                ids: vec![JobId(id)],
            },
        )
        .await;
    let msg = match r {
        Ok(v) => batch_failure(&v).map_or(Ok(()), Err),
        Err(e) => Err(e.message),
    };
    if let Err(m) = msg {
        let text = format!("Could not {} {}: {m}", done.0, quoted(&name));
        return fail(st, ctx, headers, StatusCode::CONFLICT, &text).await;
    }
    let text = format!("{} {}", done.1, quoted(&name));
    if !is_htmx(headers) {
        let code = match method_name {
            method::JOBS_PAUSE => "paused",
            method::JOBS_PURGE => "cleared",
            _ => "resumed",
        };
        return if p.str("back") == "detail" && method_name != method::JOBS_PURGE {
            redirect(&format!("{}/jobs/{id}?done={code}", st.base))
        } else {
            redirect(&format!("{}/?done={code}", st.base))
        };
    }
    let t = views::toast("ok", &format!("{text}."));
    match hx_target(headers) {
        "jobs" => table_with(st, ctx, p, vec![t]).await,
        "dhead" => match st
            .call::<Job>(method::JOBS_GET, IdParams { id: JobId(id) })
            .await
        {
            Ok(j) => html(html! {
                (views::detail_head(&st.base, Some(&ctx.csrf), &j))
                (views::oob_toast(t))
            }),
            Err(_) => toast_markup(t),
        },
        _ => toast_markup(t),
    }
}

pub async fn pause(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    job_action(&st, &c, &h, id, &p, method::JOBS_PAUSE, ("pause", "Paused")).await
}

pub async fn resume(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    job_action(
        &st,
        &c,
        &h,
        id,
        &p,
        method::JOBS_RESUME,
        ("resume", "Resumed"),
    )
    .await
}

pub async fn purge(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    job_action(
        &st,
        &c,
        &h,
        id,
        &p,
        method::JOBS_PURGE,
        ("clear", "Cleared from the list (files untouched):"),
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
        return redirect(&format!("{}/?done=moved", st.base));
    }
    table_with(&st, &c, &p, vec![views::toast("ok", "Queue updated.")]).await
}

pub async fn pause_all(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap) -> Response {
    match st.call_unit(method::QUEUE_PAUSE_ALL, ()).await {
        Ok(()) if is_htmx(&h) => toast("ok", "All downloads paused."),
        Ok(()) => redirect(&format!("{}/?done=paused", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

pub async fn resume_all(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap) -> Response {
    match st.call_unit(method::QUEUE_RESUME_ALL, ()).await {
        Ok(()) if is_htmx(&h) => toast("ok", "All downloads resumed."),
        Ok(()) => redirect(&format!("{}/?done=resumed", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

/// `POST /jobs/bulk` with `action=pause|resume` and `ids`.
pub async fn bulk(State(st): St, Extension(c): Extension<Ctx>, h: HeaderMap, p: Pairs) -> Response {
    let ids = ids_of(&p);
    let (m, verb, code) = match p.str("action") {
        "pause" => (method::JOBS_PAUSE, "Paused", "paused"),
        "resume" => (method::JOBS_RESUME, "Resumed", "resumed"),
        _ => return fail(&st, &c, &h, StatusCode::BAD_REQUEST, "Unknown action.").await,
    };
    if ids.is_empty() {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::BAD_REQUEST,
            "Tick one or more downloads first.",
        )
        .await;
    }
    let n = ids.len();
    let v: serde_json::Value = match st.call(m, IdsParams { ids }).await {
        Ok(v) => v,
        Err(e) => return fail_rpc(&st, &c, &h, &e).await,
    };
    let failed = v
        .get("failed")
        .and_then(|f| f.as_array())
        .map_or(0, Vec::len);
    if !is_htmx(&h) {
        return redirect(&format!("{}/?done={code}", st.base));
    }
    let mut toasts = vec![views::toast(
        "ok",
        &format!(
            "{verb} {} download{}.",
            n - failed,
            if n - failed == 1 { "" } else { "s" }
        ),
    )];
    if let Some(f) = batch_failure(&v) {
        toasts.push(views::toast(
            "error",
            &format!("{failed} could not be changed: {f}"),
        ));
    }
    with_header(
        table_with(&st, &c, &p, toasts).await,
        "hx-trigger",
        "dman-clear-sel",
    )
}

/// `POST /jobs/clear-removed`: forgets every removed download (files untouched).
pub async fn clear_removed(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let jobs = match list(&st).await {
        Ok(j) => j,
        Err(e) => return fail_rpc(&st, &c, &h, &e).await,
    };
    let ids: Vec<JobId> = jobs
        .iter()
        .filter(|j| j.state == dman_proto::JobState::Removed)
        .map(|j| j.id)
        .collect();
    let n = ids.len();
    if n > 0
        && let Err(e) = st.call_unit(method::JOBS_PURGE, IdsParams { ids }).await
    {
        return fail_rpc(&st, &c, &h, &e).await;
    }
    if !is_htmx(&h) {
        return redirect(&format!("{}/?done=cleared", st.base));
    }
    let text = format!(
        "Cleared {n} removed download{} from the list. Files were not touched.",
        if n == 1 { "" } else { "s" }
    );
    table_with(&st, &c, &p, vec![views::toast("ok", &text)]).await
}

// ------------------------------------------------------------------- remove

fn remove_target<'a>(
    back: &'a str,
    jobs: &'a [Job],
    ids: &[JobId],
    base: &str,
) -> RemoveTarget<'a> {
    let picked: Vec<&Job> = jobs.iter().filter(|j| ids.contains(&j.id)).collect();
    let (action, cancel_href) = match picked.as_slice() {
        [one] if ids.len() == 1 => (
            format!("{base}/jobs/{}/remove", one.id.0),
            if back == "detail" {
                format!("{base}/jobs/{}", one.id.0)
            } else {
                format!("{base}/")
            },
        ),
        _ => (format!("{base}/jobs/bulk/remove"), format!("{base}/")),
    };
    RemoveTarget {
        action,
        jobs: picked,
        back,
        cancel_href,
    }
}

/// The confirmation, as a modal fragment (htmx) or a full page.
async fn confirm_page(
    st: &AppState,
    ctx: &Ctx,
    h: &HeaderMap,
    ids: Vec<JobId>,
    back: &str,
    delete: bool,
) -> Response {
    if ids.is_empty() {
        return fail(
            st,
            ctx,
            h,
            StatusCode::BAD_REQUEST,
            "Tick one or more downloads first.",
        )
        .await;
    }
    let jobs = match list(st).await {
        Ok(j) => j,
        Err(e) => return fail_rpc(st, ctx, h, &e).await,
    };
    let t = remove_target(back, &jobs, &ids, &st.base);
    if t.jobs.is_empty() {
        return fail(
            st,
            ctx,
            h,
            StatusCode::NOT_FOUND,
            "That download no longer exists.",
        )
        .await;
    }
    let form = views::remove_form(&st.base, &ctx.csrf, &t, delete);
    if is_htmx(h) {
        return html(form);
    }
    full_page(
        st,
        ctx,
        Nav::Downloads,
        if delete { "Delete files" } else { "Remove" },
        "",
        Vec::new(),
        html! { div.card.narrow { (form) } },
    )
    .await
}

pub async fn remove_form(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    RawQuery(raw): RawQuery,
) -> Response {
    let p = query_pairs(&raw);
    confirm_page(
        &st,
        &c,
        &h,
        vec![JobId(id)],
        p.str("back"),
        p.flag("delete"),
    )
    .await
}

pub async fn bulk_remove_form(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Response {
    let p = query_pairs(&raw);
    confirm_page(&st, &c, &h, ids_of(&p), "", p.flag("delete")).await
}

pub async fn remove(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    do_remove(&st, &c, &h, vec![JobId(id)], &p).await
}

pub async fn bulk_remove(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    do_remove(&st, &c, &h, ids_of(&p), &p).await
}

/// Removal after the one confirmation. Deleting files needs `delete_files=true`,
/// which only the "Delete files" confirmation sends.
async fn do_remove(st: &AppState, c: &Ctx, h: &HeaderMap, ids: Vec<JobId>, p: &Pairs) -> Response {
    if ids.is_empty() {
        return fail(
            st,
            c,
            h,
            StatusCode::BAD_REQUEST,
            "Tick one or more downloads first.",
        )
        .await;
    }
    // Files are deleted only on the explicit flag of the "Delete files" confirmation;
    // any other remove keeps them.
    let delete_files = p.get("delete_files") == Some("true");
    let back = p.str("back");
    let n = ids.len();
    let name = if n == 1 {
        st.call::<Job>(method::JOBS_GET, IdParams { id: ids[0] })
            .await
            .map(|j| quoted(views::display_name(&j)))
            .unwrap_or_else(|_| "the download".into())
    } else {
        format!("{n} downloads")
    };
    let params = RemoveParams {
        ids,
        delete_files,
        force: false,
    };
    let v: serde_json::Value = match st.call(method::JOBS_REMOVE, params).await {
        Ok(v) => v,
        Err(e) => return fail_rpc(st, c, h, &e).await,
    };
    if let Some(m) = batch_failure(&v) {
        // The daemon explains what did and did not happen (e.g. removed, but the
        // files were unknown and so kept); the dialog closes either way.
        let msg = format!("{name}: {m}");
        if is_htmx(h) {
            return with_header(
                toast("error", &msg),
                "hx-trigger",
                "dman-close, dman-clear-sel",
            );
        }
        return fail(st, c, h, StatusCode::CONFLICT, &msg).await;
    }
    let code = if delete_files {
        "removed-files"
    } else {
        "removed"
    };
    if !is_htmx(h) {
        return redirect(&format!("{}/?done={code}", st.base));
    }
    if back == "detail" {
        return with_header(
            html(html! {}),
            "hx-redirect",
            &format!("{}/?done={code}", st.base),
        );
    }
    let text = if delete_files {
        format!("Removed {name} and deleted its files.")
    } else {
        format!("Removed {name} (files kept).")
    };
    with_header(
        toast("ok", &text),
        "hx-trigger",
        "dman-close, dman-clear-sel",
    )
}

// ------------------------------------------------------------------- detail

pub async fn detail(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    Path(id): Path<u64>,
    RawQuery(raw): RawQuery,
) -> Response {
    match st
        .call::<JobDetail>(method::JOBS_DETAIL, IdParams { id: JobId(id) })
        .await
    {
        Ok(d) => {
            let name = views::display_name(&d.job).to_string();
            full_page(
                &st,
                &ctx,
                Nav::Downloads,
                &name,
                &format!("job={id}"),
                flash(&query_pairs(&raw)),
                views::detail_page(&st.base, &ctx.csrf, &d),
            )
            .await
        }
        Err(e) => fail_rpc(&st, &ctx, &HeaderMap::new(), &e).await,
    }
}

pub async fn rename(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    let name = p.str("name");
    let result = if name.is_empty() {
        Err("Enter a new name.".to_string())
    } else if name.contains('/') || name == ".." || name == "." {
        Err("Use a plain name without slashes.".to_string())
    } else {
        let params = RenameParams {
            id: JobId(id),
            name: name.to_string(),
            file_index: p.text("file_index").and_then(|v| v.parse().ok()),
        };
        st.call_unit(method::JOBS_RENAME, params)
            .await
            .map_err(|e| e.message)
    };
    let back = format!("{}/jobs/{id}?done=renamed", st.base);
    match result {
        Ok(()) if is_htmx(&h) => with_header(html(html! {}), "hx-redirect", &back),
        Ok(()) => redirect(&back),
        Err(msg) if is_htmx(&h) => {
            match st
                .call::<JobDetail>(method::JOBS_DETAIL, IdParams { id: JobId(id) })
                .await
            {
                Ok(d) => html(views::rename_form(&st.base, &c.csrf, &d, name, Some(&msg))),
                Err(e) => toast("error", &e.message),
            }
        }
        Err(msg) => {
            fail(
                &st,
                &c,
                &h,
                StatusCode::BAD_REQUEST,
                &format!("Could not rename: {msg}"),
            )
            .await
        }
    }
}

/// Parses a changed rate field: empty = no limit.
fn changed_rate(p: &Pairs, field: &str) -> Result<Option<u64>, String> {
    let v = p.str(field);
    if p.get(field).is_none() || v == p.str(&format!("o_{field}")) {
        return Ok(None);
    }
    if v.is_empty() {
        return Ok(Some(0));
    }
    fmt::parse_rate(v).map(Some).ok_or_else(|| {
        format!("\u{201c}{v}\u{201d} is not a speed. Use a number with K, M or G, e.g. 500K.")
    })
}

fn parse_options(p: &Pairs) -> Result<JobOptions, (&'static str, String)> {
    let mut o = JobOptions {
        max_download_limit: changed_rate(p, "download").map_err(|m| ("download", m))?,
        max_upload_limit: changed_rate(p, "upload").map_err(|m| ("upload", m))?,
        ..JobOptions::default()
    };
    let ratio = p.str("seed_ratio");
    if p.get("seed_ratio").is_some() && !ratio.is_empty() && ratio != p.str("o_seed_ratio") {
        match ratio.parse::<f64>() {
            Ok(r) if r.is_finite() && r >= 0.0 => o.seed_ratio = Some(r),
            _ => return Err(("seed_ratio", "Enter a ratio such as 1.0 or 2.5.".into())),
        }
    }
    let time = p.str("seed_time");
    if p.get("seed_time").is_some() && !time.is_empty() && time != p.str("o_seed_time") {
        match time.parse::<u64>() {
            Ok(t) => o.seed_time = Some(t),
            _ => return Err(("seed_time", "Enter whole minutes, e.g. 60.".into())),
        }
    }
    Ok(o)
}

pub async fn job_limits(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
    p: Pairs,
) -> Response {
    let cur = OptValues {
        download: p.str("download").to_string(),
        upload: p.str("upload").to_string(),
        seed_ratio: p.str("seed_ratio").to_string(),
        seed_time: p.str("seed_time").to_string(),
    };
    let orig = OptValues {
        download: p.str("o_download").to_string(),
        upload: p.str("o_upload").to_string(),
        seed_ratio: p.str("o_seed_ratio").to_string(),
        seed_time: p.str("o_seed_time").to_string(),
    };
    let result = match parse_options(&p) {
        Err((f, m)) => Err((f, m)),
        Ok(o) if o == JobOptions::default() => Ok(()),
        Ok(options) => st
            .call_unit(
                method::JOBS_SET_OPTIONS,
                SetOptionsParams {
                    id: JobId(id),
                    options,
                },
            )
            .await
            .map_err(|e| ("", e.message)),
    };
    if !is_htmx(&h) {
        return match result {
            Ok(()) => redirect(&format!("{}/jobs/{id}?done=saved", st.base)),
            Err((_, m)) => fail(&st, &c, &h, StatusCode::BAD_REQUEST, &m).await,
        };
    }
    let job = match st
        .call::<Job>(method::JOBS_GET, IdParams { id: JobId(id) })
        .await
    {
        Ok(j) => j,
        Err(e) => return toast("error", &e.message),
    };
    match &result {
        Ok(()) => html(views::options_form(
            &st.base,
            &c.csrf,
            &job,
            &cur,
            &cur,
            Note::Saved,
        )),
        Err((f, m)) => html(views::options_form(
            &st.base,
            &c.csrf,
            &job,
            &orig,
            &cur,
            Note::Error(f, m),
        )),
    }
}
