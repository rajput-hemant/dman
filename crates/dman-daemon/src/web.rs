//! The browser UI inside the daemon: `dman_web::Backend` over the API dispatch, its own
//! TCP listener, and runtime control (`web.status`, `web.start`, `web.stop`).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dman_config::{ConfigDoc, Paths, Secrets, Web};
use dman_proto::method::{self, Event, WebStartParams, WebStatus};
use dman_proto::rpc::protocol;
use dman_proto::{ErrorCode, RpcError};
use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::server::{self, ServerState};

/// How long open connections (SSE streams) get to finish when the web UI stops.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Decides how the access token is handled for `web`:
///
/// * loopback bind (without `require_login`): no login and no token at all; the web
///   layer still refuses other sites (same-origin, CSRF and `Host` checks);
/// * a token exists in secrets.toml: used;
/// * none, `allow_unauthenticated` (network bind): no authentication at all;
/// * none, loopback with `require_login`: a token is generated;
/// * none, any other bind: refused (`dman serve` generates one first).
pub fn resolve_token(web: &Web, secrets: &mut Secrets) -> Result<Option<SecretString>, RpcError> {
    if web.loopback_without_login() {
        return Ok(None);
    }
    if let Some(t) = secrets.get(&web.token_ref)
        && !t.expose_secret().is_empty()
    {
        return Ok(Some(t));
    }
    if web.allow_unauthenticated && !web.require_login {
        return Ok(None);
    }
    if !web.is_loopback() {
        return Err(RpcError::refused(format!(
            "refusing to serve the web UI on {} without an access token: add `{}` to secrets.toml \
             (`dman serve` generates one) or set web.allow_unauthenticated = true",
            web.bind, web.token_ref
        )));
    }
    secrets
        .ensure(&web.token_ref)
        .map(Some)
        .map_err(|e| RpcError::new(ErrorCode::General, format!("cannot store web token: {e}")))
}

/// Checks that `web` can be started (values valid, token policy satisfied) without
/// binding anything. Generates the token for loopback binds with `require_login`.
pub fn preflight(web: &Web, paths: &Paths) -> Result<(), RpcError> {
    web.validate().map_err(RpcError::invalid)?;
    if let Some(t) = &web.tls {
        crate::tls::acceptor(t).map_err(|e| RpcError::invalid(format!("web.tls: {e}")))?;
    }
    let mut secrets = load_secrets(paths)?;
    resolve_token(web, &mut secrets).map(drop)
}

fn load_secrets(paths: &Paths) -> Result<Secrets, RpcError> {
    Secrets::load(paths)
        .map_err(|e| RpcError::new(ErrorCode::General, format!("cannot read secrets: {e}")))
}

struct Running {
    url: String,
    exposed: bool,
    auth: bool,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

/// Owns the web listener; lives in [`ServerState`].
pub struct WebHost {
    paths: Paths,
    settings: Mutex<Web>,
    running: tokio::sync::Mutex<Option<Running>>,
}

impl WebHost {
    /// `settings` are the configured `[web]` values.
    pub fn new(paths: Paths, settings: Web) -> Self {
        Self {
            paths,
            settings: Mutex::new(settings),
            running: tokio::sync::Mutex::new(None),
        }
    }

