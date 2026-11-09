//! `[notify]`: desktop notifications, hooks and the webhook.

use serde::{Deserialize, Serialize};

/// An event that can notify (`on = [...]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyEvent {
    /// A download finished.
    Completed,
    /// A download failed.
    Failed,
    /// Seeding ended.
    SeedCompleted,
}

impl NotifyEvent {
    /// Every event, the default filter.
    pub const ALL: [NotifyEvent; 3] = [Self::Completed, Self::Failed, Self::SeedCompleted];

    /// Name as used in `on`, `DMAN_EVENT` and the webhook body.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::SeedCompleted => "seed_completed",
        }
    }
}

fn all_events() -> Vec<NotifyEvent> {
    NotifyEvent::ALL.to_vec()
}

/// Webhook target (`[notify.webhook]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    /// `http://` or `https://` URL receiving a JSON POST.
    pub url: String,
    /// Per-attempt timeout.
    #[serde(default = "default_webhook_timeout")]
    pub timeout_secs: u64,
    /// Retries after the first failed attempt (exponential backoff).
    #[serde(default = "default_retries")]
    pub retries: u32,
    /// Name of the secret in `secrets.toml` used to sign the body
    /// (`X-Dman-Signature: sha256=<hex HMAC-SHA256>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

fn default_webhook_timeout() -> u64 {
    10
}
fn default_retries() -> u32 {
    3
}
fn default_hook_timeout() -> u64 {
    60
}

/// A hook command (`[[notify.hook]]`), run without a shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Events this hook runs for (default: all).
    #[serde(default = "all_events")]
    pub on: Vec<NotifyEvent>,
    /// Program and arguments (argv; no shell is involved).
    pub command: Vec<String>,
    /// The process is killed after this many seconds.
    #[serde(default = "default_hook_timeout")]
    pub timeout_secs: u64,
}

/// The `[notify]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notify {
    /// Desktop notifications shown by the TUI (on the machine running it).
    pub desktop: bool,
    /// Also notify on the daemon host.
    pub desktop_on_daemon: bool,
    /// Events that notify at all (desktop, hooks and webhook).
    pub on: Vec<NotifyEvent>,
    /// Optional webhook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<Webhook>,
    /// Hook commands.
    pub hook: Vec<Hook>,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            desktop: true,
            desktop_on_daemon: false,
            on: all_events(),
            webhook: None,
            hook: Vec::new(),
        }
    }
}

impl Notify {
    /// Whether `event` passes the global `on` filter.
    pub fn enabled(&self, event: NotifyEvent) -> bool {
        self.on.contains(&event)
    }

    /// Semantic checks; the error names the offending setting.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(w) = &self.webhook {
            match url::Url::parse(&w.url) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
                Ok(_) => {
                    return Err(format!(
                        "notify.webhook.url `{}` must be http:// or https:// with a host",
                        w.url
                    ));
                }
                Err(e) => return Err(format!("notify.webhook.url is invalid: {e}")),
            }
            if !(1..=300).contains(&w.timeout_secs) {
                return Err("notify.webhook.timeout_secs must be 1..=300".into());
            }
            if w.retries > 10 {
                return Err("notify.webhook.retries must be at most 10".into());
            }
            if w.secret_ref.as_deref().is_some_and(|s| s.trim().is_empty()) {
                return Err("notify.webhook.secret_ref must not be empty".into());
            }
        }
        for (i, h) in self.hook.iter().enumerate() {
            let n = i + 1;
            if h.command.first().is_none_or(|c| c.trim().is_empty()) {
                return Err(format!("notify.hook #{n}: command must not be empty"));
            }
            if h.on.is_empty() {
                return Err(format!("notify.hook #{n}: on must not be empty"));
            }
            if !(1..=3600).contains(&h.timeout_secs) {
                return Err(format!("notify.hook #{n}: timeout_secs must be 1..=3600"));
            }
        }
        Ok(())
    }
}
