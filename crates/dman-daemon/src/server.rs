//! The API server: JSON-RPC 2.0 over WebSocket at `GET /api/v1/ws`.
//!
//! Every connection must start with `session.hello`; the token is compared in
//! constant time. Requests after that are dispatched to the job manager
//! concurrently (the manager serialises them); `session.subscribe` sends a
//! snapshot followed by every broadcast event, and a fresh snapshot whenever
//! the connection fell behind.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response as HttpResponse;
use axum::routing::get;
use dman_proto::method::{self, Event, HelloParams, HelloResult};
use dman_proto::rpc::{Notification, Request as RpcRequest, Response, WireError, protocol};
use dman_proto::{API_VERSION, ErrorCode, RpcError};
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc, watch};

use crate::manager::{ManagerHandle, Request};

/// Outgoing frames buffered per connection before back-pressure applies.
const OUT_BUFFER: usize = 256;

/// Shared state of all connections.
pub struct ServerState {
    /// The job manager.
    pub manager: ManagerHandle,
    /// The API token.
    pub token: SecretString,
    /// Served profile name (reported by `session.hello`).
    pub profile: String,
    /// Event bus.
    pub events: broadcast::Sender<Event>,
    /// Becomes `true` when the daemon shuts down.
    pub shutdown: watch::Receiver<bool>,
    /// Asks the daemon to shut down (`daemon.shutdown`).
    pub shutdown_request: mpsc::Sender<()>,
    /// The engine owner (`engine.restart`).
    pub engine: crate::engine::EngineHandle,
    /// Magnet previews (`jobs.preview_magnet`).
    pub preview: Arc<crate::preview::Preview>,
    /// Search, providers and tracker list.
    pub search: Arc<crate::search::SearchService>,
    /// The video engine's tools (`video.probe`, re-detection on `engine.restart`).
    pub video: Arc<crate::video::VideoService>,
}

/// The axum router serving the API.
pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/api/v1/ws", get(ws_handler))
        .with_state(state)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<ServerState>>) -> HttpResponse {
    ws.on_upgrade(move |socket| session(socket, state))
}

fn wire(code: i64, message: impl Into<String>) -> WireError {
    WireError {
        code,
        message: message.into(),
        data: None,
    }
}

fn encode(r: &Response) -> String {
    serde_json::to_string(r).unwrap_or_else(|e| {
        format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"{e}"}}}}"#)
    })
}

fn notification(e: &Event) -> Option<String> {
    let n = Notification::new(method::EVENT, e).ok()?;
    serde_json::to_string(&n).ok()
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| RpcError::invalid(format!("invalid params: {e}")))
}

/// Maps a method call to a manager request.
fn to_request(method: &str, params: Value) -> Result<Request, WireError> {
    let p = |r: Result<Request, RpcError>| r.map_err(WireError::from);
    Ok(match method {
        method::JOBS_ADD => p(parse(params).map(Request::Add))?,
        method::JOBS_LIST => p(parse(params).map(Request::List))?,
        method::JOBS_GET => p(parse::<method::IdParams>(params).map(|x| Request::Get(x.id)))?,
        method::JOBS_DETAIL => p(parse::<method::IdParams>(params).map(|x| Request::Detail(x.id)))?,
        method::JOBS_PAUSE => p(parse::<method::IdsParams>(params).map(|x| Request::Pause(x.ids)))?,
        method::JOBS_RESUME => {
            p(parse::<method::IdsParams>(params).map(|x| Request::Resume(x.ids)))?
        }
        method::JOBS_REMOVE => p(parse(params).map(Request::Remove))?,
        method::JOBS_PURGE => p(parse::<method::IdsParams>(params).map(|x| Request::Purge(x.ids)))?,
        method::JOBS_REORDER => p(parse(params).map(Request::Reorder))?,
        method::JOBS_SET_OPTIONS => p(parse(params).map(Request::SetOptions))?,
        method::JOBS_RENAME => p(parse(params).map(Request::Rename))?,
        method::QUEUE_PAUSE_ALL => Request::PauseAll,
        method::QUEUE_RESUME_ALL => Request::ResumeAll,
        method::LIMITS_GET => Request::LimitsGet,
        method::LIMITS_SET => p(parse(params).map(Request::LimitsSet))?,
        method::SCHEDULE_GET => Request::ScheduleGet,
        method::STATS_GET => Request::Stats,
        method::STATS_HISTORY => Request::StatsHistory,
        method::ENGINE_STATUS => Request::EngineStatus,
        method::FS_LIST => p(parse(params).map(Request::FsList))?,
        other => {
            return Err(wire(
                protocol::METHOD_NOT_FOUND,
                format!("method `{other}` not found"),
            ));
        }
    })
}

