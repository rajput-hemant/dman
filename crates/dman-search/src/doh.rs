//! DNS over HTTPS (RFC 8484) as a `reqwest` resolver.
//!
//! Only what provider requests need: A and AAAA lookups, POSTed as binary DNS
//! messages. No dependency beyond the HTTP client dman already has; a full DNS
//! library would cost far more than these few functions.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use reqwest::{
    Client,
    dns::{Addrs, Name, Resolve, Resolving},
};
use url::Url;

const MAX_MESSAGE: usize = 64 * 1024;
const MAX_CACHE: Duration = Duration::from_secs(300);
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

/// Binary query for `host` (`qtype` A or AAAA), recursion desired, id 0 as RFC
/// 8484 recommends for cache friendliness.
pub fn encode_query(host: &str, qtype: u16) -> Result<Vec<u8>, String> {
    let mut m = vec![0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    let host = host.trim_end_matches('.');
    if host.is_empty() || host.len() > 253 {
        return Err(format!("bad host name length ({})", host.len()));
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() {
            return Err(format!("bad host name label `{label}`"));
        }
        m.push(u8::try_from(label.len()).unwrap_or(63));
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&1u16.to_be_bytes()); // class IN
    Ok(m)
}

fn be16(m: &[u8], at: usize) -> Result<u16, String> {
    m.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| "truncated DNS message".to_owned())
}

/// Position after a (possibly compressed) name starting at `pos`.
fn skip_name(m: &[u8], mut pos: usize) -> Result<usize, String> {
    loop {
        let len = *m.get(pos).ok_or("truncated DNS name")?;
        match len {
            0 => return Ok(pos + 1),
            l if l & 0xC0 == 0xC0 => return Ok(pos + 2),
            l if l & 0xC0 != 0 => return Err("bad DNS name".into()),
            l => pos += 1 + usize::from(l),
        }
    }
}

/// Addresses (and the smallest TTL in seconds) of a response; an error for a
/// non-zero response code.
pub fn decode_response(m: &[u8]) -> Result<(Vec<IpAddr>, u32), String> {
    if m.len() < 12 {
        return Err("truncated DNS message".into());
    }
    let rcode = m[3] & 0x0F;
    match rcode {
        0 => {}
        3 => return Err("name does not exist (NXDOMAIN)".into()),
        c => return Err(format!("DNS server answered with error code {c}")),
    }
    let (qd, an) = (be16(m, 4)?, be16(m, 6)?);
    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_name(m, pos)? + 4;
    }
    let mut out = Vec::new();
    let mut ttl = u32::MAX;
    for _ in 0..an {
        pos = skip_name(m, pos)?;
        let (ty, rdlen) = (be16(m, pos)?, usize::from(be16(m, pos + 8)?));
        let t = m
            .get(pos + 4..pos + 8)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or("truncated DNS record")?;
        let data = m
            .get(pos + 10..pos + 10 + rdlen)
            .ok_or("truncated DNS record")?;
        pos += 10 + rdlen;
        match (ty, data) {
            (TYPE_A, [a, b, c, d]) => {
                out.push(IpAddr::V4(Ipv4Addr::new(*a, *b, *c, *d)));
                ttl = ttl.min(t);
            }
            (TYPE_AAAA, d) if d.len() == 16 => {
                let mut o = [0u8; 16];
                o.copy_from_slice(d);
                out.push(IpAddr::V6(Ipv6Addr::from(o)));
                ttl = ttl.min(t);
            }
            _ => {} // CNAME and others: the chain's addresses follow
        }
    }
    Ok((out, if ttl == u32::MAX { 0 } else { ttl }))
}

struct Inner {
    client: Client,
    endpoints: Vec<Url>,
    cache: Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>,
}

/// Resolver that asks DoH endpoints, with a small in-memory cache.
pub struct DohResolver(Arc<Inner>);

impl DohResolver {
    pub fn new(user_agent: &str, endpoints: Vec<Url>) -> Result<Arc<Self>, reqwest::Error> {
        crate::engine::ensure_crypto_provider();
        let client = Client::builder()
            .user_agent(user_agent)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Arc::new(Self(Arc::new(Inner {
            client,
            endpoints,
            cache: Mutex::new(HashMap::new()),
        }))))
    }

    /// Addresses of `host` (IPv4 first), cached for the records' TTL (at most 5 minutes).
    ///
    /// # Errors
    /// A message that names the host and the first failure.
    pub async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        self.0.lookup(host).await
    }
}

