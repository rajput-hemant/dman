//! A single authenticated WebSocket connection.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use dman_proto::method::{
    self, EngineStatus, Event, HelloParams, HelloResult, IdParams, IdsParams, LimitsUpdate,
    ListParams, RemoveParams, Reorder, ReorderParams, SetOptionsParams, SubscribeParams,
};
use dman_proto::rpc::{Request, ServerMessage};
use dman_proto::{
    API_VERSION, AddOutcome, AddParams, ErrorCode, GlobalStats, Job, JobId, JobOptions, Limits,
};
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::error::ClientError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_CAPACITY: usize = 256;

type Reply = oneshot::Sender<Result<Value, ClientError>>;

enum Command {
    Call {
        id: u64,
        payload: String,
        reply: Reply,
    },
    Cancel(u64),
}

/// The socket owner's handle: request correlation over one connection.
struct Link {
    cmd_tx: mpsc::UnboundedSender<Command>,
    next_id: AtomicU64,
    /// `true` once the socket task has ended.
    closed: watch::Receiver<bool>,
}

impl Link {
    async fn call_value(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        if *self.closed.borrow() {
            return Err(ClientError::Disconnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request =
            Request::new(id, method, params).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let payload =
            serde_json::to_string(&request).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Call { id, payload, reply })
            .map_err(|_| ClientError::Disconnected)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(ClientError::Disconnected),
            Err(_) => {
                let _ = self.cmd_tx.send(Command::Cancel(id));
                Err(ClientError::Timeout)
            }
        }
    }
}

struct Inner {
    link: Link,
    hello: HelloResult,
    events: broadcast::Sender<Event>,
    subscribed: AtomicBool,
}

