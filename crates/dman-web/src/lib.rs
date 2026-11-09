//! dman's browser UI: server-rendered HTML (maud) with htmx and Server-Sent Events.
//!
//! The crate knows nothing about the daemon. The host implements [`Backend`]
//! (same semantics as the daemon's JSON-RPC dispatch plus an event feed) and
//! mounts [`router`] on its HTTP listener.
//!
//! * Pages: downloads, detail, search, add, settings, login.
//! * Live updates: `GET {base}/events` pushes changed table rows, the stats line and
//!   status messages, throttled to two batches per second per client.
//! * Auth: optional access token; session cookie or `Authorization: Bearer`; CSRF
//!   protection (Origin check plus a per-session token) on every state-changing request.
//! * No inline scripts or styles: the CSP is `default-src 'self'`.

mod assets;
mod auth;
mod fmt;
mod pages;
mod sse;
mod views;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use dman_proto::RpcError;
use dman_proto::method::Event;
use futures_util::future::BoxFuture;
use secrecy::SecretString;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// What the web UI needs from the daemon.
pub trait Backend: Send + Sync + 'static {
    /// Same semantics as the daemon's JSON-RPC dispatch.
    fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> BoxFuture<'static, Result<serde_json::Value, RpcError>>;

    /// Live events. A `Snapshot` first is not guaranteed; the UI fetches initial
    /// state with `jobs.list` and friends.
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event>;
}

/// Web UI settings.
pub struct WebConfig {
    /// Mount point behind a reverse proxy: `"/"` or `"/dman"`.
    pub base_path: String,
    /// Access token; when set, every page needs a login or a bearer header.
    pub token: Option<SecretString>,
    /// Shown in the header and tab title.
    pub title: String,
    /// The listener is reachable beyond loopback; warns when no token is set.
    pub exposed: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            base_path: "/".into(),
            token: None,
            title: "dman".into(),
            exposed: false,
        }
    }
}

/// Maximum size of one uploaded `.torrent`/`.metalink` request.
pub const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

pub(crate) struct AppState {
    pub backend: Arc<dyn Backend>,
    /// `""` for the root, otherwise `/prefix` without trailing slash.
    pub base: String,
    pub title: String,
    pub token: Option<SecretString>,
    pub exposed: bool,
    pub sessions: Mutex<HashMap<String, auth::Session>>,
    /// CSRF token used when no access token is configured (no sessions).
    pub open_csrf: String,
}

impl AppState {
    /// Typed backend call.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<T, RpcError> {
        let params = serde_json::to_value(params)
            .map_err(|e| RpcError::invalid(format!("encode params: {e}")))?;
        let v = self.backend.call(method, params).await?;
        serde_json::from_value(v)
            .map_err(|e| RpcError::new(dman_proto::ErrorCode::General, format!("bad reply: {e}")))
    }

    /// Call whose result is ignored.
    pub async fn call_unit(&self, method: &str, params: impl Serialize) -> Result<(), RpcError> {
        let params = serde_json::to_value(params)
            .map_err(|e| RpcError::invalid(format!("encode params: {e}")))?;
        self.backend.call(method, params).await.map(|_| ())
    }
}

fn normalize_base(p: &str) -> String {
    let t = p.trim().trim_matches('/');
    if t.is_empty() {
        String::new()
    } else {
        format!("/{t}")
    }
}

/// Builds the web UI router. Mount it as the root service of the HTTP listener; the
/// base path is applied to every route itself.
pub fn router(backend: Arc<dyn Backend>, cfg: WebConfig) -> Router {
    let base = normalize_base(&cfg.base_path);
    let st = Arc::new(AppState {
        backend,
        base: base.clone(),
        title: cfg.title,
        token: cfg.token,
        exposed: cfg.exposed,
        sessions: Mutex::new(HashMap::new()),
        open_csrf: auth::random_id(),
    });
    let r = |p: &str| format!("{base}{p}");
    let mut app = Router::new()
        .route(&r("/"), get(pages::downloads))
        .route(&r("/downloads/table"), get(pages::table))
        .route(&r("/jobs/{id}"), get(pages::detail))
        .route(&r("/jobs/{id}/live"), get(pages::detail_live))
        .route(
            &r("/jobs/{id}/remove"),
            get(pages::remove_form).post(pages::remove),
        )
        .route(&r("/jobs/{id}/pause"), post(pages::pause))
        .route(&r("/jobs/{id}/resume"), post(pages::resume))
        .route(&r("/jobs/{id}/purge"), post(pages::purge))
        .route(&r("/jobs/{id}/reorder"), post(pages::reorder))
        .route(&r("/jobs/{id}/rename"), post(pages::rename))
        .route(&r("/jobs/{id}/limits"), post(pages::job_limits))
        .route(&r("/queue/pause-all"), post(pages::pause_all))
        .route(&r("/queue/resume-all"), post(pages::resume_all))
        .route(&r("/add"), get(pages::add_form).post(pages::add))
        .route(
            &r("/add/upload"),
            post(pages::upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 64 * 1024)),
        )
        .route(&r("/search"), get(pages::search).post(pages::search_start))
        .route(&r("/search/add"), post(pages::search_add))
        .route(
            &r("/search/history/clear"),
            post(pages::search_history_clear),
        )
        .route(&r("/settings"), get(pages::settings))
        .route(&r("/settings/limits"), post(pages::settings_limits))
        .route(&r("/settings/provider"), post(pages::provider_set))
        .route(&r("/settings/provider/test"), post(pages::provider_test))
        .route(&r("/events"), get(sse::events))
        .route(&r("/login"), get(auth::login_form).post(auth::login))
        .route(&r("/logout"), post(auth::logout))
        .route(&r("/static/{*path}"), get(assets::serve))
        .fallback(pages::not_found);
    if !base.is_empty() {
        // `/dman` and `/dman/` both reach the downloads page.
        app = app.route(&base, get(pages::downloads));
    }
    app.layer(axum::middleware::from_fn_with_state(
        st.clone(),
        auth::guard,
    ))
    .with_state(st)
}

/// Total size in bytes of the embedded static assets.
pub fn embedded_asset_bytes() -> usize {
    assets::total_bytes()
}
