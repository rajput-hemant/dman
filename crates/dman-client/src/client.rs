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
use crate::tls::{TlsOptions, connect_error};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_CAPACITY: usize = 256;
/// How often an idle connection is pinged, and how long silence (no frame of any
/// kind, pongs included) is tolerated before the connection counts as dead.
/// Without this a half-open TCP connection would never be noticed.
const PING_EVERY: Duration = Duration::from_secs(20);
const DEAD_AFTER: Duration = Duration::from_secs(60);

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
        Self::connect_tls(url, token, client_name, &TlsOptions::default()).await
    }

    /// Like [`Client::connect`] with explicit TLS options for `wss://` URLs.
    ///
    /// # Errors
    /// As [`Client::connect`]; certificate problems are [`ClientError::Connect`] with a
    /// message starting `certificate not trusted`.
    pub async fn connect_tls(
        url: &Url,
        token: SecretString,
        client_name: &str,
        tls: &TlsOptions,
    ) -> Result<Self, ClientError> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self::connect_with_events(url, &token, client_name, events, tls).await
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
        tls: &TlsOptions,
    ) -> Result<Self, ClientError> {
        if url.scheme() == "ws" && !is_loopback_host(url) {
            tracing::warn!(
                "connecting over plain ws:// to a non-loopback host: the access token is sent unencrypted; use wss://"
            );
        }
        let connector = if url.scheme() == "wss" {
            Some(tls.connector()?)
        } else {
            None
        };
        let connecting =
            tokio_tungstenite::connect_async_tls_with_config(url.as_str(), None, false, connector);
        let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| ClientError::Connect("connection attempt timed out".into()))?
            .map_err(|e| connect_error(&e))?;

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

    /// Like [`Client::call`] with a custom timeout for this call only.
    ///
    /// # Errors
    /// As [`Client::call`].
    pub async fn call_with_timeout<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
        timeout: Duration,
    ) -> Result<R, ClientError> {
        self.clone()
            .with_timeout(timeout)
            .call(method, params)
            .await
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

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(a)) => a.is_loopback(),
        Some(url::Host::Ipv6(a)) => a.is_loopback(),
        None => false,
    }
}