impl Inner {
    async fn ask(&self, host: &str, qtype: u16) -> Result<(Vec<IpAddr>, u32), String> {
        let query = encode_query(host, qtype)?;
        let mut last = String::from("no DoH endpoint configured");
        for url in &self.endpoints {
            let res = async {
                let resp = self
                    .client
                    .post(url.clone())
                    .header("Content-Type", "application/dns-message")
                    .header("Accept", "application/dns-message")
                    .body(query.clone())
                    .send()
                    .await
                    .map_err(|e| e.without_url().to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status().as_u16()));
                }
                let body = resp
                    .bytes()
                    .await
                    .map_err(|e| e.without_url().to_string())?;
                if body.len() > MAX_MESSAGE {
                    return Err("DNS answer too large".to_owned());
                }
                decode_response(&body)
            }
            .await;
            match res {
                Ok(r) => return Ok(r),
                // The name does not exist: another server would say the same.
                Err(e) if e.contains("NXDOMAIN") => return Err(e),
                Err(e) => last = format!("{}: {e}", url.host_str().unwrap_or_default()),
            }
        }
        Err(last)
    }

    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let key = host.to_ascii_lowercase();
        if let Some((until, ips)) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            && *until > Instant::now()
        {
            return Ok(ips.clone());
        }
        let (a, aaaa) =
            futures_util::future::join(self.ask(&key, TYPE_A), self.ask(&key, TYPE_AAAA)).await;
        let mut ips = Vec::new();
        let mut ttl = u32::MAX;
        let mut errors = Vec::new();
        for r in [a, aaaa] {
            match r {
                Ok((v, t)) => {
                    ips.extend(v);
                    ttl = ttl.min(t);
                }
                Err(e) => errors.push(e),
            }
        }
        if ips.is_empty() {
            return Err(match errors.into_iter().next() {
                Some(e) => format!("DNS-over-HTTPS lookup of {host} failed: {e}"),
                None => format!("DNS-over-HTTPS lookup of {host} found no addresses"),
            });
        }
        let keep = Duration::from_secs(u64::from(ttl)).min(MAX_CACHE);
        if !keep.is_zero() {
            self.cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (Instant::now() + keep, ips.clone()));
        }
        Ok(ips)
    }
}

impl Resolve for DohResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let inner = Arc::clone(&self.0);
        Box::pin(async move {
            let ips = inner
                .lookup(name.as_str())
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_layout() {
        let q = encode_query("a.example.org", TYPE_A).unwrap();
        assert_eq!(&q[..12], &[0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&q[12..], b"\x01a\x07example\x03org\x00\x00\x01\x00\x01");
        assert!(encode_query("", TYPE_A).is_err());
        assert!(encode_query("a..b", TYPE_A).is_err());
        assert!(encode_query(&"x".repeat(64), TYPE_A).is_err());
        assert_eq!(
            &encode_query("x.", TYPE_AAAA).unwrap()[12..],
            b"\x01x\x00\x00\x1c\x00\x01"
        );
    }

    /// A response for `a.example.org` with a CNAME (compressed name) followed
    /// by two A records and an AAAA.
    fn sample() -> Vec<u8> {
        let mut m = vec![0, 0, 0x81, 0x80, 0, 1, 0, 4, 0, 0, 0, 0];
        m.extend_from_slice(b"\x01a\x07example\x03org\x00\x00\x01\x00\x01");
        // CNAME c0 0c -> (pointer to the question name)
        m.extend_from_slice(&[0xC0, 12, 0, 5, 0, 1, 0, 0, 1, 0, 0, 2, 0xC0, 12]);
        m.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 1]);
        m.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 192, 0, 2, 2]);
        let mut v6 = vec![0xC0, 12, 0, 28, 0, 1, 0, 0, 0, 90, 0, 16];
        v6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        m.extend_from_slice(&v6);
        m
    }

    #[test]
    fn decodes_addresses_and_ttl() {
        let (ips, ttl) = decode_response(&sample()).unwrap();
        assert_eq!(
            ips,
            [
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)),
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            ]
        );
        assert_eq!(ttl, 30);
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        let mut nx = sample();
        nx[3] = 0x83;
        assert!(decode_response(&nx).unwrap_err().contains("NXDOMAIN"));
        nx[3] = 0x82;
        assert!(decode_response(&nx).unwrap_err().contains("error code 2"));
        let s = sample();
        for cut in [0, 5, 11, 20, 40, s.len() - 1] {
            assert!(decode_response(&s[..cut]).is_err(), "cut at {cut}");
        }
        assert_eq!(
            decode_response(&[0, 0, 0x81, 0x80, 0, 0, 0, 0, 0, 0, 0, 0])
                .unwrap()
                .0,
            Vec::<IpAddr>::new()
        );
    }
}
