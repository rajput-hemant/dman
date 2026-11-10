//! Settings: global limits, queue size, providers (enable, test), status displays.

use axum::Extension;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use dman_proto::method::{self, EngineStatus, LimitsUpdate, ReloadResult, ScheduleStatus};
use dman_proto::search::{
    self, ProviderInfo, ProviderNameParams, ProviderSetParams, ProviderTestReport,
};
use dman_proto::{Limits, VideoPreferences, VideoPreferencesUpdate};

use super::{
    Pairs, St, changed_rate, fail, fail_rpc, flash, full_page, html, is_htmx, query_pairs,
    redirect, toast,
};
use crate::AppState;
use crate::auth::Ctx;
use crate::views::{self, LimitValues, Nav, SettingsData, WebInfo};

fn web_info(st: &AppState, h: &HeaderMap, csrf: &str) -> WebInfo {
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let https = st.tls
        || h.get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    WebInfo {
        url: format!(
            "{}://{host}{}/",
            if https { "https" } else { "http" },
            st.base
        ),
        auth: st.token.is_some(),
        exposed: st.exposed,
        tls: https,
        csrf: csrf.to_string(),
    }
}

async fn render(
    st: &AppState,
    ctx: &Ctx,
    h: &HeaderMap,
    values: Option<LimitValues>,
    error: Option<(&str, String)>,
    toasts: Vec<maud::Markup>,
    reload: Option<&Result<ReloadResult, String>>,
) -> Response {
    let limits: Option<Limits> = st.call(method::LIMITS_GET, ()).await.ok();
    let providers: Vec<ProviderInfo> = st
        .call(search::PROVIDERS_LIST, ())
        .await
        .unwrap_or_default();
    let schedule: Option<ScheduleStatus> = st.call(method::SCHEDULE_GET, ()).await.ok();
    let engines: Vec<EngineStatus> = st.call(method::ENGINE_STATUS, ()).await.unwrap_or_default();
    // The Video section needs the engine and readable preferences.
    let video: Option<VideoPreferences> = if engines
        .iter()
        .any(|e| e.engine == dman_proto::Engine::YtDlp && e.available)
    {
        st.call(method::VIDEO_PREFERENCES_GET, ()).await.ok()
    } else {
        None
    };
    let data = SettingsData {
        limits: limits.as_ref(),
        values: values.unwrap_or_else(|| LimitValues::from_limits(limits.as_ref())),
        error,
        providers: &providers,
        schedule: schedule.as_ref(),
        engines: &engines,
        video: video.as_ref(),
        web: web_info(st, h, &ctx.csrf),
        reload,
    };
    let content = views::settings_page(&st.base, &ctx.csrf, &data);
    full_page(st, ctx, Nav::Settings, "Settings", "", toasts, content).await
}

pub async fn settings(
    State(st): St,
    Extension(ctx): Extension<Ctx>,
    h: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Response {
    render(&st, &ctx, &h, None, None, flash(&query_pairs(&raw)), None).await
}

pub async fn settings_limits(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let mut u = LimitsUpdate::default();
    let mut error: Option<(&str, String)> = None;
    for (field, slot) in [("download", &mut u.download), ("upload", &mut u.upload)] {
        match changed_rate(&p, field) {
            Ok(v) => *slot = v,
            Err(m) => error = error.or(Some((field, m))),
        }
    }
    if let Some(v) = p.get("max_concurrent").map(str::trim)
        && v != p.str("o_max_concurrent")
    {
        match v.parse::<u32>() {
            Ok(n) if (1..=100).contains(&n) => u.max_concurrent = Some(n),
            _ => {
                error = error.or(Some((
                    "max_concurrent",
                    "Enter a whole number from 1 to 100.".into(),
                )))
            }
        }
    }
    if error.is_none()
        && u != LimitsUpdate::default()
        && let Err(e) = st.call_unit(method::LIMITS_SET, u).await
    {
        if is_htmx(&h) {
            return fail_rpc(&st, &c, &h, &e).await;
        }
        error = Some(("", e.message));
    }
    match error {
        None => redirect(&format!("{}/settings?done=limits", st.base)),
        Some(err) => {
            let limits: Option<Limits> = st.call(method::LIMITS_GET, ()).await.ok();
            let mut values = LimitValues::from_limits(limits.as_ref());
            for (k, slot) in [
                ("download", &mut values.download),
                ("upload", &mut values.upload),
                ("max_concurrent", &mut values.max_concurrent),
            ] {
                if let Some(v) = p.get(k) {
                    *slot = v.trim().to_string();
                }
            }
            let mut r = render(&st, &c, &h, Some(values), Some(err), Vec::new(), None).await;
            *r.status_mut() = StatusCode::BAD_REQUEST;
            r
        }
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
        Ok(info) if is_htmx(&h) => {
            let idx = p.text("idx").and_then(|i| i.parse().ok()).unwrap_or(0);
            html(views::provider_row(&st.base, &c.csrf, idx, &info))
        }
        Ok(_) if p.str("back") == "search" => redirect(&format!("{}/search", st.base)),
        Ok(_) => redirect(&format!("{}/settings?done=provider#providers", st.base)),
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
    let r = st
        .call::<ProviderTestReport>(
            search::PROVIDERS_TEST,
            ProviderNameParams {
                name: name.to_string(),
            },
        )
        .await;
    let (level, text) = match r {
        Ok(r) if r.ok => (
            "ok",
            format!(
                "{} works: {} results in {} ms.",
                r.name, r.results, r.elapsed_ms
            ),
        ),
        Ok(r) => (
            "error",
            format!(
                "{} failed: {}. The site may be down, blocked on your network, or changed its layout.",
                r.name,
                r.error.unwrap_or_else(|| "no results".into())
            ),
        ),
        Err(e) => return fail_rpc(&st, &c, &h, &e).await,
    };
    if is_htmx(&h) {
        return toast(level, &text);
    }
    render(
        &st,
        &c,
        &h,
        None,
        None,
        vec![views::toast(level, &text)],
        None,
    )
    .await
}

/// `POST /settings/video`: the quality picker settings (`video.preferences.set`).
/// The checkbox is absent when unticked, so `ask_quality` is always sent.
pub async fn settings_video(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    p: Pairs,
) -> Response {
    let update = VideoPreferencesUpdate {
        ask_quality: Some(p.flag("ask_quality")),
        default_quality: p.text("default_quality").map(str::to_owned),
        default_audio: p.text("default_audio").map(str::to_owned),
        // An empty string clears the container default.
        default_container: p.get("default_container").map(|v| v.trim().to_owned()),
    };
    match st
        .call::<VideoPreferences>(method::VIDEO_PREFERENCES_SET, update)
        .await
    {
        Ok(_) if is_htmx(&h) => toast("ok", "Video settings saved."),
        Ok(_) => redirect(&format!("{}/settings?done=video", st.base)),
        Err(e) => fail_rpc(&st, &c, &h, &e).await,
    }
}

/// `POST /settings/reload`: re-reads the config file (`config.reload`) and says what
/// was applied and what needs a restart.
pub async fn settings_reload(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
) -> Response {
    let outcome: Result<ReloadResult, String> = st
        .call(method::CONFIG_RELOAD, ())
        .await
        .map_err(|e| e.message);
    if is_htmx(&h) {
        return html(views::reload_result(&outcome));
    }
    let mut r = render(&st, &c, &h, None, None, Vec::new(), Some(&outcome)).await;
    if outcome.is_err() {
        *r.status_mut() = StatusCode::BAD_GATEWAY;
    }
    r
}
