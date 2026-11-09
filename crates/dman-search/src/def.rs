//! Declarative provider definitions: data model, validation, import/export
//! and the built-in set.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{error::DefError, json, util};
#[cfg(feature = "engine")]
use crate::{error::ProviderError, feed, html, result::SearchResult, torznab};

const BUILTIN_TOML: &str = include_str!("builtin.toml");

fn d_timeout() -> u64 {
    15
}
fn d_max_results() -> usize {
    100
}
fn d_items() -> String {
    String::new()
}
fn d_detail_limit() -> usize {
    10
}

/// HTTP method of a JSON provider request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
}

/// HTML scraper settings. Selectors are `css` (text) or `css @attr`
/// (attribute), evaluated inside each `row`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlSpec {
    /// URL template containing `{query}`.
    pub search_url: String,
    pub row: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leechers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// strftime-like (`%Y-%m-%d %H:%M`), or `unix`, `rfc2822`, `rfc3339`,
    /// `relative`, `auto` (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_format: Option<String>,
    /// Fetch each result's detail page to read the magnet link.
    #[serde(default)]
    pub follow_detail_for_magnet: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_magnet: Option<String>,
    /// At most this many detail pages are fetched per search (the rest keep
    /// their `detail_url`; see `Searcher::resolve_magnet`).
    #[serde(default = "d_detail_limit")]
    pub detail_limit: usize,
}

/// JSON API settings. See [`crate::json`] for the field template syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonSpec {
    /// URL template; `{query}` (and `{category}`) are percent-encoded.
    pub url: String,
    #[serde(default)]
    pub method: HttpMethod,
    /// POST body template; `{query}` is JSON-escaped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// JSON pointer to the result array (`""` = the document root).
    #[serde(default = "d_items")]
    pub items: String,
    /// Optional pointer (relative to an item) to a sub-array; one result is
    /// produced per element (e.g. YTS `torrents`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leechers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// Provider kind with its kind-specific settings (`kind = "..."` in TOML).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Kind {
    Torznab {
        /// Torznab endpoint, e.g. `http://localhost:9696/api/v2.0/indexers/all/results/torznab/api`.
        url: String,
        /// Name of the secret holding the API key. The key itself is supplied
        /// at runtime (`Searcher::set_secret`) and never stored in a definition.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        categories: Vec<u32>,
    },
    Feed {
        /// RSS/Atom URL; `{query}` is optional.
        url: String,
    },
    Html(HtmlSpec),
    Json(JsonSpec),
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Torznab { .. } => "torznab",
            Self::Feed { .. } => "feed",
            Self::Html(_) => "html",
            Self::Json(_) => "json",
        }
    }
}

/// A search provider definition (one `[[provider]]` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDef {
    pub name: String,
    #[serde(flatten)]
    pub kind: Kind,
    /// Providers are off until the user enables them.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_timeout")]
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_min: Option<u32>,
    #[serde(default = "d_max_results")]
    pub max_results: usize,
    /// Sort order in lists (ascending).
    #[serde(default)]
    pub order: i32,
    /// Query used by the test action (default `test`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_query: Option<String>,
    /// `true` once someone has confirmed the definition against the live site.
    #[serde(default)]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Extra request headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

