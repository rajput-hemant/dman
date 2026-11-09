//! JSON API providers: JSON-pointer item lists plus field templates.
//!
//! A field value is one of:
//! * a JSON pointer (`/name`) resolved against the current element;
//! * a parent pointer (`^/title`) resolved against the enclosing item when
//!   `expand` is used (one result per element of a sub-array);
//! * a template with `{pointer}` / `{pointer|enc}` parts
//!   (`magnet:?xt=urn:btih:{/hash}&dn={^/title|enc}`); filters chain with
//!   `|`: `enc` percent-encodes, `html` decodes HTML entities (apibay
//!   returns names like `Linux &amp; Scripting`), e.g. `{/name|html|enc}`;
//! * otherwise a literal string.
//!
//! A template whose pointers are missing or null yields no value.

use serde_json::Value;

use crate::{def::JsonSpec, error::ProviderError, result::SearchResult, util};

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn pointer<'a>(expr: &str, sub: &'a Value, parent: &'a Value) -> Option<&'a Value> {
    match expr.strip_prefix('^') {
        Some(rest) => parent.pointer(rest),
        None => sub.pointer(expr),
    }
}

/// Check a field template's syntax (balanced braces, pointer prefixes).
pub fn check_template(tpl: &str) -> Result<(), String> {
    let is_ptr = |e: &str| e.starts_with('/') || e.starts_with("^/") || e == "^";
    if !tpl.contains('{') {
        if tpl.contains('}') {
            return Err(format!("unbalanced `}}` in `{tpl}`"));
        }
        return Ok(());
    }
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let j = after
            .find('}')
            .ok_or_else(|| format!("unclosed `{{` in `{tpl}`"))?;
        let inner = &after[..j];
        let mut parts = inner.split('|');
        let expr = parts.next().unwrap_or("");
        if !is_ptr(expr) {
            return Err(format!(
                "`{{{inner}}}` must contain a JSON pointer starting with `/` or `^/`"
            ));
        }
        for filter in parts {
            if !matches!(filter, "enc" | "html") {
                return Err(format!(
                    "unknown filter `{filter}` (only `enc` and `html` exist)"
                ));
            }
        }
        rest = &after[j + 1..];
    }
    Ok(())
}

/// Evaluate a field template against the current element and its parent.
pub fn eval(tpl: &str, sub: &Value, parent: &Value) -> Option<String> {
    if !tpl.contains('{') {
        return if tpl.starts_with('/') || tpl.starts_with("^/") {
            scalar(pointer(tpl, sub, parent)?)
        } else {
            Some(tpl.to_string()).filter(|s| !s.is_empty())
        };
    }
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let j = after.find('}')?;
        let inner = &after[..j];
        let mut parts = inner.split('|');
        let expr = parts.next().unwrap_or("");
        let mut v = scalar(pointer(expr, sub, parent)?)?;
        for filter in parts {
            v = match filter {
                "enc" => util::percent_encode(&v),
                "html" => util::decode_html_entities(&v),
                _ => v,
            };
        }
        out.push_str(&v);
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Parse a JSON API response. With `strict`, a missing `items` pointer is an error.
pub fn parse(
    spec: &JsonSpec,
    body: &str,
    strict: bool,
) -> Result<Vec<SearchResult>, ProviderError> {
    let root: Value = serde_json::from_str(body).map_err(|e| {
        let head: String = body.chars().take(60).collect();
        ProviderError::Parse(format!(
            "invalid JSON ({e}); response starts with `{}`",
            head.trim()
        ))
    })?;
    let list = match root.pointer(&spec.items) {
        Some(Value::Array(a)) => a,
        Some(Value::Null) => return Ok(Vec::new()),
        Some(_) => {
            return Err(ProviderError::Parse(format!(
                "items pointer `{}` does not point to an array",
                spec.items
            )));
        }
        None if strict => {
            let keys = match &root {
                Value::Object(m) => m.keys().take(8).cloned().collect::<Vec<_>>().join(", "),
                Value::Array(_) => "(top-level array)".to_string(),
                _ => "(scalar)".to_string(),
            };
            return Err(ProviderError::Parse(format!(
                "items pointer `{}` not found in response (top-level keys: {keys})",
                spec.items
            )));
        }
        None => return Ok(Vec::new()),
    };

    let empty = Vec::new();
    let mut out = Vec::new();
    let mut titled = 0usize;
    for item in list {
        let subs: Vec<&Value> = match &spec.expand {
            Some(p) => match item.pointer(p) {
                Some(Value::Array(a)) => a.iter().collect(),
                _ => empty.iter().collect(),
            },
            None => vec![item],
        };
        for sub in subs {
            let f = |t: &Option<String>| t.as_deref().and_then(|t| eval(t, sub, item));
            let Some(title) = eval(&spec.title, sub, item) else {
                continue;
            };
            titled += 1;
            let mut r = SearchResult {
                title,
                size: f(&spec.size).and_then(|s| util::parse_size(&s)),
                seeders: f(&spec.seeders).and_then(|s| util::parse_count(&s)),
                leechers: f(&spec.leechers).and_then(|s| util::parse_count(&s)),
                date: f(&spec.date).and_then(|s| util::parse_date(&s)),
                info_hash: f(&spec.info_hash),
                magnet: f(&spec.magnet),
                torrent_url: f(&spec.torrent_url),
                detail_url: f(&spec.detail_url),
                category: f(&spec.category),
                ..SearchResult::default()
            };
            // apibay answers "no results" with a single all-zero placeholder.
            if r.info_hash
                .as_deref()
                .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b == b'0'))
            {
                continue;
            }
            r.size = r.size.filter(|&s| s > 0);
            out.push(r);
        }
    }
    if !list.is_empty() && titled == 0 {
        return Err(ProviderError::Parse(format!(
            "title mapping `{}` produced no value for any of the {} items",
            spec.title,
            list.len()
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn templates() {
        let sub = json!({"hash": "AB", "q": "1080p", "n": 5, "z": null});
        let parent = json!({"title": "A Movie & Co"});
        assert_eq!(eval("/hash", &sub, &parent).as_deref(), Some("AB"));
        assert_eq!(eval("/n", &sub, &parent).as_deref(), Some("5"));
        assert_eq!(
            eval("^/title", &sub, &parent).as_deref(),
            Some("A Movie & Co")
        );
        assert_eq!(
            eval("x:{/hash}:{^/title|enc}", &sub, &parent).as_deref(),
            Some("x:AB:A%20Movie%20%26%20Co")
        );
        assert_eq!(eval("{/z}", &sub, &parent), None);
        assert_eq!(eval("/missing", &sub, &parent), None);
        assert_eq!(eval("movies", &sub, &parent).as_deref(), Some("movies"));
    }

    #[test]
    fn template_syntax() {
        assert!(check_template("a{/b}c").is_ok());
        assert!(check_template("a{b}c").is_err());
        assert!(check_template("a{/b").is_err());
        assert!(check_template("a{/b|zip}").is_err());
        assert!(check_template("a}").is_err());
    }
}