    pub(crate) fn settings(&self) -> Web {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// `web.status`.
    pub async fn status(&self) -> WebStatus {
        match &*self.running.lock().await {
            Some(r) => WebStatus {
                enabled: true,
                url: Some(r.url.clone()),
                exposed: r.exposed,
                auth: r.auth,
            },
            None => {
                let s = self.settings();
                WebStatus {
                    enabled: false,
                    url: None,
                    exposed: !s.is_loopback(),
                    auth: false,
                }
            }
        }
    }

    /// Starts (or restarts, when already running) the listener with the configured values.
    pub async fn start_configured(&self, state: &Arc<ServerState>) -> Result<WebStatus, RpcError> {
        let mut web = self.settings();
        web.enabled = true;
        self.start(state, web, false).await
    }

    /// `web.start`: configured values overridden by `p`.
    pub async fn start_with(
        &self,
        state: &Arc<ServerState>,
        p: WebStartParams,
    ) -> Result<WebStatus, RpcError> {
        let mut web = self.settings();
        web.enabled = true;
        if let Some(b) = p.bind {
            web.bind = b;
        }
        if let Some(port) = p.port {
            web.port = port;
        }
        if let Some(b) = p.base_path {
            web.base_path = b;
        }
        self.start(state, web, p.persist).await
    }

    async fn start(
        &self,
        state: &Arc<ServerState>,
        web: Web,
        persist: bool,
    ) -> Result<WebStatus, RpcError> {
        web.validate().map_err(RpcError::invalid)?;
        let mut running = self.running.lock().await;
        if let Some(r) = running.take() {
            stop_running(r).await;
        }
        let token = resolve_token(&web, &mut load_secrets(&self.paths)?)?;
        let acceptor = web
            .tls
            .as_ref()
            .map(crate::tls::acceptor)
            .transpose()
            .map_err(|e| RpcError::invalid(format!("web.tls: {e}")))?;
        let ip = web
            .bind_ip()
            .ok_or_else(|| RpcError::invalid("invalid bind address"))?;
        let addr = SocketAddr::new(ip, web.port);
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
            RpcError::new(ErrorCode::General, format!("cannot listen on {addr}: {e}"))
        })?;
        let exposed = !web.is_loopback();
        let auth = token.is_some();
        let url = web.url();
        let base = web.base_path.clone();
        if exposed {
            tracing::warn!(
                %url,
                auth,
                "web UI is EXPOSED beyond loopback{}",
                if auth { "" } else { " and has NO access token" }
            );
        } else {
            tracing::info!(%url, auth, "web UI listening");
        }
        let backend = Arc::new(DaemonBackend {
            state: Arc::downgrade(state),
            events: state.events.clone(),
        });
        let router = dman_web::router(
            backend,
            dman_web::WebConfig {
                base_path: base,
                token,
                title: web.title.clone(),
                exposed,
                tls: acceptor.is_some(),
                bind: Some(ip),
                allowed_hosts: web.allowed_hosts.clone(),
            },
        );
        if exposed && acceptor.is_none() {
            tracing::warn!(
                %url,
                "web UI is served over plain http beyond loopback: the access token crosses the \
                 network in clear text; configure web.tls or put it behind a TLS reverse proxy"
            );
        }
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let r = crate::tls::serve(listener, router, acceptor, async move {
                let _ = stopped.wait_for(|v| *v).await;
            })
            .await;
            if let Err(e) = r {
                tracing::error!("web server failed: {e}");
            }
        });
        *running = Some(Running {
            url,
            exposed,
            auth,
            stop,
            task,
        });
        if persist {
            self.persist(&web)?;
        }
        if let Ok(mut s) = self.settings.lock() {
            *s = web.clone();
        }
        drop(running);
        Ok(self.status().await)
    }

    /// `web.stop`.
    pub async fn stop_with(&self, persist: bool) -> Result<WebStatus, RpcError> {
        self.stop().await;
        if persist {
            let mut web = self.settings();
            web.enabled = false;
            self.persist(&web)?;
            if let Ok(mut s) = self.settings.lock() {
                s.enabled = false;
            }
        }
        Ok(self.status().await)
    }

    /// Stops the listener (also used at daemon shutdown).
    pub async fn stop(&self) {
        if let Some(r) = self.running.lock().await.take() {
            stop_running(r).await;
        }
    }

    fn persist(&self, web: &Web) -> Result<(), RpcError> {
        let err = |e: dman_config::ConfigError| {
            RpcError::new(ErrorCode::General, format!("cannot write config: {e}"))
        };
        let mut doc = ConfigDoc::open(&self.paths).map_err(err)?;
        doc.set(&["web", "enabled"], web.enabled).map_err(err)?;
        if web.enabled {
            doc.set(&["web", "bind"], web.bind.as_str()).map_err(err)?;
            doc.set(&["web", "port"], i64::from(web.port))
                .map_err(err)?;
            doc.set(&["web", "base_path"], web.base_path.as_str())
                .map_err(err)?;
        }
        doc.save().map_err(err)
    }
}

async fn stop_running(r: Running) {
    let _ = r.stop.send(true);
    let mut task = r.task;
    if tokio::time::timeout(STOP_GRACE, &mut task).await.is_err() {
        task.abort();
    }
    tracing::info!(url = %r.url, "web UI stopped");
}

/// [`dman_web::Backend`] over the daemon's own dispatch. Calls are authenticated by the
/// web layer, so they skip `session.hello`; session control and `web.*` are not exposed.
struct DaemonBackend {
    state: Weak<ServerState>,
    events: broadcast::Sender<Event>,
}

impl dman_web::Backend for DaemonBackend {
    fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> BoxFuture<'static, Result<serde_json::Value, RpcError>> {
        let state = self.state.upgrade();
        let method = method.to_owned();
        Box::pin(async move {
            let Some(state) = state else {
                return Err(RpcError::new(ErrorCode::General, "daemon is shutting down"));
            };
            if method.starts_with("web.")
                || matches!(
                    method.as_str(),
                    method::HELLO | method::SUBSCRIBE | method::DAEMON_SHUTDOWN
                )
            {
                return Err(RpcError::new(
                    ErrorCode::General,
                    format!("method `{method}` not found"),
                ));
            }
            server::dispatch(&state, &method, params)
                .await
                .map_err(|e| {
                    if e.code == protocol::METHOD_NOT_FOUND {
                        RpcError::new(ErrorCode::General, e.message)
                    } else {
                        e.into()
                    }
                })
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
}
