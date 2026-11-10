//! Remove from list / delete files (one confirmation) and the undo of a removal.

use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use dman_proto::Job;
use dman_proto::JobId;
use dman_proto::method::{self, IdParams, IdsParams, RemoveParams};
use maud::html;

use super::jobs::{batch_failure, id_list, ids_of, list, quoted};
use super::{
    Pairs, St, fail, fail_rpc, full_page, html, is_htmx, query_pairs, redirect, toast,
    toast_markup, with_header,
};
use crate::AppState;
use crate::auth::Ctx;
use crate::views::{self, Nav, RemoveTarget};

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
    let form = views::remove_form(&st.base, &ctx.csrf, &t, delete, None);
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

/// The confirmation again, with the daemon's reason why it was refused.
async fn refused_form(
    st: &AppState,
    ctx: &Ctx,
    ids: &[JobId],
    back: &str,
    delete: bool,
    reason: &str,
) -> Option<maud::Markup> {
    let jobs = list(st).await.ok()?;
    let t = remove_target(back, &jobs, ids, &st.base);
    if t.jobs.is_empty() {
        return None;
    }
    Some(views::remove_form(
        &st.base,
        &ctx.csrf,
        &t,
        delete,
        Some(reason),
    ))
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
    let undo = if delete_files {
        String::new()
    } else {
        id_list(&ids)
    };
    let ids_for_undo = ids.clone();
    let params = RemoveParams {
        ids,
        delete_files,
        force: false,
    };
    let v: serde_json::Value = match st.call(method::JOBS_REMOVE, params).await {
        Ok(v) => v,
        Err(e) => {
            // A refusal is explained inside the open dialog, which stays open.
            if is_htmx(h)
                && let Some(form) =
                    refused_form(st, c, &ids_for_undo, back, delete_files, &e.message).await
            {
                return html(form);
            }
            return fail_rpc(st, c, h, &e).await;
        }
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
    // Only "Remove from list" can be undone; deleted files are gone for good.
    let to = if undo.is_empty() {
        format!("{}/?done={code}", st.base)
    } else {
        format!("{}/?done={code}&undo={undo}", st.base)
    };
    if !is_htmx(h) {
        return redirect(&to);
    }
    if back == "detail" {
        return with_header(html(html! {}), "hx-redirect", &to);
    }
    let t = if delete_files {
        views::toast("ok", &format!("Removed {name} and deleted its files."))
    } else {
        views::toast_undo(
            &st.base,
            &c.csrf,
            &format!("Removed {name} (files kept)."),
            &ids_for_undo,
        )
    };
    with_header(toast_markup(t), "hx-trigger", "dman-close, dman-clear-sel")
}

/// `POST /jobs/restore`: undo of "Remove from list" (`ids=1,2`).
pub async fn restore(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let ids = ids_of(&p);
    if ids.is_empty() {
        return fail(&st, &c, &h, StatusCode::BAD_REQUEST, "Nothing to restore.").await;
    }
    let name = if let [one] = ids.as_slice() {
        st.call::<Job>(method::JOBS_GET, IdParams { id: *one })
            .await
            .map(|j| quoted(views::display_name(&j)))
            .unwrap_or_else(|_| "the download".into())
    } else {
        format!("{} downloads", ids.len())
    };
    let v: serde_json::Value = match st.call(method::JOBS_RESTORE, IdsParams { ids }).await {
        Ok(v) => v,
        Err(e) => return fail_rpc(&st, &c, &h, &e).await,
    };
    if let Some(m) = batch_failure(&v) {
        return fail(
            &st,
            &c,
            &h,
            StatusCode::CONFLICT,
            &format!("Could not restore {name}: {m}"),
        )
        .await;
    }
    if !is_htmx(&h) {
        return redirect(&format!("{}/?done=restored", st.base));
    }
    // The Undo toast is replaced by this one; the list refreshes itself.
    with_header(
        html(views::toast("ok", &format!("Restored {name}."))),
        "hx-trigger",
        "dman-restored",
    )
}
