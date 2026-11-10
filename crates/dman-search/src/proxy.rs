//! Proxy settings for provider and tracker-list requests.
//!
//! A proxy URL may carry credentials (`socks5h://user:pass@host:1080`), so the
//! type keeps them out of `Debug`, `Display` and error messages; only
//! [`ProxyUrl::as_str`] gives the raw value, for building the HTTP client.

use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

/// The value that forces a direct connection, overriding `[search] proxy`.
pub const PROXY_NONE: &str = "none";

/// `http://`, `https://`, `socks5://` or `socks5h://` proxy URL, or `none`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProxyUrl(String);

impl ProxyUrl {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The raw value, credentials included. Never log it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `proxy = "none"`: connect directly even when `[search] proxy` is set.
    pub fn is_direct(&self) -> bool {
        self.0.trim().eq_ignore_ascii_case(PROXY_NONE)
    }

    /// Scheme, host and port; credentials are replaced by `***`, path and
    /// query are dropped. Safe to show and to log.
    pub fn redacted(&self) -> String {
        if self.is_direct() {
            return PROXY_NONE.to_owned();
        }
        match Url::parse(self.0.trim()) {
            Ok(u) => {
                let creds = if u.username().is_empty() && u.password().is_none() {
                    ""
                } else {
                    "***@"
                };
                let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
                format!(
                    "{}://{creds}{}{port}",
                    u.scheme(),
                    u.host_str().unwrap_or_default()
                )
            }
            Err(_) => "(invalid proxy url)".to_owned(),
        }
    }

    /// Syntax check (no network).
    ///
    /// # Errors
    /// A message that never contains the credentials.
    pub fn validate(&self) -> Result<(), String> {
        if self.is_direct() {
            return Ok(());
        }
        match Url::parse(self.0.trim()) {
            Ok(u)
                if matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h")
                    && u.host_str().is_some_and(|h| !h.is_empty()) =>
            {
                Ok(())
            }
            _ => Err(format!(
                "proxy `{}` must be an http, https, socks5 or socks5h URL (or `none`)",
                self.redacted()
            )),
        }
    }
}

impl From<String> for ProxyUrl {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ProxyUrl {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ProxyUrl").field(&self.redacted()).finish()
    }
}

impl fmt::Display for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_everywhere() {
        let p = ProxyUrl::new("socks5h://alice:hunter2@proxy.invalid:1080/x?k=v");
        assert_eq!(p.redacted(), "socks5h://***@proxy.invalid:1080");
        for s in [format!("{p}"), format!("{p:?}"), format!("{:?}", Some(&p))] {
            assert!(!s.contains("hunter2") && !s.contains("alice"), "{s}");
        }
        assert_eq!(
            p.as_str(),
            "socks5h://alice:hunter2@proxy.invalid:1080/x?k=v"
        );
    }

    #[test]
    fn validation() {
        for ok in [
            "socks5h://127.0.0.1:9050",
            "http://p:3128",
            "https://p",
            "none",
            "NONE",
        ] {
            assert!(ProxyUrl::new(ok).validate().is_ok(), "{ok}");
        }
        for bad in ["", "nonsense", "ftp://h:1", "socks4://h:1", "http://"] {
            assert!(ProxyUrl::new(bad).validate().is_err(), "{bad}");
        }
        let e = ProxyUrl::new("ftp://u:secret@h:1").validate().unwrap_err();
        assert!(!e.contains("secret"), "{e}");
    }

    #[test]
    fn direct_marker() {
        assert!(ProxyUrl::new(" None ").is_direct());
        assert!(!ProxyUrl::new("socks5://h:1").is_direct());
    }
}
