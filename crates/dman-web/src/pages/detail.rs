//! Detail page, rename and per-download options.

use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use dman_proto::method::{
    self, CleanupFile, CleanupParams, CleanupResult, IdParams, RenameParams, SetOptionsParams,
};
use dman_proto::{Job, JobDetail, JobId, JobOptions};
use maud::html;

use super::{
    Pairs, St, changed_rate, fail, fail_rpc, flash, full_page, html, is_htmx, query_pairs,
    redirect, toast, with_header,
};
use crate::AppState;
use crate::auth::Ctx;
use crate::views::{self, Nav, Note, OptValues};

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
            let cleanup = match d.cleanup_pending {
                Some(info) => Some((info, leftover_names(&st, &d).await)),
                None => None,
            };
            let name = views::display_name(&d.job).to_string();
            full_page(
                &st,
                &ctx,
                Nav::Downloads,
                &name,
                &format!("job={id}"),
                flash(&query_pairs(&raw)),
                views::detail_page(
                    &st.base,
                    &ctx.csrf,
                    &d,
                    cleanup.as_ref().map(|(i, f)| (*i, f.as_slice())),
                ),
            )
            .await
        }
        Err(e) => fail_rpc(&st, &ctx, &HeaderMap::new(), &e).await,
    }
}

/// Names of the leftover files the daemon flagged for the job (a dry run;
/// nothing is deleted). Empty when the daemon cannot say.
async fn leftover_names(st: &AppState, d: &JobDetail) -> Vec<CleanupFile> {
    let params = CleanupParams {
        id: d.job.id,
        dry_run: true,
    };
    st.call::<CleanupResult>(method::JOBS_CLEANUP_UNSELECTED, params)
        .await
        .map(|r| r.files)
        .unwrap_or_default()
}

/// `POST /jobs/{id}/cleanup`: deletes the leftovers listed on the detail page. The
/// button of that card is the one confirmation.
pub async fn cleanup(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let params = CleanupParams {
        id: JobId(id),
        dry_run: false,
    };
    match st
        .call::<CleanupResult>(method::JOBS_CLEANUP_UNSELECTED, params)
        .await
    {
        Ok(_) => redirect(&format!("{}/jobs/{id}?done=cleaned", st.base)),
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
