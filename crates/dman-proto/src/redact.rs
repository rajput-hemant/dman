//! Helpers that keep credentials out of `Debug` output and log lines.

/// A URL that is safe to log: scheme, host, port and path are kept; user info
/// is dropped and query values are replaced by `<redacted>` (query keys stay).
/// Fragments are dropped. Anything that is not an absolute URL (for example a
/// bare file name) is returned as is, except that a `magnet:` link is reduced
/// to its scheme, because its parameters can carry tracker keys.
pub fn redact_url(url: &str) -> String {
    let url = url.trim();
    if url
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))
    {
        return "magnet:<redacted>".to_owned();
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return redact_query(url.split('#').next().unwrap_or(""));
    };
    let rest = rest.split('#').next().unwrap_or("");
    let (authority, tail) = match rest.find(['/', '?']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{host}{}", redact_query(tail))
}

fn redact_query(s: &str) -> String {
    let Some((path, query)) = s.split_once('?') else {
        return s.to_owned();
    };
    let keys: Vec<String> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| format!("{}=<redacted>", kv.split_once('=').map_or(kv, |(k, _)| k)))
        .collect();
    if keys.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", keys.join("&"))
    }
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn strips_userinfo_and_query_values() {
        assert_eq!(
            redact_url("https://user:pw@host.example:8443/a/b?token=abc&x=1#frag"),
            "https://host.example:8443/a/b?token=<redacted>&x=<redacted>"
        );
        assert_eq!(redact_url("ftp://u:p@h/file.iso"), "ftp://h/file.iso");
        assert_eq!(redact_url("https://u:p@h?k=v"), "https://h?k=<redacted>");
        assert_eq!(redact_url("file.iso"), "file.iso");
        assert_eq!(
            redact_url("magnet:?xt=urn:btih:abc&tr=http://t/announce?key=1"),
            "magnet:<redacted>"
        );
    }
}
