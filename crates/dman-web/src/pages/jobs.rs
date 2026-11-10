//! Downloads list and job actions (single and bulk).

use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use dman_proto::method::{self, IdParams, IdsParams, ListParams, Reorder, ReorderParams};
use dman_proto::{Job, JobId, RpcError};
use maud::html;

use super::{
    Pairs, St, fail, fail_rpc, flash, full_page, html, hx_target, is_htmx, query_pairs, redirect,
    toast, toast_markup, with_header,
};
use crate::AppState;
use crate::auth::Ctx;
use crate::views::{self, Nav, TableQuery};

pub(super) async fn list(st: &AppState) -> Result<Vec<Job>, RpcError> {
    st.call(method::JOBS_LIST, ListParams::default()).await
}

pub(super) fn ids_of(p: &Pairs) -> Vec<JobId> {
    p.all("ids")
        .flat_map(|v| v.split(','))
        .filter_map(|v| v.trim().parse().ok())
        .map(JobId)
        .collect()
}

/// Job ids of a `undo=1,2` query value (the undo toast after a redirect).
pub(super) fn undo_ids(p: &Pairs) -> Vec<JobId> {
    p.str("undo")
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .take(200)
        .map(JobId)
        .collect()
}

pub(super) fn id_list(ids: &[JobId]) -> String {
    ids.iter()
        .map(|i| i.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn quoted(name: &str) -> String {
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
    let undo = undo_ids(&p);
    if p.str("done") == "removed" && !undo.is_empty() {
        // "Remove from list" offers an undo for a few seconds.
        toasts = vec![views::toast_undo(
            &st.base,
            &ctx.csrf,
            "Removed (files kept on disk).",
            &undo,
        )];
    }
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
pub(super) async fn table_with(
    st: &AppState,
    ctx: &Ctx,
    p: &Pairs,
    toasts: Vec<maud::Markup>,
) -> Response {
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
pub(super) fn batch_failure(v: &serde_json::Value) -> Option<String> {
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