fn check_url_template(
    name: &str,
    field: &str,
    tpl: &str,
    need_query: bool,
) -> Result<(), DefError> {
    if need_query && !tpl.contains("{query}") {
        return Err(DefError::new(
            name,
            field,
            "URL template must contain `{query}`",
        ));
    }
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else {
            return Err(DefError::new(name, field, "unclosed `{`"));
        };
        let ph = &after[..j];
        if ph != "query" && ph != "category" {
            return Err(DefError::new(
                name,
                field,
                format!("unknown placeholder `{{{ph}}}` (allowed: {{query}}, {{category}})"),
            ));
        }
        rest = &after[j + 1..];
    }
    let probe = util::fill_url(tpl, "x", "x");
    let url = Url::parse(&probe)
        .map_err(|e| DefError::new(name, field, format!("not a valid URL ({e}): `{tpl}`")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DefError::new(
            name,
            field,
            "URL scheme must be http or https",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(DefError::new(name, field, "URL has no host"));
    }
    Ok(())
}

impl ProviderDef {
    /// Minimal HTML-kind constructor helpers are intentionally absent: build
    /// definitions as data. This validates everything that can be checked
    /// without network access, including compiling all CSS selectors.
    pub fn validate(&self) -> Result<(), DefError> {
        let n = self.name.as_str();
        if n.trim().is_empty() {
            return Err(DefError::new(n, "name", "must not be empty"));
        }
        if self.timeout_secs == 0 || self.timeout_secs > 600 {
            return Err(DefError::new(
                n,
                "timeout_secs",
                "must be between 1 and 600",
            ));
        }
        if self.max_results == 0 {
            return Err(DefError::new(n, "max_results", "must be at least 1"));
        }
        if self.rate_limit_per_min == Some(0) {
            return Err(DefError::new(
                n,
                "rate_limit_per_min",
                "must be at least 1 (omit for no limit)",
            ));
        }
        match &self.kind {
            Kind::Torznab {
                url, api_key_ref, ..
            } => {
                check_url_template(n, "url", url, false)?;
                if api_key_ref.as_deref().is_some_and(|r| r.trim().is_empty()) {
                    return Err(DefError::new(n, "api_key_ref", "must not be empty"));
                }
            }
            Kind::Feed { url } => check_url_template(n, "url", url, false)?,
            Kind::Html(h) => {
                check_url_template(n, "search_url", &h.search_url, true)?;
                #[cfg(feature = "engine")]
                html::Compiled::compile(h).map_err(|(f, e)| DefError::new(n, f, e))?;
                if h.follow_detail_for_magnet {
                    if h.detail_magnet.is_none() {
                        return Err(DefError::new(
                            n,
                            "detail_magnet",
                            "required when follow_detail_for_magnet = true",
                        ));
                    }
                    if h.detail_link.is_none() {
                        return Err(DefError::new(
                            n,
                            "detail_link",
                            "required when follow_detail_for_magnet = true",
                        ));
                    }
                }
                if h.magnet.is_none() && h.detail_link.is_none() {
                    return Err(DefError::new(
                        n,
                        "magnet",
                        "set `magnet` or `detail_link` (and detail_magnet) so results can be downloaded",
                    ));
                }
            }
            Kind::Json(j) => {
                check_url_template(n, "url", &j.url, false)?;
                if !j.url.contains("{query}")
                    && !j.body.as_deref().is_some_and(|b| b.contains("{query}"))
                {
                    return Err(DefError::new(
                        n,
                        "url",
                        "`{query}` must appear in the url or the body",
                    ));
                }
                match (&j.method, &j.body) {
                    (HttpMethod::Post, None) => {
                        return Err(DefError::new(n, "body", "POST requires a body template"));
                    }
                    (HttpMethod::Get, Some(_)) => {
                        return Err(DefError::new(
                            n,
                            "body",
                            "body is only allowed with method = \"post\"",
                        ));
                    }
                    (HttpMethod::Post, Some(b)) => {
                        let probe = util::fill_json_body(b, "x", "x");
                        serde_json::from_str::<serde_json::Value>(&probe).map_err(|e| {
                            DefError::new(
                                n,
                                "body",
                                format!("not valid JSON after filling placeholders: {e}"),
                            )
                        })?;
                    }
                    _ => {}
                }
                if !(j.items.is_empty() || j.items.starts_with('/')) {
                    return Err(DefError::new(
                        n,
                        "items",
                        "must be a JSON pointer (empty or starting with `/`)",
                    ));
                }
                if let Some(e) = &j.expand
                    && !e.starts_with('/')
                {
                    return Err(DefError::new(
                        n,
                        "expand",
                        "must be a JSON pointer starting with `/`",
                    ));
                }
                let fields = [
                    ("title", Some(&j.title)),
                    ("size", j.size.as_ref()),
                    ("seeders", j.seeders.as_ref()),
                    ("leechers", j.leechers.as_ref()),
                    ("date", j.date.as_ref()),
                    ("info_hash", j.info_hash.as_ref()),
                    ("magnet", j.magnet.as_ref()),
                    ("torrent_url", j.torrent_url.as_ref()),
                    ("detail_url", j.detail_url.as_ref()),
                    ("category", j.category.as_ref()),
                ];
                for (f, v) in fields {
                    if let Some(v) = v {
                        json::check_template(v).map_err(|e| DefError::new(n, f, e))?;
                    }
                }
                if j.info_hash.is_none() && j.magnet.is_none() && j.torrent_url.is_none() {
                    return Err(DefError::new(
                        n,
                        "info_hash",
                        "map at least one of info_hash, magnet or torrent_url",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The query string used by the test action.
    pub fn probe_query(&self) -> &str {
        self.test_query.as_deref().unwrap_or("test")
    }
}

/// Parse a response body with the definition's parser (no network).
#[cfg(feature = "engine")]
/// `strict` turns "found nothing" situations into precise errors.
pub fn parse_response(
    def: &ProviderDef,
    body: &str,
    strict: bool,
) -> Result<Vec<SearchResult>, ProviderError> {
    match &def.kind {
        Kind::Torznab { .. } => torznab::parse(body),
        Kind::Feed { .. } => feed::parse(body),
        Kind::Html(h) => {
            let base = Url::parse(&util::fill_url(&h.search_url, "x", ""))
                .map_err(|e| ProviderError::Definition(e.to_string()))?;
            html::parse(h, body, &base, strict)
        }
        Kind::Json(j) => json::parse(j, body, strict),
    }
}

// ------------------------------------------------------- import / export

#[derive(Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "provider")]
    providers: Vec<ProviderDef>,
}

/// Serialize definitions as a TOML file of `[[provider]]` tables.
pub fn export(defs: &[ProviderDef]) -> String {
    let file = File {
        providers: defs.to_vec(),
    };
    toml::to_string(&file).unwrap_or_default()
}

/// Parse and validate a TOML file of `[[provider]]` tables.
pub fn import(text: &str) -> Result<Vec<ProviderDef>, DefError> {
    let file: File = toml::from_str(text)
        .map_err(|e| DefError::new("(file)", "toml", e.to_string().trim().to_string()))?;
    let mut seen = std::collections::HashSet::new();
    for d in &file.providers {
        d.validate()?;
        if !seen.insert(d.name.to_lowercase()) {
            return Err(DefError::new(&d.name, "name", "duplicate provider name"));
        }
    }
    Ok(file.providers)
}

/// The built-in provider set (all disabled, see `builtin.toml`).
pub fn builtin_providers() -> Vec<ProviderDef> {
    // The file is embedded and covered by tests; a failure here is a build bug.
    import(BUILTIN_TOML).expect("embedded builtin.toml is valid")
}
