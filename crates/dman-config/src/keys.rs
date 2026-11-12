//! Dotted config keys (`limits.download`), the effective configuration as a
//! TOML value tree, and redaction of secrets in that tree.

pub use toml::Value;

use crate::{Config, ConfigError};

/// Shown instead of a secret value.
pub const REDACTED: &str = "<redacted>";

/// The effective configuration (file plus defaults) as a value tree.
pub fn effective_value(config: &Config) -> Result<Value, ConfigError> {
    Value::try_from(config).map_err(|e| ConfigError::Invalid(e.to_string()))
}

/// A value as printed by `dman config get`: text without quotes, numbers and
/// booleans as they are, lists inline, sections as TOML.
pub fn render_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Table(_) => render_toml(v),
        other => other.to_string(),
    }
}

/// A table as TOML text (other values as [`render_value`] would print them).
pub fn render_toml(v: &Value) -> String {
    match v {
        Value::Table(t) => toml::to_string_pretty(t).unwrap_or_default(),
        other => render_value(other),
    }
}

/// Split `limits.download` into `["limits", "download"]`; empty segments are refused.
pub fn split_key(key: &str) -> Result<Vec<&str>, ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    if key.is_empty() || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(ConfigError::BadPath(key.to_owned()));
    }
    Ok(parts)
}

/// The value at `path`; array elements are addressed by index (`schedule.0.start`).
pub fn lookup<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(root, |v, k| match v {
        Value::Table(t) => t.get(*k),
        Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    })
}

