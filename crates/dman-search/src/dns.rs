//! `[search] dns`: how provider and tracker-list requests resolve host names.
//!
//! Many country-level blocks only poison the system resolver. With
//! `doh:...` dman asks a DNS-over-HTTPS server instead (the lookup travels as
//! an ordinary HTTPS request). aria2 does not use this; it has its own
//! `async-dns-server` option.

use std::fmt;

use url::Url;

/// Built-in DNS-over-HTTPS servers, tried in order. The addresses are literal
/// so that no system lookup is needed to reach them (both certificates carry
/// IP subject names).
pub const CLOUDFLARE: &[&str] = &["https://1.1.1.1/dns-query", "https://1.0.0.1/dns-query"];
pub const QUAD9: &[&str] = &[
    "https://9.9.9.9/dns-query",
    "https://149.112.112.112/dns-query",
];

/// Resolver choice for provider and tracker-list requests.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum DnsMode {
    /// The operating system's resolver.
    #[default]
    System,
    /// DNS over HTTPS (RFC 8484) against these endpoints, tried in order.
    Doh {
        /// The setting as written (`doh:cloudflare`, ...), for display.
        label: String,
        endpoints: Vec<Url>,
    },
}

impl DnsMode {
    /// Parse `system`, `doh:cloudflare`, `doh:quad9` or `doh:https://host/path`.
    ///
    /// # Errors
    /// A message naming the accepted forms.
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let lower = t.to_ascii_lowercase();
        let forms =
            "expected `system`, `doh:cloudflare`, `doh:quad9` or `doh:https://host/dns-query`";
        match lower.as_str() {
            "" | "system" => return Ok(Self::System),
            "doh:cloudflare" => return Ok(Self::doh(&lower, CLOUDFLARE)),
            "doh:quad9" => return Ok(Self::doh(&lower, QUAD9)),
            _ => {}
        }
        let Some(url) = t
            .get(..4)
            .filter(|p| p.eq_ignore_ascii_case("doh:"))
            .map(|_| &t[4..])
        else {
            return Err(format!("unknown dns setting `{t}`: {forms}"));
        };
        match Url::parse(url) {
            Ok(u)
                if u.scheme() == "https"
                    && u.host_str().is_some_and(|h| !h.is_empty())
                    && u.username().is_empty()
                    && u.password().is_none() =>
            {
                Ok(Self::Doh {
                    label: format!("doh:{}", host_label(&u)),
                    endpoints: vec![u],
                })
            }
            _ => Err(format!(
                "bad DNS-over-HTTPS url in dns setting: {forms} (https only, no credentials)"
            )),
        }
    }

    fn doh(label: &str, urls: &[&str]) -> Self {
        Self::Doh {
            label: label.to_owned(),
            endpoints: urls.iter().filter_map(|u| Url::parse(u).ok()).collect(),
        }
    }

    /// Whether lookups go through DNS over HTTPS.
    pub fn is_doh(&self) -> bool {
        matches!(self, Self::Doh { .. })
    }
}

fn host_label(u: &Url) -> String {
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!(
        "https://{}{port}{}",
        u.host_str().unwrap_or_default(),
        u.path()
    )
}

impl fmt::Debug for DnsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => f.write_str("System"),
            Self::Doh { label, .. } => write!(f, "Doh({label})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_forms() {
        assert_eq!(DnsMode::parse("system").unwrap(), DnsMode::System);
        assert_eq!(DnsMode::parse("").unwrap(), DnsMode::System);
        let DnsMode::Doh { endpoints, label } = DnsMode::parse("DoH:Cloudflare").unwrap() else {
            panic!()
        };
        assert_eq!(label, "doh:cloudflare");
        assert_eq!(endpoints.len(), 2);
        let DnsMode::Doh { endpoints, .. } = DnsMode::parse("doh:quad9").unwrap() else {
            panic!()
        };
        assert_eq!(endpoints[0].as_str(), "https://9.9.9.9/dns-query");
        let DnsMode::Doh { endpoints, label } =
            DnsMode::parse("doh:https://dns.example:8443/q").unwrap()
        else {
            panic!()
        };
        assert_eq!(endpoints[0].as_str(), "https://dns.example:8443/q");
        assert_eq!(label, "doh:https://dns.example:8443/q");
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "doh",
            "doh:",
            "doh:nonsense",
            "doh:http://dns.example/q",
            "doh:https://u:p@dns.example/q",
            "tls:1.1.1.1",
            "8.8.8.8",
        ] {
            assert!(DnsMode::parse(bad).is_err(), "{bad}");
        }
    }
}