/// Generates the typed API on a type that has
/// `async fn call<P, R>(&self, &str, P) -> Result<R, ClientError>`.
macro_rules! typed_api {
    () => {
        /// `jobs.add`: one outcome per source, in order. The daemon fetches
        /// http(s) `.torrent` URLs before adding them, so this may take a while.
        pub async fn add(&self, params: AddParams) -> Result<Vec<AddOutcome>, ClientError> {
            let wait = 15 + 30 * params.sources.len().min(10) as u64;
            self.call_with_timeout(method::JOBS_ADD, params, Duration::from_secs(wait))
                .await
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

        /// `jobs.rename`: new output name (waiting/paused) or move on disk
        /// (completed). `file_index` is 1-based and selects a file of a
        /// multi-file job.
        pub async fn rename(
            &self,
            id: JobId,
            name: &str,
            file_index: Option<u32>,
        ) -> Result<(), ClientError> {
            self.call::<_, Value>(
                method::JOBS_RENAME,
                method::RenameParams {
                    id,
                    name: name.to_owned(),
                    file_index,
                },
            )
            .await
            .map(drop)
        }

        /// `jobs.preview_magnet`: fetch the file list of a magnet without
        /// starting it. Can take up to `timeout_secs` (default 120 s); the call
        /// waits that long plus a margin.
        pub async fn preview_magnet(
            &self,
            uri: &str,
            timeout_secs: Option<u32>,
        ) -> Result<method::MagnetPreview, ClientError> {
            let wait = u64::from(timeout_secs.unwrap_or(120).min(600)) + 30;
            self.call_with_timeout(
                method::JOBS_PREVIEW_MAGNET,
                method::PreviewMagnetParams {
                    uri: uri.to_owned(),
                    dir: None,
                    timeout_secs,
                },
                Duration::from_secs(wait),
            )
            .await
        }

        /// `video.probe`: formats, subtitles, playlist entries and quality
        /// plans of a video URL (yt-dlp runs on the daemon host, up to 60 s).
        pub async fn video_probe(
            &self,
            params: dman_proto::VideoProbeParams,
        ) -> Result<dman_proto::VideoProbe, ClientError> {
            self.call_with_timeout(method::VIDEO_PROBE, params, Duration::from_secs(90))
                .await
        }

        /// `video.plan`: the format selector and post-processing a video download with
        /// these options would use now (nothing is probed).
        pub async fn video_plan(
            &self,
            params: dman_proto::VideoPlanParams,
        ) -> Result<dman_proto::VideoDownloadPlan, ClientError> {
            self.call(method::VIDEO_PLAN, params).await
        }

        /// `video.preferences.get`: the quality-picker defaults (`[video]`).
        pub async fn video_preferences(&self) -> Result<dman_proto::VideoPreferences, ClientError> {
            self.call(method::VIDEO_PREFERENCES_GET, ()).await
        }

        /// `video.preferences.set`: changes the given fields, applies them at
        /// once, saves them to `[video]` in config.toml and returns the result.
        pub async fn video_preferences_set(
            &self,
            update: dman_proto::VideoPreferencesUpdate,
        ) -> Result<dman_proto::VideoPreferences, ClientError> {
            self.call(method::VIDEO_PREFERENCES_SET, update).await
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

        /// `jobs.restore`: undo a remove-from-list (never possible after file deletion).
        pub async fn restore(&self, ids: &[JobId]) -> Result<method::BatchResult, ClientError> {
            self.call(method::JOBS_RESTORE, IdsParams { ids: ids.to_vec() })
                .await
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

        /// `schedule.get`: configured windows, the active one and the next change.
        pub async fn schedule(&self) -> Result<dman_proto::method::ScheduleStatus, ClientError> {
            self.call(method::SCHEDULE_GET, ()).await
        }

        /// `jobs.cleanup_unselected`: delete (or with `dry_run` only list) the partial
        /// files of a torrent's deselected files.
        pub async fn cleanup_unselected(
            &self,
            id: JobId,
            dry_run: bool,
        ) -> Result<method::CleanupResult, ClientError> {
            self.call(
                method::JOBS_CLEANUP_UNSELECTED,
                method::CleanupParams { id, dry_run },
            )
            .await
        }

        /// `config.reload`: re-read `config.toml` and apply what can be applied live.
        pub async fn config_reload(&self) -> Result<method::ReloadResult, ClientError> {
            self.call(method::CONFIG_RELOAD, ()).await
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

        /// `fs.list`: list a directory on the daemon host.
        pub async fn fs_list(
            &self,
            params: method::FsListParams,
        ) -> Result<method::FsListing, ClientError> {
            self.call(method::FS_LIST, params).await
        }

        /// `search.start`: begin a background search; progress arrives as
        /// `SearchUpdate` / `SearchFinished` events (subscribe first).
        pub async fn search_start(
            &self,
            params: dman_proto::search::SearchStartParams,
        ) -> Result<dman_proto::search::SearchStarted, ClientError> {
            self.call(dman_proto::search::SEARCH_START, params).await
        }

        /// `search.cancel`
        pub async fn search_cancel(&self, search_id: u64) -> Result<(), ClientError> {
            self.call::<_, Value>(
                dman_proto::search::SEARCH_CANCEL,
                dman_proto::search::SearchIdParams { search_id },
            )
            .await
            .map(drop)
        }

        /// `search.results`: latest merged results of one of the last searches.
        pub async fn search_results(
            &self,
            search_id: u64,
        ) -> Result<dman_proto::search::SearchSnapshot, ClientError> {
            self.call(
                dman_proto::search::SEARCH_RESULTS,
                dman_proto::search::SearchIdParams { search_id },
            )
            .await
        }

        /// `search.resolve`: magnet link from a result's detail page.
        pub async fn search_resolve(
            &self,
            params: dman_proto::search::SearchResolveParams,
        ) -> Result<dman_proto::search::ResolvedMagnet, ClientError> {
            self.call_with_timeout(
                dman_proto::search::SEARCH_RESOLVE,
                params,
                Duration::from_secs(90),
            )
            .await
        }

        /// `search.history`: recent queries, newest first.
        pub async fn search_history(
            &self,
        ) -> Result<Vec<dman_proto::search::SearchHistoryEntry>, ClientError> {
            self.call(dman_proto::search::SEARCH_HISTORY, ()).await
        }

        /// `search.history_clear`
        pub async fn search_history_clear(&self) -> Result<(), ClientError> {
            self.call::<_, Value>(dman_proto::search::SEARCH_HISTORY_CLEAR, ())
                .await
                .map(drop)
        }

        /// `providers.list`
        pub async fn providers_list(
            &self,
        ) -> Result<Vec<dman_proto::search::ProviderInfo>, ClientError> {
            self.call(dman_proto::search::PROVIDERS_LIST, ()).await
        }

        /// `providers.set`: applied at once and saved to `config.toml`.
        pub async fn providers_set(
            &self,
            params: dman_proto::search::ProviderSetParams,
        ) -> Result<dman_proto::search::ProviderInfo, ClientError> {
            self.call(dman_proto::search::PROVIDERS_SET, params).await
        }

        /// `providers.test`: a failing provider is `ok == false`, not an error.
        pub async fn providers_test(
            &self,
            name: &str,
        ) -> Result<dman_proto::search::ProviderTestReport, ClientError> {
            self.call_with_timeout(
                dman_proto::search::PROVIDERS_TEST,
                dman_proto::search::ProviderNameParams {
                    name: name.to_owned(),
                },
                Duration::from_secs(90),
            )
            .await
        }

        /// `providers.import`: add or replace custom providers from TOML.
        pub async fn providers_import(
            &self,
            toml: &str,
        ) -> Result<dman_proto::search::ProvidersImported, ClientError> {
            self.call(
                dman_proto::search::PROVIDERS_IMPORT,
                dman_proto::search::ProvidersImportParams {
                    toml: toml.to_owned(),
                },
            )
            .await
        }

        /// `providers.export`: TOML of the named providers (none = custom ones).
        pub async fn providers_export(
            &self,
            names: Option<Vec<String>>,
        ) -> Result<String, ClientError> {
            self.call(
                dman_proto::search::PROVIDERS_EXPORT,
                dman_proto::search::ProvidersExportParams { names },
            )
            .await
        }

        /// `providers.remove`: custom providers only.
        pub async fn providers_remove(&self, name: &str) -> Result<(), ClientError> {
            self.call::<_, Value>(
                dman_proto::search::PROVIDERS_REMOVE,
                dman_proto::search::ProviderNameParams {
                    name: name.to_owned(),
                },
            )
            .await
            .map(drop)
        }

        /// `trackers.status`
        pub async fn trackers_status(
            &self,
        ) -> Result<dman_proto::search::TrackersStatus, ClientError> {
            self.call(dman_proto::search::TRACKERS_STATUS, ()).await
        }

        /// `web.status`
        pub async fn web_status(&self) -> Result<method::WebStatus, ClientError> {
            self.call(method::WEB_STATUS, ()).await
        }

        /// `web.start`
        pub async fn web_start(
            &self,
            params: method::WebStartParams,
        ) -> Result<method::WebStatus, ClientError> {
            self.call(method::WEB_START, params).await
        }

        /// `web.stop`
        pub async fn web_stop(&self, persist: bool) -> Result<method::WebStatus, ClientError> {
            self.call(method::WEB_STOP, method::WebStopParams { persist })
                .await
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
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if last_rx.elapsed() > DEAD_AFTER
                    || ws.send(Message::Ping(Vec::new().into())).await.is_err()
                {
                    break;
                }
            }
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
            msg = ws.next() => {
                last_rx = tokio::time::Instant::now();
                match msg {
                    Some(Ok(Message::Text(t))) => handle_text(t.as_str(), &mut pending, &events),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
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
