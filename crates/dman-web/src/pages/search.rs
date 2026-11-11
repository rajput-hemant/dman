//! Search: start, results (live over SSE), one-click download, resolve for options.

use axum::Extension;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use dman_proto::method;
use dman_proto::search::{
    self, ProviderInfo, ResolvedMagnet, SearchHistoryEntry, SearchResolveParams, SearchSnapshot,
    SearchStartParams, SearchStarted,
};
use dman_proto::{AddOutcome, AddParams, AddSource, JobOptions};

use super::{Pairs, St, fail, fail_rpc, full_page, is_htmx, query_pairs, redirect, toast_markup};
use crate::auth::Ctx;
use crate::views::{self, Nav, SearchView};

const SORTS: [&str; 4] = ["size", "seeders", "date", "title"];

pub async fn search(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let p = query_pairs(&raw);
    let id: Option<u64> = p.text("id").and_then(|v| v.parse().ok());
    let sort = SORTS
        .into_iter()
        .find(|s| *s == p.str("sort"))
        .unwrap_or("");
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
        .map_or_else(|| p.str("q").to_string(), |s| s.query.clone());
    let view = SearchView {
        search_id: id,
        finished: snap.as_ref().is_some_and(|s| s.finished),
        providers: snap.as_ref().map_or(&[], |s| s.providers.as_slice()),
        results: snap.as_ref().map_or(&[], |s| s.results.as_slice()),
        sort,
    };
    let content = views::search_page(&st.base, &ctx.csrf, &query, &providers, &history, &view);
    let mut sse = match id {
        Some(i) if sort.is_empty() => format!("search={i}"),
        Some(i) => format!("search={i}&sort={sort}"),
        None => String::new(),
    };
    if view.finished {
        sse.push_str("&done=1");
    }
    let title = if query.is_empty() {
        "Search".to_string()
    } else {
        format!("{query} \u{b7} Search")
    };
    full_page(&st, &ctx, Nav::Search, &title, &sse, Vec::new(), content).await
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
            "Enter something to search for.",
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
        Ok(s) => redirect(&format!("{}/search?id={}", st.base, s.search_id)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

/// The link of a result: a magnet or torrent URL as is; a detail page is resolved.
async fn result_uri(st: &crate::AppState, p: &Pairs) -> Result<String, String> {
    if let Some(m) = p.text("magnet") {
        return Ok(m.to_string());
    }
    if let Some(t) = p.text("torrent_url") {
        return Ok(t.to_string());
    }
    let Some(d) = p.text("detail_url") else {
        return Err("This result has no download link.".into());
    };
    let params = SearchResolveParams {
        detail_url: Some(d.to_string()),
        provider: p.text("provider").map(str::to_string),
        ..SearchResolveParams::default()
    };
    st.call::<ResolvedMagnet>(search::SEARCH_RESOLVE, params)
        .await
        .map(|r| r.magnet)
        .map_err(|e| e.message)
}

/// `POST /search/add`: starts the result right away.
pub async fn search_add(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let title = p.text("title").unwrap_or("the result").to_string();
    let failed =
        |m: String| format!("Could not start \u{201c}{title}\u{201d}: {m}. Try another result.");
    let uri = match result_uri(&st, &p).await {
        Ok(u) => u,
        Err(m) => return fail(&st, &c, &h, StatusCode::BAD_GATEWAY, &failed(m)).await,
    };
    let params = AddParams {
        sources: vec![AddSource::Uris { uris: vec![uri] }],
        options: JobOptions::default(),
        paused: false,
        force: false,
    };
    let v: serde_json::Value = match st.call(method::JOBS_ADD, params).await {
        Ok(v) => v,
        Err(e) => return fail(&st, &c, &h, StatusCode::BAD_GATEWAY, &failed(e.message)).await,
    };
    let outcome = serde_json::from_value::<Vec<AddOutcome>>(v)
        .ok()
        .and_then(|o| o.into_iter().next());
    let id = match outcome {
        Some(AddOutcome::Added { id, .. }) => id,
        Some(AddOutcome::Failed { error }) => {
            return fail(&st, &c, &h, StatusCode::BAD_REQUEST, &failed(error.message)).await;
        }
        None => {
            return fail(
                &st,
                &c,
                &h,
                StatusCode::BAD_GATEWAY,
                &failed("no answer".into()),
            )
            .await;
        }
    };
    if !is_htmx(&h) {
        return redirect(&format!("{}/?added={}", st.base, id.0));
    }
    let href = format!("{}/jobs/{}", st.base, id.0);
    toast_markup(views::toast_link(
        "ok",
        &format!("Started \u{201c}{title}\u{201d}."),
        Some((&href, "View")),
    ))
}

/// `GET /search/resolve?detail_url=..&provider=..`: finds the magnet of a result
/// and opens the add page with it.
pub async fn search_resolve(
    State(st): St,
    Extension(c): Extension<Ctx>,
    RawQuery(raw): RawQuery,
) -> Response {
    let p = query_pairs(&raw);
    match result_uri(&st, &p).await {
        Ok(uri) => {
            let qs = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("uri", &uri)
                .finish();
            redirect(&format!("{}/add?{qs}", st.base))
        }
        Err(m) => {
            fail(
                &st,
                &c,
                &HeaderMap::new(),
                StatusCode::BAD_GATEWAY,
                &format!("Could not get the download link: {m}"),
            )
            .await
        }
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