/// Every key of the effective configuration, as dotted paths: sections and
/// leaves, sorted. Lists are leaves. The defaults are included so a section
/// that is absent from the file still completes.
pub fn config_keys(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for cfg in [&Config::default(), config] {
        if let Ok(v) = effective_value(cfg) {
            collect_keys(&v, "", &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_keys(v: &Value, prefix: &str, out: &mut Vec<String>) {
    if let Value::Table(t) = v {
        for (k, child) in t {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            out.push(path.clone());
            collect_keys(child, &path, out);
        }
    }
}

/// Top-level names this version knows (sections and plain keys).
pub fn top_level_keys() -> Vec<String> {
    match effective_value(&Config::default()) {
        Ok(Value::Table(t)) => t.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Key names whose value is a credential (names that merely refer to a secret
/// in `secrets.toml`, such as `token_ref`, are not).
fn sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    if k.ends_with("_ref") || k.ends_with("_file") {
        return false;
    }
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "passkey",
        "authorization",
        "cookie",
        "credential",
    ]
    .iter()
    .any(|s| k.contains(s))
}

/// Query parameter names whose value is a credential.
fn sensitive_param(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    sensitive_key(&k)
        || matches!(
            k.as_str(),
            "key" | "pass" | "auth" | "sid" | "sig" | "signature"
        )
}

/// A string that is a URL gets its credentials masked: user info, and the
/// values of credential-like query parameters. Anything else is returned as is.
fn redact_url_string(s: &str) -> String {
    let looks_like_url = s.contains("://");
    let Some(mut u) = looks_like_url.then(|| url::Url::parse(s).ok()).flatten() else {
        return s.to_owned();
    };
    let mut changed = false;
    if !u.username().is_empty() || u.password().is_some() {
        let _ = u.set_username("redacted");
        let _ = u.set_password(Some("redacted"));
        changed = true;
    }
    let pairs: Vec<(String, String)> = u
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.iter().any(|(k, _)| sensitive_param(k)) {
        let mut q = u.query_pairs_mut();
        q.clear();
        for (k, v) in &pairs {
            q.append_pair(k, if sensitive_param(k) { "REDACTED" } else { v });
        }
        drop(q);
        changed = true;
    }
    if changed { u.to_string() } else { s.to_owned() }
}

/// A proxy value: a URL, or aria2's scheme-less `user:password@host:port`.
fn redact_proxy_value(value: &str) -> String {
    if !value.contains("://")
        && let Some((_, host)) = value.rsplit_once('@')
    {
        return format!("{REDACTED}@{host}");
    }
    redact_url_string(value)
}

/// The value given to `flag` (a credential or a proxy) with its secret masked.
fn redact_flag_value(flag: &str, value: &str) -> String {
    if sensitive_key(flag) {
        REDACTED.to_owned()
    } else if flag.contains("proxy") {
        redact_proxy_value(value)
    } else {
        redact_url_string(value)
    }
}

/// Mask the arguments of an engine command line: the value of a credential-like
/// flag (`--http-passwd=...` or `--http-passwd ...`), the user info of proxy
/// flags, and credentials in any URL.
fn redact_args(args: &mut [Value]) {
    let mut pending_flag: Option<String> = None;
    for item in args {
        let Value::String(s) = item else {
            pending_flag = None;
            continue;
        };
        if let Some(flag) = pending_flag.take()
            && !s.starts_with('-')
        {
            *s = redact_flag_value(&flag, s);
            continue;
        }
        match s.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => {
                *s = format!("{flag}={}", redact_flag_value(flag, value));
            }
            _ if s.starts_with('-') => pending_flag = Some(s.clone()),
            _ => *s = redact_url_string(s),
        }
    }
}

/// Mask every secret in a configuration tree: values of credential-like keys,
/// all values under `headers`, the webhook URL (its path usually carries a
/// token), user info and credential query parameters of URLs, and credential
/// flags in command lines. Names of secrets (`secret_ref`) stay visible.
pub fn redact_config(root: &mut Value) {
    redact_node(root, None, None);
}

fn redact_node(v: &mut Value, key: Option<&str>, parent: Option<&str>) {
    match v {
        Value::Table(t) => {
            let in_headers = key == Some("headers");
            for (k, child) in t.iter_mut() {
                if in_headers && matches!(child, Value::String(_)) {
                    *child = Value::String(REDACTED.to_owned());
                } else {
                    redact_node(child, Some(k), key);
                }
            }
        }
        Value::Array(a) => {
            if key == Some("extra_args") || key == Some("command") {
                redact_args(a);
            } else {
                for item in a {
                    redact_node(item, key, parent);
                }
            }
        }
        Value::String(s) => {
            if key.is_some_and(sensitive_key) && !s.is_empty() {
                *s = REDACTED.to_owned();
            } else if key == Some("url") && parent == Some("webhook") {
                *s = redact_webhook_url(s);
            } else if key.is_some_and(|k| k.contains("proxy")) {
                *s = redact_proxy_value(s);
            } else {
                *s = redact_url_string(s);
            }
        }
        _ => {}
    }
}

/// Scheme and host only: the rest of a webhook URL is usually the secret.
fn redact_webhook_url(s: &str) -> String {
    match url::Url::parse(s) {
        Ok(u) if u.host_str().is_some() => {
            let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
            format!(
                "{}://{}{port}/{REDACTED}",
                u.scheme(),
                u.host_str().unwrap_or("")
            )
        }
        _ => REDACTED.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Config {
        Config::from_toml_str(text, std::path::Path::new("c.toml")).unwrap()
    }

    #[test]
    fn keys_include_sections_leaves_and_profiles() {
        let keys = config_keys(&cfg("[profiles.work]\ndownload_dir = \"/w\"\n"));
        for k in [
            "limits",
            "limits.download",
            "search.dns",
            "profiles.work.download_dir",
            "profiles.local.daemon.kind",
            "default_profile",
        ] {
            assert!(keys.iter().any(|x| x == k), "missing {k}");
        }
        assert!(top_level_keys().iter().any(|k| k == "ui"));
    }

    #[test]
    fn split_and_lookup() {
        assert_eq!(split_key("a.b").unwrap(), ["a", "b"]);
        assert!(split_key("a..b").is_err());
        assert!(split_key("").is_err());
        let v = effective_value(&cfg(
            "[[schedule]]\nname = \"n\"\ndays = [\"mon\"]\nstart = \"01:00\"\nend = \"02:00\"\n",
        ))
        .unwrap();
        assert_eq!(
            lookup(&v, &["limits", "max_concurrent"]).and_then(Value::as_integer),
            Some(3)
        );
        assert_eq!(
            lookup(&v, &["schedule", "0", "name"]).and_then(Value::as_str),
            Some("n")
        );
        assert!(lookup(&v, &["limits", "nope"]).is_none());
    }

    #[test]
    fn secrets_are_masked() {
        let text = r#"
[search]
proxy = "http://user:hunter2@proxy.example:8080"
[[search.provider]]
name = "mine"
kind = "feed"
url = "http://x.example/rss?apikey=abc123&q={query}"
headers = { Authorization = "Bearer abc", X-A = "b" }
[notify.webhook]
url = "https://hooks.example/services/T000/B000/XXXX"
secret_ref = "hook"
[aria2]
extra_args = ["--all-proxy=http://u:pw@p.example:3128", "--http-passwd=zzz", "--max-tries=5", "--ftp-passwd", "yyy", "--https-proxy=me:qqq@p.example:3128", "--http-proxy", "me:vvv@p.example:3128"]
"#;
        let mut v = effective_value(&cfg(text)).unwrap();
        redact_config(&mut v);
        let out = toml::to_string(&v).unwrap();
        for leaked in [
            "hunter2", "abc123", "Bearer", "XXXX", "pw@", "zzz", "yyy", "qqq", "vvv",
        ] {
            assert!(!out.contains(leaked), "{leaked} leaked:\n{out}");
        }
        assert!(out.contains("proxy.example:8080"), "{out}");
        assert!(out.contains("secret_ref = \"hook\""), "names stay: {out}");
        assert!(out.contains("--max-tries=5"), "{out}");
        assert!(
            out.contains("q=%7Bquery%7D") || out.contains("q={query}"),
            "{out}"
        );
        assert!(out.contains("hooks.example"), "{out}");
    }

    #[test]
    fn token_refs_and_plain_values_are_kept() {
        let mut v = effective_value(&Config::default()).unwrap();
        redact_config(&mut v);
        let web = lookup(&v, &["web", "token_ref"]).and_then(Value::as_str);
        assert!(web.is_some_and(|s| s != REDACTED), "{web:?}");
        assert_eq!(
            lookup(&v, &["search", "dns"]).and_then(Value::as_str),
            Some("system")
        );
    }
}
