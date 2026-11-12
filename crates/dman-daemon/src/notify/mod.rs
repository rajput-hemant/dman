//! Notifications: hooks, webhook and desktop notifications for `JobFinished`.
//!
//! The [`Notifier`] subscribes to the event bus. For each `JobFinished` that
//! passes the `[notify] on` filter it looks the job up, then runs the matching
//! hooks (at most [`MAX_HOOKS`] processes at a time), posts the webhook and,
//! when `desktop_on_daemon` is set, shows a desktop notification. Failures
//! become `Message` warnings on the event bus; nothing here can stall the
//! manager.

mod desktop;
mod hook;
mod webhook;

#[cfg(test)]
#[cfg(unix)] // the hooks under test are shell scripts
mod tests;

use std::sync::Arc;

use dman_config::{Notify, NotifyEvent};
use dman_proto::method::{Event, Level, Outcome};
use dman_proto::{Job, JobId};
use secrecy::SecretString;
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, watch};

use crate::manager::{ManagerHandle, Request};

pub use hook::run_hook;
pub use webhook::{Backoff, WebhookSender, signature};

/// Never run more hooks than this at once.
pub const MAX_HOOKS: usize = 4;

/// Installs the `ring` TLS provider for reqwest. Idempotent; call before
/// building any HTTP client.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Everything known about a finished job; the webhook body and the hook
/// environment are derived from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Payload {
    /// `completed`, `failed` or `seed_completed`.
    pub event: String,
    pub id: u64,
    pub name: String,
    pub path: String,
    /// Job state (`complete`, `seeding`, `error`, ...).
    pub status: String,
    pub size: u64,
    pub engine: String,
    pub error: Option<String>,
    /// Unix seconds.
    pub at: i64,
}

fn word<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl Payload {
    /// Builds the payload from the event fields and the job (when still known).
    pub fn new(
        id: JobId,
        name: &str,
        path: &str,
        outcome: &Outcome,
        job: Option<&Job>,
        at: i64,
    ) -> Self {
        let (status, engine, size) = match job {
            Some(j) => (word(&j.state), word(&j.engine), j.total_bytes),
            None => (String::new(), String::new(), 0),
        };
        Self {
            event: outcome.name().to_owned(),
            id: id.0,
            name: name.to_owned(),
            path: path.to_owned(),
            status,
            engine,
            size,
            error: match outcome {
                Outcome::Failed { error } => Some(error.clone()),
                _ => None,
            },
            at,
        }
    }

    /// The `DMAN_*` environment of a hook process.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("DMAN_EVENT", self.event.clone()),
            ("DMAN_ID", self.id.to_string()),
            ("DMAN_NAME", self.name.clone()),
            ("DMAN_PATH", self.path.clone()),
            ("DMAN_STATUS", self.status.clone()),
            ("DMAN_ENGINE", self.engine.clone()),
            ("DMAN_SIZE", self.size.to_string()),
            ("DMAN_ERROR", self.error.clone().unwrap_or_default()),
        ]
    }
}

fn event_of(outcome: &Outcome) -> NotifyEvent {
    match outcome {
        Outcome::Completed => NotifyEvent::Completed,
        Outcome::Failed { .. } => NotifyEvent::Failed,
        Outcome::SeedCompleted => NotifyEvent::SeedCompleted,
    }
}

/// The notification service of the daemon. Cheap to clone.
#[derive(Clone)]
pub struct Notifier {
    cfg: Notify,
    webhook: Option<Arc<WebhookSender>>,
    hooks: Arc<Semaphore>,
    events: broadcast::Sender<Event>,
}

impl Notifier {
    /// `secret` is the resolved `notify.webhook.secret_ref`, if any.
    pub fn new(
        cfg: Notify,
        secret: Option<SecretString>,
        events: broadcast::Sender<Event>,
    ) -> anyhow::Result<Self> {
        install_crypto_provider();
        let webhook = match &cfg.webhook {
            Some(w) => Some(Arc::new(WebhookSender::new(w.clone(), secret)?)),
            None => None,
        };
        Ok(Self {
            cfg,
            webhook,
            hooks: Arc::new(Semaphore::new(MAX_HOOKS)),
            events,
        })
    }

    /// Whether anything would happen for any event (nothing configured = no
    /// need to run a task at all).
    pub fn is_idle(&self) -> bool {
        self.webhook.is_none() && self.cfg.hook.is_empty() && !self.cfg.desktop_on_daemon
    }

    fn warn(events: &broadcast::Sender<Event>, text: String) {
        tracing::warn!("{text}");
        let _ = events.send(Event::Message {
            level: Level::Warn,
            text,
        });
    }

    /// Handles one finished job: spawns the hooks, webhook and desktop
    /// notification and returns immediately.
    pub fn dispatch(&self, payload: Payload, outcome: &Outcome) {
        let event = event_of(outcome);
        if !self.cfg.enabled(event) {
            return;
        }
        for (i, hook) in self.cfg.hook.iter().enumerate() {
            if !hook.on.contains(&event) {
                continue;
            }
            let hook = hook.clone();
            let env = payload.env();
            let sem = self.hooks.clone();
            let events = self.events.clone();
            let program = hook.command.first().map_or("", String::as_str);
            let label = format!("hook #{} ({program})", i + 1);
            let name = payload.name.clone();
            tokio::spawn(async move {
                let Ok(_permit) = sem.acquire_owned().await else {
                    return;
                };
                if let Err(e) = run_hook(&hook, &env).await {
                    Self::warn(&events, format!("{label} for {name}: {e}"));
                }
            });
        }
        if let Some(wh) = &self.webhook {
            let wh = wh.clone();
            let events = self.events.clone();
            let body = serde_json::to_vec(&payload).unwrap_or_default();
            let name = payload.name.clone();
            tokio::spawn(async move {
                if let Err(e) = wh.send(&body).await {
                    Self::warn(&events, format!("webhook for {name}: {e}"));
                }
            });
        }
        if self.cfg.desktop_on_daemon {
            desktop::show(&payload);
        }
    }

    /// Runs until the event bus closes. The notifier in `current` is looked up
    /// for every event, so `config.reload` can swap it at any time.
    pub async fn run(
        current: watch::Receiver<Notifier>,
        mut rx: broadcast::Receiver<Event>,
        manager: ManagerHandle,
    ) {
        loop {
            match rx.recv().await {
                Ok(Event::JobFinished {
                    id,
                    name,
                    path,
                    outcome,
                }) => {
                    let this = current.borrow().clone();
                    if this.is_idle() {
                        continue;
                    }
                    let job = match manager.call(Request::Get(id)).await {
                        Ok(v) => serde_json::from_value::<Job>(v).ok(),
                        Err(_) => None,
                    };
                    let at = crate::manager::now();
                    let payload = Payload::new(id, &name, &path, &outcome, job.as_ref(), at);
                    this.dispatch(payload, &outcome);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("notifier lagged; {n} events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}
