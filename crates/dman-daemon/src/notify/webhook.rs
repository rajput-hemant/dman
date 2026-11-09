//! Webhook delivery: JSON POST with retries, exponential backoff and an
//! optional HMAC-SHA256 signature.

use std::time::Duration;

use dman_config::Webhook;
use hmac::{Hmac, KeyInit as _, Mac as _};
use secrecy::{ExposeSecret as _, SecretString};
use sha2::Sha256;

/// Header carrying `sha256=<hex>` of the body.
pub const SIGNATURE_HEADER: &str = "X-Dman-Signature";

/// Backoff between attempts: `base * 2^attempt`, capped at `max`.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    fn delay(&self, attempt: u32) -> Duration {
        self.base
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max)
    }
}

/// `sha256=<hex HMAC-SHA256(secret, body)>`.
pub fn signature(secret: &str, body: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let mut s = String::from("sha256=");
    for b in tag {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Sends webhook bodies.
pub struct WebhookSender {
    client: reqwest::Client,
    cfg: Webhook,
    secret: Option<SecretString>,
    backoff: Backoff,
}

impl WebhookSender {
    /// Builds the HTTP client; the TLS provider must be installed.
    pub fn new(cfg: Webhook, secret: Option<SecretString>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .user_agent(concat!("dman/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            cfg,
            secret,
            backoff: Backoff::default(),
        })
    }

    /// Replaces the retry delays (tests).
    pub fn with_backoff(mut self, b: Backoff) -> Self {
        self.backoff = b;
        self
    }

    /// POSTs `body`; retries up to `retries` times after the first failure.
    /// Errors never contain the URL (it may carry credentials) or the secret.
    pub async fn send(&self, body: &[u8]) -> Result<(), String> {
        let attempts = self.cfg.retries.saturating_add(1);
        let mut last = String::new();
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(self.backoff.delay(attempt - 1)).await;
            }
            let mut req = self
                .client
                .post(&self.cfg.url)
                .header("Content-Type", "application/json")
                .body(body.to_vec());
            if let Some(s) = &self.secret {
                req = req.header(SIGNATURE_HEADER, signature(s.expose_secret(), body));
            }
            match req.send().await {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.without_url().to_string(),
            }
            tracing::debug!(attempt = attempt + 1, "webhook attempt failed: {last}");
        }
        Err(format!("gave up after {attempts} attempt(s): {last}"))
    }
}
