//! A supervised connection that reconnects with exponential backoff.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dman_proto::method::{
    self, EngineStatus, Event, IdParams, IdsParams, LimitsUpdate, ListParams,
};
use dman_proto::method::{RemoveParams, Reorder, ReorderParams, SetOptionsParams};
use dman_proto::{AddOutcome, AddParams, GlobalStats, Job, JobId, JobOptions, Limits};
use secrecy::SecretString;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use url::Url;

use crate::client::{Client, typed_api};
use crate::error::ClientError;
use crate::tls::TlsOptions;

const EVENT_CAPACITY: usize = 256;
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Connection state published by [`ReconnectingClient::state`].
#[derive(Debug, Clone, PartialEq)]
pub enum ConnState {
    /// A connection attempt is in progress.
    Connecting,
    /// Connected, authenticated and subscribed.
    Connected {
        /// Version string reported by the daemon.
        daemon_version: String,
    },
    /// Not connected; the next attempt happens after `retry_in`.
    Disconnected {
        /// Delay before the next attempt.
        retry_in: Duration,
        /// Why the last attempt or connection failed.
        last_error: String,
    },
    /// Permanent failure (bad token or incompatible daemon); retries stopped.
    Failed {
        /// The fatal error.
        error: ClientError,
    },
}

struct Inner {
    current: Arc<Mutex<Option<Client>>>,
    events: broadcast::Sender<Event>,
    state: watch::Receiver<ConnState>,
    task: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A client that keeps reconnecting in the background.
///
/// Events from every connection flow into one long-lived broadcast channel; after
/// each reconnect the client re-subscribes so a fresh `Snapshot` arrives. Calls
/// while disconnected fail immediately with [`ClientError::Disconnected`]
/// (actions are disabled, never queued). The supervisor stops when the last
/// clone is dropped.
#[derive(Clone)]
pub struct ReconnectingClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ReconnectingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectingClient")
            .field("state", &*self.inner.state.borrow())
            .finish_non_exhaustive()
    }
}

impl ReconnectingClient {
    /// Start the supervisor (must be called inside a Tokio runtime) with the
    /// default backoff of 250 ms doubling up to 10 s.
    pub fn spawn(url: Url, token: SecretString, client_name: &str) -> Self {
        Self::spawn_with_backoff(url, token, client_name, INITIAL_BACKOFF, MAX_BACKOFF)
    }

    /// Like [`ReconnectingClient::spawn`] with custom backoff bounds.
    pub fn spawn_with_backoff(
        url: Url,
        token: SecretString,
        client_name: &str,
        initial: Duration,
        max: Duration,
    ) -> Self {
        Self::spawn_tls(url, token, client_name, initial, max, TlsOptions::default())
    }

    /// Like [`ReconnectingClient::spawn_with_backoff`] with TLS options for `wss://` URLs.
    pub fn spawn_tls(
        url: Url,
        token: SecretString,
        client_name: &str,
        initial: Duration,
        max: Duration,
        tls: TlsOptions,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (state_tx, state) = watch::channel(ConnState::Connecting);
        let current = Arc::new(Mutex::new(None));
        let task = tokio::spawn(supervise(
            url,
            token,
            client_name.to_owned(),
            initial,
            max,
            tls,
            events.clone(),
            current.clone(),
            state_tx,
        ));
        Self {
            inner: Arc::new(Inner {
                current,
                events,
                state,
                task,
            }),
        }
    }

    /// Watch the connection state.
    pub fn state(&self) -> watch::Receiver<ConnState> {
        self.inner.state.clone()
    }

    /// Receiver for events across all connections. Each (re)connect starts with a
    /// `Snapshot`, which consumers should treat as a full state replacement.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// The current connection, if any.
    pub fn client(&self) -> Option<Client> {
        self.inner
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Call any daemon method on the current connection.
    ///
    /// # Errors
    /// [`ClientError::Disconnected`] immediately when not connected, otherwise as
    /// [`Client::call`].
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, ClientError> {
        self.client()
            .ok_or(ClientError::Disconnected)?
            .call(method, params)
            .await
    }

    /// Like [`ReconnectingClient::call`] with a custom timeout for this call.
    ///
    /// # Errors
    /// As [`ReconnectingClient::call`].
    pub async fn call_with_timeout<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
        timeout: Duration,
    ) -> Result<R, ClientError> {
        self.client()
            .ok_or(ClientError::Disconnected)?
            .call_with_timeout(method, params, timeout)
            .await
    }

    typed_api!();
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    url: Url,
    token: SecretString,
    name: String,
    initial: Duration,
    max: Duration,
    tls: TlsOptions,
    events: broadcast::Sender<Event>,
    current: Arc<Mutex<Option<Client>>>,
    state: watch::Sender<ConnState>,
) {
    let mut delay = initial;
    loop {
        state.send_replace(ConnState::Connecting);
        let attempt = async {
            let client =
                Client::connect_with_events(&url, &token, &name, events.clone(), &tls).await?;
            client.subscribe().await?;
            Ok::<_, ClientError>(client)
        }
        .await;
        let last_error = match attempt {
            Ok(client) => {
                delay = initial;
                let version = client.daemon_version().to_owned();
                *current.lock().unwrap_or_else(PoisonError::into_inner) = Some(client.clone());
                tracing::info!(daemon_version = %version, "connected to daemon");
                state.send_replace(ConnState::Connected {
                    daemon_version: version,
                });
                client.closed().await;
                *current.lock().unwrap_or_else(PoisonError::into_inner) = None;
                ClientError::Disconnected.to_string()
            }
            Err(error) if error.is_fatal() => {
                tracing::error!(%error, "daemon connection failed permanently");
                state.send_replace(ConnState::Failed { error });
                return;
            }
            Err(error) => error.to_string(),
        };
        tracing::debug!(%last_error, ?delay, "will reconnect");
        state.send_replace(ConnState::Disconnected {
            retry_in: delay,
            last_error,
        });
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(max);
    }
}