/// Cheaply clonable handle to one authenticated daemon connection.
///
/// A background task owns the socket. When it closes, pending and later calls
/// fail with [`ClientError::Disconnected`]; see [`Client::closed`]. Use
/// [`crate::ReconnectingClient`] for automatic reconnection.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    timeout: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("daemon_version", &self.inner.hello.daemon_version)
            .field("connected", &self.is_connected())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connect to `url` (e.g. `ws://127.0.0.1:6801/api/v1/ws`), authenticate with
    /// `token` via `session.hello` and check the API version.
    ///
    /// # Errors
    /// [`ClientError::Connect`], [`ClientError::Unauthorized`],
    /// [`ClientError::IncompatibleVersion`] or other transport errors.
    pub async fn connect(
        url: &Url,
        token: SecretString,
        client_name: &str,
    ) -> Result<Self, ClientError> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self::connect_with_events(url, &token, client_name, events).await
    }

    /// Connect, perform one call and close the connection (for the CLI).
    ///
    /// # Errors
    /// Any connect error, or the error of the call itself.
    pub async fn call_once<P: Serialize, R: DeserializeOwned>(
        url: &Url,
        token: SecretString,
        client_name: &str,
        method: &str,
        params: P,
    ) -> Result<R, ClientError> {
        Self::connect(url, token, client_name)
            .await?
            .call(method, params)
            .await
    }

    /// Like [`Client::connect`] but forwards events into an existing channel.
    pub(crate) async fn connect_with_events(
        url: &Url,
        token: &SecretString,
        client_name: &str,
        events: broadcast::Sender<Event>,
    ) -> Result<Self, ClientError> {
        let connecting = tokio_tungstenite::connect_async(url.as_str());
        let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| ClientError::Connect("connection attempt timed out".into()))?
            .map_err(|e| ClientError::Connect(e.to_string()))?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed_rx) = watch::channel(false);
        tokio::spawn(run(ws, cmd_rx, events.clone(), closed_tx));
        let link = Link {
            cmd_tx,
            next_id: AtomicU64::new(1),
            closed: closed_rx,
        };

        let params = serde_json::to_value(HelloParams {
            token: token.expose_secret().to_owned(),
            api_version: API_VERSION,
            client: client_name.to_owned(),
        })
        .map_err(|e| ClientError::Protocol(e.to_string()))?;
        let value = link
            .call_value(method::HELLO, params, DEFAULT_TIMEOUT)
            .await
            .map_err(|e| match e {
                ClientError::Rpc(r) if r.code == ErrorCode::Unauthorized => {
                    ClientError::Unauthorized
                }
                other => other,
            })?;
        let hello: HelloResult = serde_json::from_value(value)
            .map_err(|e| ClientError::Protocol(format!("malformed hello result: {e}")))?;
        if hello.api_version != API_VERSION {
            return Err(ClientError::IncompatibleVersion {
                daemon: hello.api_version,
                client: API_VERSION,
            });
        }
        Ok(Self {
            inner: Arc::new(Inner {
                link,
                hello,
                events,
                subscribed: AtomicBool::new(false),
            }),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Return a handle with a different per-call timeout (default 15 s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Daemon version string from the hello reply.
    pub fn daemon_version(&self) -> &str {
        &self.inner.hello.daemon_version
    }

    /// Profile name from the hello reply.
    pub fn profile(&self) -> &str {
        &self.inner.hello.profile
    }

    /// Whether the connection is still open.
    pub fn is_connected(&self) -> bool {
        !*self.inner.link.closed.borrow()
    }

    /// Resolves once the connection has closed.
    pub async fn closed(&self) {
        let mut rx = self.inner.link.closed.clone();
        // An Err means the task is gone entirely: also closed.
        let _ = rx.wait_for(|closed| *closed).await;
    }

    /// Call any daemon method. `params` is serialised as the JSON-RPC params.
    ///
    /// # Errors
    /// Transport errors, [`ClientError::Timeout`], or [`ClientError::Rpc`] when
    /// the daemon answers with an error.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, ClientError> {
        let params =
            serde_json::to_value(params).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let value = self
            .inner
            .link
            .call_value(method, params, self.timeout)
            .await?;
        serde_json::from_value(value)
            .map_err(|e| ClientError::Protocol(format!("{method}: malformed result: {e}")))
    }

    /// Start receiving events (sends `session.subscribe` once per connection) and
    /// return a receiver. The daemon's first event is a `Snapshot`; the receiver
    /// is created before subscribing so it is never missed. Further calls just
    /// return additional receivers (without a new snapshot).
    ///
    /// # Errors
    /// Any error of the `session.subscribe` call.
    pub async fn subscribe(&self) -> Result<broadcast::Receiver<Event>, ClientError> {
        let rx = self.inner.events.subscribe();
        if !self.inner.subscribed.swap(true, Ordering::SeqCst)
            && let Err(e) = self
                .call::<_, Value>(method::SUBSCRIBE, SubscribeParams {})
                .await
        {
            self.inner.subscribed.store(false, Ordering::SeqCst);
            return Err(e);
        }
        Ok(rx)
    }
}

/// Generates the typed API on a type that has
/// `async fn call<P, R>(&self, &str, P) -> Result<R, ClientError>`.
macro_rules! typed_api {
    () => {
        /// `jobs.add`: one outcome per source, in order.
        pub async fn add(&self, params: AddParams) -> Result<Vec<AddOutcome>, ClientError> {
            self.call(method::JOBS_ADD, params).await
        }

        /// `jobs.list`: jobs in the given states (all when empty).
        pub async fn list(&self, params: ListParams) -> Result<Vec<Job>, ClientError> {
            self.call(method::JOBS_LIST, params).await
        }

        /// `jobs.get`
        pub async fn get(&self, id: JobId) -> Result<Job, ClientError> {
            self.call(method::JOBS_GET, IdParams { id }).await
        }

        /// `jobs.detail`: files, peers, trackers, pieces and speed history of one job.
        pub async fn detail(&self, id: JobId) -> Result<dman_proto::JobDetail, ClientError> {
            self.call(method::JOBS_DETAIL, IdParams { id }).await
        }

        /// `jobs.pause`
        pub async fn pause(&self, ids: &[JobId]) -> Result<method::BatchResult, ClientError> {
            self.call(method::JOBS_PAUSE, IdsParams { ids: ids.to_vec() })
                .await
        }

        /// `jobs.resume`
        pub async fn resume(&self, ids: &[JobId]) -> Result<method::BatchResult, ClientError> {
            self.call(method::JOBS_RESUME, IdsParams { ids: ids.to_vec() })
                .await
        }

        /// `jobs.remove` (set `delete_files` only after explicit user confirmation).
        pub async fn remove(
            &self,
            params: RemoveParams,
        ) -> Result<method::BatchResult, ClientError> {
            self.call(method::JOBS_REMOVE, params).await
        }

        /// `jobs.purge`: drop finished jobs from the history.
        pub async fn purge(&self, ids: &[JobId]) -> Result<method::BatchResult, ClientError> {
            self.call(method::JOBS_PURGE, IdsParams { ids: ids.to_vec() })
                .await
        }

        /// `jobs.reorder`; the result payload is ignored.
        pub async fn reorder(&self, id: JobId, to: Reorder) -> Result<(), ClientError> {
            self.call::<_, Value>(method::JOBS_REORDER, ReorderParams { id, to })
                .await
                .map(drop)
        }

        /// `jobs.set_options`; the result payload is ignored.
        pub async fn set_options(&self, id: JobId, options: JobOptions) -> Result<(), ClientError> {
            self.call::<_, Value>(method::JOBS_SET_OPTIONS, SetOptionsParams { id, options })
                .await
                .map(drop)
        }

        /// `queue.pause_all`
        pub async fn pause_all(&self) -> Result<(), ClientError> {
            self.call::<_, Value>(method::QUEUE_PAUSE_ALL, ())
                .await
                .map(drop)
        }

        /// `queue.resume_all`
        pub async fn resume_all(&self) -> Result<(), ClientError> {
            self.call::<_, Value>(method::QUEUE_RESUME_ALL, ())
                .await
                .map(drop)
        }

        /// `limits.get`
        pub async fn limits_get(&self) -> Result<Limits, ClientError> {
            self.call(method::LIMITS_GET, ()).await
        }

        /// `limits.set`: returns the limits now in effect.
        pub async fn limits_set(&self, update: LimitsUpdate) -> Result<Limits, ClientError> {
            self.call(method::LIMITS_SET, update).await
        }

        /// `stats.get`
        pub async fn stats(&self) -> Result<GlobalStats, ClientError> {
            self.call(method::STATS_GET, ()).await
        }

        /// `stats.history`: global speed samples, oldest first.
        pub async fn stats_history(&self) -> Result<Vec<dman_proto::SpeedSample>, ClientError> {
            self.call(method::STATS_HISTORY, ()).await
        }

        /// `engine.restart`: (re)start the aria2 engine; returns the engine status.
        pub async fn engine_restart(&self) -> Result<Vec<EngineStatus>, ClientError> {
            self.call(method::ENGINE_RESTART, ()).await
        }

        /// `engine.status`
        pub async fn engine_status(&self) -> Result<Vec<EngineStatus>, ClientError> {
            self.call(method::ENGINE_STATUS, ()).await
        }

        /// `daemon.shutdown`
        pub async fn shutdown(&self) -> Result<(), ClientError> {
            self.call::<_, Value>(method::DAEMON_SHUTDOWN, ())
                .await
                .map(drop)
        }
    };
}
pub(crate) use typed_api;

impl Client {
    typed_api!();
}

/// Background task owning the socket.
async fn run(
    mut ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    events: broadcast::Sender<Event>,
    closed_tx: watch::Sender<bool>,
) {
    let mut pending: HashMap<u64, Reply> = HashMap::new();
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                None => break,
                Some(Command::Cancel(id)) => { pending.remove(&id); }
                Some(Command::Call { id, payload, reply }) => {
                    pending.insert(id, reply);
                    if ws.send(Message::text(payload)).await.is_err() {
                        break;
                    }
                }
            },
            msg = ws.next() => match msg {
                Some(Ok(Message::Text(t))) => handle_text(t.as_str(), &mut pending, &events),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    closed_tx.send_replace(true);
    cmd_rx.close();
    for (_, reply) in pending.drain() {
        let _ = reply.send(Err(ClientError::Disconnected));
    }
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let Command::Call { reply, .. } = cmd {
            let _ = reply.send(Err(ClientError::Disconnected));
        }
    }
    let _ = ws.close(None).await;
}

fn handle_text(text: &str, pending: &mut HashMap<u64, Reply>, events: &broadcast::Sender<Event>) {
    let msg = match serde_json::from_str::<ServerMessage>(text) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("daemon sent an unparseable message: {e}");
            return;
        }
    };
    match msg {
        ServerMessage::Response(resp) => {
            let Some(id) = resp.id.as_u64() else {
                tracing::warn!("daemon response with non-numeric id");
                return;
            };
            let Some(reply) = pending.remove(&id) else {
                return; // timed out or cancelled
            };
            let res = match resp.error {
                Some(e) => Err(ClientError::Rpc(e.into())),
                None => Ok(resp.result.unwrap_or(Value::Null)),
            };
            let _ = reply.send(res);
        }
        ServerMessage::Notification(n) if n.method == method::EVENT => {
            match serde_json::from_value::<Event>(n.params) {
                Ok(event) => {
                    // No receivers is fine.
                    let _ = events.send(event);
                }
                Err(e) => tracing::warn!("daemon sent an unknown event: {e}"),
            }
        }
        ServerMessage::Notification(n) => {
            tracing::debug!(method = %n.method, "ignoring unknown notification");
        }
    }
}