fn hello(state: &ServerState, params: Value) -> Result<HelloResult, RpcError> {
    let p: HelloParams = parse(params)?;
    let ok: bool = p
        .token
        .as_bytes()
        .ct_eq(state.token.expose_secret().as_bytes())
        .into();
    if !ok {
        tracing::warn!(client = %p.client, "rejected connection with a wrong token");
        return Err(RpcError::new(ErrorCode::Unauthorized, "invalid token"));
    }
    if p.api_version != API_VERSION {
        let mut e = RpcError::invalid(format!(
            "API version mismatch: daemon speaks {API_VERSION}, client {}",
            p.api_version
        ));
        e.data = Some(json!({ "api_version": API_VERSION }));
        return Err(e);
    }
    tracing::debug!(client = %p.client, "client authenticated");
    Ok(HelloResult {
        api_version: API_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        profile: state.profile.clone(),
    })
}

async fn snapshot(manager: &ManagerHandle) -> Option<String> {
    let v = manager.call(Request::Snapshot).await.ok()?;
    let e: Event = serde_json::from_value(v).ok()?;
    notification(&e)
}

/// Forwards broadcast events to one subscriber; on lag sends a new snapshot.
async fn forward(
    mut rx: broadcast::Receiver<Event>,
    out: mpsc::Sender<String>,
    manager: ManagerHandle,
) {
    loop {
        let frame = match rx.recv().await {
            Ok(e) => notification(&e),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!(skipped = n, "subscriber lagged; sending a fresh snapshot");
                snapshot(&manager).await
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if let Some(f) = frame
            && out.send(f).await.is_err()
        {
            return;
        }
    }
}

async fn session(socket: WebSocket, state: Arc<ServerState>) {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(OUT_BUFFER);
    let writer = tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            if sink.send(Message::Text(s.into())).await.is_err() {
                return;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });
    let reply = |r: Response| {
        let out = out_tx.clone();
        async move {
            let _ = out.send(encode(&r)).await;
        }
    };

    let mut shutdown = state.shutdown.clone();
    let mut authed = false;
    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // Magnet previews are cancelled (and cleaned up) when the connection ends.
    let mut previews: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    loop {
        let msg = tokio::select! {
            m = stream.next() => m,
            _ = shutdown.wait_for(|v| *v) => break,
        };
        let text = match msg {
            Some(Ok(Message::Text(t))) => t,
            Some(Ok(Message::Binary(_))) => {
                reply(Response::err(
                    Value::Null,
                    wire(protocol::INVALID_REQUEST, "binary frames are not supported"),
                ))
                .await;
                continue;
            }
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(_)) => continue,
        };
        let req: RpcRequest = match serde_json::from_str(text.as_str()) {
            Ok(r) => r,
            Err(e) => {
                reply(Response::err(
                    Value::Null,
                    wire(protocol::PARSE_ERROR, format!("parse error: {e}")),
                ))
                .await;
                continue;
            }
        };
        // Notifications (no id) get no answer and have no effect.
        let Some(id) = req.id.clone() else { continue };
        if req.jsonrpc != "2.0" {
            reply(Response::err(
                id,
                wire(protocol::INVALID_REQUEST, "jsonrpc must be \"2.0\""),
            ))
            .await;
            continue;
        }
        if req.method == method::HELLO {
            match hello(&state, req.params) {
                Ok(h) => {
                    authed = true;
                    reply(Response::ok(id, json!(h))).await;
                }
                Err(e) => {
                    let close = e.code == ErrorCode::Unauthorized;
                    reply(Response::err(id, e)).await;
                    if close {
                        break;
                    }
                }
            }
            continue;
        }
        if !authed {
            reply(Response::err(
                id,
                RpcError::new(
                    ErrorCode::Unauthorized,
                    "authenticate with session.hello first",
                ),
            ))
            .await;
            continue;
        }
        match req.method.as_str() {
            method::SUBSCRIBE => {
                if forwarder.is_none() {
                    // Subscribe before taking the snapshot so nothing is missed.
                    let rx = state.events.subscribe();
                    reply(Response::ok(id, Value::Null)).await;
                    if let Some(s) = snapshot(&state.manager).await {
                        let _ = out_tx.send(s).await;
                    }
                    forwarder = Some(tokio::spawn(forward(
                        rx,
                        out_tx.clone(),
                        state.manager.clone(),
                    )));
                } else {
                    reply(Response::ok(id, Value::Null)).await;
                }
            }
            method::DAEMON_SHUTDOWN => {
                reply(Response::ok(id, Value::Null)).await;
                tracing::info!("shutdown requested over the API");
                let _ = state.shutdown_request.try_send(());
            }
            method::JOBS_PREVIEW_MAGNET => {
                let params = match parse::<method::PreviewMagnetParams>(req.params) {
                    Ok(p) => p,
                    Err(e) => {
                        reply(Response::err(id, e)).await;
                        continue;
                    }
                };
                let preview = state.preview.clone();
                let out = out_tx.clone();
                previews.retain(|t| !t.is_finished());
                previews.push(tokio::spawn(async move {
                    let resp = match preview.run(params).await {
                        Ok(v) => Response::ok(id, json!(v)),
                        Err(e) => Response::err(id, e),
                    };
                    let _ = out.send(encode(&resp)).await;
                }));
            }
            method::VIDEO_PROBE => {
                let params = match parse::<dman_proto::VideoProbeParams>(req.params) {
                    Ok(p) => p,
                    Err(e) => {
                        reply(Response::err(id, e)).await;
                        continue;
                    }
                };
                // Runs off the manager; aborted (yt-dlp killed) on disconnect.
                let video = state.video.clone();
                let out = out_tx.clone();
                previews.retain(|t| !t.is_finished());
                previews.push(tokio::spawn(async move {
                    let resp = match video.probe(params).await {
                        Ok(v) => Response::ok(id, json!(v)),
                        Err(e) => Response::err(id, e),
                    };
                    let _ = out.send(encode(&resp)).await;
                }));
            }
            method::ENGINE_RESTART => {
                let engine = state.engine.clone();
                let video = state.video.clone();
                let out = out_tx.clone();
                tasks.retain(|t| !t.is_finished());
                tasks.push(tokio::spawn(async move {
                    // yt-dlp/ffmpeg are looked up again first (the manager
                    // publishes the new status), then aria2 is restarted.
                    let tools = video.redetect().await;
                    let resp = match engine.restart().await {
                        Ok(status) => {
                            Response::ok(id, json!([status, crate::video::engine_status(&tools)]))
                        }
                        Err(e) => Response::err(id, e),
                    };
                    let _ = out.send(encode(&resp)).await;
                }));
            }
            m if crate::search::handles(m) => {
                let search = state.search.clone();
                let out = out_tx.clone();
                let method = m.to_owned();
                tasks.retain(|t| !t.is_finished());
                tasks.push(tokio::spawn(async move {
                    let resp = match search.call(&method, req.params).await {
                        Ok(v) => Response::ok(id, v),
                        Err(e) => Response::err(id, e),
                    };
                    let _ = out.send(encode(&resp)).await;
                }));
            }
            m => match to_request(m, req.params) {
                Err(e) => reply(Response::err(id, e)).await,
                Ok(r) => {
                    let manager = state.manager.clone();
                    let out = out_tx.clone();
                    tasks.retain(|t| !t.is_finished());
                    tasks.push(tokio::spawn(async move {
                        let resp = match manager.call(r).await {
                            Ok(v) => Response::ok(id, v),
                            Err(e) => Response::err(id, e),
                        };
                        let _ = out.send(encode(&resp)).await;
                    }));
                }
            },
        }
    }
    if let Some(f) = forwarder {
        f.abort();
    }
    for t in &previews {
        t.abort();
    }
    // Let in-flight requests answer briefly, then close.
    for t in tasks {
        let _ = tokio::time::timeout(Duration::from_secs(2), t).await;
    }
    drop(out_tx);
    let mut writer = writer;
    if tokio::time::timeout(Duration::from_secs(2), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_mapping() {
        assert!(matches!(
            to_request(method::JOBS_LIST, Value::Null),
            Ok(Request::List(_))
        ));
        assert!(matches!(
            to_request(method::STATS_GET, Value::Null),
            Ok(Request::Stats)
        ));
        let e = to_request("nope.nope", Value::Null).unwrap_err();
        assert_eq!(e.code, protocol::METHOD_NOT_FOUND);
        let e = to_request(method::JOBS_GET, json!({"id": "x"})).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput.rpc_code());
        let e = to_request(method::JOBS_PAUSE, Value::Null).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput.rpc_code());
    }
}
