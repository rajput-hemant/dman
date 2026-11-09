//! The `[web]` section: the browser UI served by the daemon.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::tls::TlsFiles;

/// Browser UI settings. Off by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Web {
    /// Start the web UI together with the daemon.
    pub enabled: bool,
    /// Address to bind (an IP address). Anything but loopback exposes the UI.
    pub bind: String,
    /// TCP port, separate from the API port.
    pub port: u16,
    /// Mount point behind a reverse proxy, e.g. `"/"` or `"/dman"`.
    pub base_path: String,
    /// Name of the secrets.toml entry holding the access token. Only used when the UI
    /// asks for a login: beyond loopback, or on loopback with `require_login`.
    pub token_ref: String,
    /// Allow serving a non-loopback address without any access token.
    pub allow_unauthenticated: bool,
    /// Ask for the access token even on loopback, e.g. when a reverse proxy on this
    /// machine makes the UI reachable from elsewhere. Off: loopback opens directly.
    pub require_login: bool,
    /// Extra host names accepted in the `Host` header when no login is asked for
    /// (DNS-rebinding defence). Loopback names and IP addresses are always accepted.
    pub allowed_hosts: Vec<String>,
    /// Shown in the header and tab title.
    pub title: String,
    /// Serve https with this certificate; plain http when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsFiles>,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1".into(),
            port: 6811,
            base_path: "/".into(),
            token_ref: "web".into(),
            allow_unauthenticated: false,
            require_login: false,
            allowed_hosts: Vec::new(),
            title: "dman".into(),
            tls: None,
        }
    }
}

impl Web {
    /// The parsed bind address.
    pub fn bind_ip(&self) -> Option<IpAddr> {
        parse_bind(&self.bind)
    }

    /// Whether the bind address is loopback (an unparsable one counts as exposed).
    pub fn is_loopback(&self) -> bool {
        self.bind_ip().is_some_and(|ip| ip.is_loopback())
    }

    /// Loopback bind without `require_login`: the UI opens directly, no token is used
    /// (decision U3). Same-origin, CSRF and `Host` checks still apply.
    pub fn loopback_without_login(&self) -> bool {
        self.is_loopback() && !self.require_login
    }

    /// `http(s)://bind:port{base_path}` (IPv6 in brackets; base path without a trailing slash,
    /// `/` for the root).
    pub fn url(&self) -> String {
        let host = match self.bind_ip() {
            Some(IpAddr::V6(v6)) => format!("[{v6}]"),
            Some(v4) => v4.to_string(),
            None => self.bind.clone(),
        };
        let t = self.base_path.trim_matches('/');
        let base = if t.is_empty() {
            "/".to_owned()
        } else {
            format!("/{t}")
        };
        let scheme = if self.tls.is_some() { "https" } else { "http" };
        format!("{scheme}://{host}:{}{base}", self.port)
    }

    /// Check the values.
    pub fn validate(&self) -> Result<(), String> {
        if self.bind_ip().is_none() {
            return Err(format!(
                "web.bind `{}` is not a valid IP address (e.g. 127.0.0.1, 0.0.0.0, ::1)",
                self.bind
            ));
        }
        if self.port == 0 {
            return Err("web.port must be non-zero".into());
        }
        validate_base_path(&self.base_path)?;
        if let Some(t) = &self.tls {
            t.validate("web.tls")?;
        }
        if self.token_ref.trim().is_empty() {
            return Err("web.token_ref must not be empty".into());
        }
        if let Some(h) = self.allowed_hosts.iter().find(|h| {
            h.is_empty()
                || !h
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c))
        }) {
            return Err(format!(
                "web.allowed_hosts entry `{h}` is not a host name (letters, digits, `-` and `.`)"
            ));
        }
        Ok(())
    }
}

/// Parses an IP address, accepting IPv6 in brackets.
pub fn parse_bind(s: &str) -> Option<IpAddr> {
    let t = s.trim();
    let t = t
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(t);
    t.parse().ok()
}

/// A base path starts with `/` and has no whitespace, `?`, `#` or `..`.
pub fn validate_base_path(p: &str) -> Result<(), String> {
    let bad = |m: &str| Err(format!("web.base_path `{p}` {m}"));
    if !p.starts_with('/') {
        return bad("must start with `/`");
    }
    if p.chars().any(|c| c.is_whitespace() || c.is_control())
        || p.contains(['?', '#', '\\'])
        || p.split('/').any(|s| s == "..")
    {
        return bad("contains forbidden characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_valid_and_loopback() {
        let w = Web::default();
        w.validate().unwrap();
        assert!(!w.enabled && w.is_loopback() && w.loopback_without_login());
        assert_eq!(
            (w.port, w.base_path.as_str(), w.token_ref.as_str()),
            (6811, "/", "web")
        );
    }

    #[test]
    fn tls_switches_url_scheme() {
        let w = Web {
            tls: Some(TlsFiles {
                cert: "c".into(),
                key: "k".into(),
            }),
            ..Web::default()
        };
        assert_eq!(w.url(), "https://127.0.0.1:6811/");
        assert!(w.validate().is_err(), "missing files");
    }

    #[test]
    fn validation() {
        let mut w = Web {
            bind: "0.0.0.0".into(),
            ..Web::default()
        };
        w.validate().unwrap();
        assert!(!w.is_loopback() && !w.loopback_without_login());
        assert_eq!(w.url(), "http://0.0.0.0:6811/");
        w.base_path = "/dman/".into();
        assert_eq!(w.url(), "http://0.0.0.0:6811/dman");
        w.base_path = "/".into();
        w.bind = "[::1]".into();
        assert!(w.is_loopback());
        assert_eq!(w.url(), "http://[::1]:6811/");
        w.bind = "localhost".into();
        assert!(w.validate().is_err());
        let mut w = Web {
            port: 0,
            ..Web::default()
        };
        assert!(w.validate().is_err());
        for p in ["dman", "/a b", "/a?x", "/../x", ""] {
            w.port = 1;
            w.base_path = p.into();
            assert!(w.validate().is_err(), "{p}");
        }
        w.base_path = "/dman".into();
        w.validate().unwrap();
        w.allowed_hosts = vec!["nas.local".into()];
        w.validate().unwrap();
        w.allowed_hosts = vec!["evil.example:80".into()];
        assert!(w.validate().is_err());
        let w = Web {
            require_login: true,
            ..Web::default()
        };
        assert!(w.is_loopback() && !w.loopback_without_login());
    }
}
