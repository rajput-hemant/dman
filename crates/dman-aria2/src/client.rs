//! The aria2 client implementation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::error::{Aria2Error, Error, Result};
use crate::types::{
    Aria2Event, Aria2Notification, File, FileUri, Gid, GlobalStat, Options, Peer, PosHow,
    ServerGroup, SessionInfo, Status, Version,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const NOTIFICATION_CAPACITY: usize = 256;

type Reply = oneshot::Sender<Result<Value>>;

enum Command {
    Call {
        id: u64,
        payload: String,
        reply: Reply,
    },
    Cancel(u64),
}

struct Inner {
    cmd_tx: mpsc::UnboundedSender<Command>,
    notif_tx: broadcast::Sender<Aria2Notification>,
    secret: SecretString,
    next_id: AtomicU64,
    state: watch::Receiver<bool>,
}

/// Cheaply clonable handle to one aria2 WebSocket connection.
///
/// A background task owns the socket. When the socket closes every pending
/// call fails with [`Error::Disconnected`] and later calls fail immediately;
/// create a new client to reconnect (see [`Aria2Client::closed`]).
#[derive(Clone)]
pub struct Aria2Client {
    inner: Arc<Inner>,
    timeout: Duration,
}

impl std::fmt::Debug for Aria2Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aria2Client")
            .field("connected", &self.is_connected())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Aria2Client {
    /// Connect to `ws://addr/jsonrpc`.
    pub async fn connect(addr: SocketAddr, secret: SecretString) -> Result<Self> {
        let url = Url::parse(&format!("ws://{addr}/jsonrpc"))
            .map_err(|e| Error::Connect(e.to_string()))?;
        Self::connect_url(&url, secret).await
    }

    /// Connect to an explicit `ws://` URL.
    pub async fn connect_url(url: &Url, secret: SecretString) -> Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| Error::Connect(e.to_string()))?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (notif_tx, _) = broadcast::channel(NOTIFICATION_CAPACITY);
        let (state_tx, state_rx) = watch::channel(true);
        tokio::spawn(run(ws, cmd_rx, notif_tx.clone(), state_tx));
        Ok(Self {
            inner: Arc::new(Inner {
                cmd_tx,
                notif_tx,
                secret,
                next_id: AtomicU64::new(1),
                state: state_rx,
            }),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Return a handle with a different per-call timeout (default 10 s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Subscribe to server notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<Aria2Notification> {
        self.inner.notif_tx.subscribe()
    }

    /// Whether the connection is still open.
    pub fn is_connected(&self) -> bool {
        *self.inner.state.borrow()
    }

    /// Resolves once the connection has closed.
    pub async fn closed(&self) {
        let mut rx = self.inner.state.clone();
        // An Err means the task ended and dropped the sender: also closed.
        let _ = rx.wait_for(|connected| !*connected).await;
    }

    /// Send a raw request without touching the params and await the result.
    async fn call_raw(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        let (reply, rx) = oneshot::channel();
        self.inner
            .cmd_tx
            .send(Command::Call { id, payload, reply })
            .map_err(|_| Error::Disconnected)?;
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(Error::Disconnected),
            Err(_) => {
                let _ = self.inner.cmd_tx.send(Command::Cancel(id));
                Err(Error::Timeout)
            }
        }
    }

    fn token(&self) -> Value {
        Value::String(format!("token:{}", self.inner.secret.expose_secret()))
    }

    /// Call any aria2 method (full name, e.g. `"aria2.getVersion"`). The
    /// `token:` secret is inserted as first parameter automatically.
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Vec<Value>) -> Result<T> {
        let mut full = Vec::with_capacity(params.len() + 1);
        full.push(self.token());
        full.extend(params);
        let v = self.call_raw(method, full).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Run several calls in one `system.multicall` request. Methods are full
    /// names; the token is inserted into every call. The outer result fails
    /// only on transport errors; each entry carries its own aria2 result.
    pub async fn multicall(
        &self,
        calls: Vec<(String, Vec<Value>)>,
    ) -> Result<Vec<std::result::Result<Value, Aria2Error>>> {
        let n = calls.len();
        let list: Vec<Value> = calls
            .into_iter()
            .map(|(m, p)| {
                let mut params = vec![self.token()];
                params.extend(p);
                json!({"methodName": m, "params": params})
            })
            .collect();
        let v = self
            .call_raw("system.multicall", vec![Value::Array(list)])
            .await?;
        let Value::Array(entries) = v else {
            return Err(Error::Protocol("multicall result is not an array".into()));
        };
        if entries.len() != n {
            return Err(Error::Protocol(format!(
                "multicall returned {} results for {n} calls",
                entries.len()
            )));
        }
        entries
            .into_iter()
            .map(|e| match e {
                Value::Array(mut a) if a.len() == 1 => Ok(Ok(a.remove(0))),
                obj @ Value::Object(_) => Ok(Err(serde_json::from_value::<Aria2Error>(obj)?)),
                _ => Err(Error::Protocol("malformed multicall entry".into())),
            })
            .collect()
    }

    async fn call_ok(&self, method: &str, params: Vec<Value>) -> Result<()> {
        self.call::<Value>(method, params).await.map(|_| ())
    }

    // ----- adding downloads -------------------------------------------------

    /// `aria2.addUri`
    pub async fn add_uri(
        &self,
        uris: &[String],
        options: &Options,
        position: Option<u32>,
    ) -> Result<Gid> {
        let mut p = vec![json!(uris), json!(options)];
        p.extend(position.map(|n| json!(n)));
        self.call("aria2.addUri", p).await
    }

    /// `aria2.addTorrent`; `torrent` is the raw `.torrent` file content.
    pub async fn add_torrent(
        &self,
        torrent: &[u8],
        uris: &[String],
        options: &Options,
        position: Option<u32>,
    ) -> Result<Gid> {
        let mut p = vec![json!(b64(torrent)), json!(uris), json!(options)];
        p.extend(position.map(|n| json!(n)));
        self.call("aria2.addTorrent", p).await
    }

    /// `aria2.addMetalink`; `metalink` is the raw file content.
    pub async fn add_metalink(
        &self,
        metalink: &[u8],
        options: &Options,
        position: Option<u32>,
    ) -> Result<Vec<Gid>> {
        let mut p = vec![json!(b64(metalink)), json!(options)];
        p.extend(position.map(|n| json!(n)));
        self.call("aria2.addMetalink", p).await
    }

    // ----- control ----------------------------------------------------------

    /// `aria2.remove`
    pub async fn remove(&self, gid: &Gid) -> Result<Gid> {
        self.call("aria2.remove", vec![json!(gid)]).await
    }

    /// `aria2.forceRemove`
    pub async fn force_remove(&self, gid: &Gid) -> Result<Gid> {
        self.call("aria2.forceRemove", vec![json!(gid)]).await
    }

    /// `aria2.pause`
    pub async fn pause(&self, gid: &Gid) -> Result<Gid> {
        self.call("aria2.pause", vec![json!(gid)]).await
    }

    /// `aria2.forcePause`
    pub async fn force_pause(&self, gid: &Gid) -> Result<Gid> {
        self.call("aria2.forcePause", vec![json!(gid)]).await
    }

    /// `aria2.pauseAll`
    pub async fn pause_all(&self) -> Result<()> {
        self.call_ok("aria2.pauseAll", vec![]).await
    }

    /// `aria2.forcePauseAll`
    pub async fn force_pause_all(&self) -> Result<()> {
        self.call_ok("aria2.forcePauseAll", vec![]).await
    }

    /// `aria2.unpause`
    pub async fn unpause(&self, gid: &Gid) -> Result<Gid> {
        self.call("aria2.unpause", vec![json!(gid)]).await
    }

    /// `aria2.unpauseAll`
    pub async fn unpause_all(&self) -> Result<()> {
        self.call_ok("aria2.unpauseAll", vec![]).await
    }

    // ----- queries ----------------------------------------------------------

    /// `aria2.tellStatus`; an empty `keys` slice returns all fields.
    pub async fn tell_status(&self, gid: &Gid, keys: &[&str]) -> Result<Status> {
        let mut p = vec![json!(gid)];
        p.extend(keys_param(keys));
        self.call("aria2.tellStatus", p).await
    }

    /// `aria2.tellActive`
    pub async fn tell_active(&self, keys: &[&str]) -> Result<Vec<Status>> {
        self.call("aria2.tellActive", keys_param(keys).into_iter().collect())
            .await
    }

    /// `aria2.tellWaiting`
    pub async fn tell_waiting(&self, offset: i64, num: u32, keys: &[&str]) -> Result<Vec<Status>> {
        let mut p = vec![json!(offset), json!(num)];
        p.extend(keys_param(keys));
        self.call("aria2.tellWaiting", p).await
    }

    /// `aria2.tellStopped`
    pub async fn tell_stopped(&self, offset: i64, num: u32, keys: &[&str]) -> Result<Vec<Status>> {
        let mut p = vec![json!(offset), json!(num)];
        p.extend(keys_param(keys));
        self.call("aria2.tellStopped", p).await
    }

    /// `aria2.getFiles`
    pub async fn get_files(&self, gid: &Gid) -> Result<Vec<File>> {
        self.call("aria2.getFiles", vec![json!(gid)]).await
    }

    /// `aria2.getPeers`
    pub async fn get_peers(&self, gid: &Gid) -> Result<Vec<Peer>> {
        self.call("aria2.getPeers", vec![json!(gid)]).await
    }

    /// `aria2.getServers`
    pub async fn get_servers(&self, gid: &Gid) -> Result<Vec<ServerGroup>> {
        self.call("aria2.getServers", vec![json!(gid)]).await
    }

    /// `aria2.getUris`
    pub async fn get_uris(&self, gid: &Gid) -> Result<Vec<FileUri>> {
        self.call("aria2.getUris", vec![json!(gid)]).await
    }

    /// `aria2.changePosition`; returns the new position.
    pub async fn change_position(&self, gid: &Gid, pos: i64, how: PosHow) -> Result<i64> {
        self.call(
            "aria2.changePosition",
            vec![json!(gid), json!(pos), json!(how.as_str())],
        )
        .await
    }

    // ----- options ----------------------------------------------------------

    /// `aria2.changeOption`
    pub async fn change_option(&self, gid: &Gid, options: &Options) -> Result<()> {
        self.call_ok("aria2.changeOption", vec![json!(gid), json!(options)])
            .await
    }

    /// `aria2.getOption`
    pub async fn get_option(&self, gid: &Gid) -> Result<Options> {
        self.call("aria2.getOption", vec![json!(gid)]).await
    }

    /// `aria2.changeGlobalOption`
    pub async fn change_global_option(&self, options: &Options) -> Result<()> {
        self.call_ok("aria2.changeGlobalOption", vec![json!(options)])
            .await
    }

    /// `aria2.getGlobalOption`
    pub async fn get_global_option(&self) -> Result<Options> {
        self.call("aria2.getGlobalOption", vec![]).await
    }

    // ----- global -----------------------------------------------------------

    /// `aria2.getGlobalStat`
    pub async fn get_global_stat(&self) -> Result<GlobalStat> {
        self.call("aria2.getGlobalStat", vec![]).await
    }

    /// `aria2.purgeDownloadResult`
    pub async fn purge_download_result(&self) -> Result<()> {
        self.call_ok("aria2.purgeDownloadResult", vec![]).await
    }

    /// `aria2.removeDownloadResult`
    pub async fn remove_download_result(&self, gid: &Gid) -> Result<()> {
        self.call_ok("aria2.removeDownloadResult", vec![json!(gid)])
            .await
    }

    /// `aria2.getVersion`
    pub async fn get_version(&self) -> Result<Version> {
        self.call("aria2.getVersion", vec![]).await
    }

    /// `aria2.getSessionInfo`
    pub async fn get_session_info(&self) -> Result<SessionInfo> {
        self.call("aria2.getSessionInfo", vec![]).await
    }

    /// `aria2.saveSession`
    pub async fn save_session(&self) -> Result<()> {
        self.call_ok("aria2.saveSession", vec![]).await
    }

    /// `aria2.shutdown`
    pub async fn shutdown(&self) -> Result<()> {
        self.call_ok("aria2.shutdown", vec![]).await
    }

    /// `aria2.forceShutdown`
    pub async fn force_shutdown(&self) -> Result<()> {
        self.call_ok("aria2.forceShutdown", vec![]).await
    }
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn keys_param(keys: &[&str]) -> Option<Value> {
    (!keys.is_empty()).then(|| json!(keys))
}

/// Background task owning the socket.
async fn run(
    mut ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    notif_tx: broadcast::Sender<Aria2Notification>,
    state_tx: watch::Sender<bool>,
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
                Some(Ok(Message::Text(t))) => handle_text(t.as_str(), &mut pending, &notif_tx),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = state_tx.send(false);
    cmd_rx.close();
    for (_, reply) in pending.drain() {
        let _ = reply.send(Err(Error::Disconnected));
    }
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let Command::Call { reply, .. } = cmd {
            let _ = reply.send(Err(Error::Disconnected));
        }
    }
    let _ = ws.close(None).await;
}

fn handle_text(
    text: &str,
    pending: &mut HashMap<u64, Reply>,
    notif_tx: &broadcast::Sender<Aria2Notification>,
) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        tracing::warn!("aria2 sent invalid JSON");
        return;
    };
    if let Some(id) = v.get("id").and_then(Value::as_u64) {
        let Some(reply) = pending.remove(&id) else {
            return; // cancelled or timed out
        };
        let res = if let Some(err) = v.get("error") {
            match serde_json::from_value::<Aria2Error>(err.clone()) {
                Ok(e) => Err(e.into()),
                Err(_) => Err(Error::Protocol("malformed error object".into())),
            }
        } else if let Some(result) = v.get("result") {
            Ok(result.clone())
        } else {
            Err(Error::Protocol("response without result or error".into()))
        };
        let _ = reply.send(res);
    } else if let Some(method) = v.get("method").and_then(Value::as_str) {
        let Some(event) = Aria2Event::from_method(method) else {
            return;
        };
        let gid = v
            .pointer("/params/0/gid")
            .and_then(Value::as_str)
            .map(Gid::from);
        match gid {
            Some(gid) => {
                let _ = notif_tx.send(Aria2Notification { event, gid });
            }
            None => tracing::warn!(method, "aria2 notification without gid"),
        }
    }
}
