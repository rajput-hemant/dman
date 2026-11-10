//! Keeps URL credentials out of error messages and `Debug` output.

/// Scheme, host, port and path of `url`; user info and fragment are dropped and
/// query values are replaced by `<redacted>`. Unparseable input is not echoed.
pub(crate) fn redact_url(url: &str) -> String {
    let Ok(mut u) = url::Url::parse(url.trim()) else {
        return "(invalid url)".to_owned();
    };
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.set_fragment(None);
    let keys: Vec<String> = u.query_pairs().map(|(k, _)| k.into_owned()).collect();
    if keys.is_empty() {
        u.set_query(None);
    } else {
        let q = keys
            .iter()
            .map(|k| format!("{k}=REDACTED"))
            .collect::<Vec<_>>()
            .join("&");
        u.set_query(Some(&q));
    }
    u.to_string()
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn drops_credentials() {
        let r = redact_url("wss://u:pw@h.example:1/p?token=abc#f");
        assert_eq!(r, "wss://h.example:1/p?token=REDACTED");
        assert_eq!(redact_url("not a url pw"), "(invalid url)");
    }
}
