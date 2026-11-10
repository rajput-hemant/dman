//! Search, provider and tracker-list API types.
//!
//! These mirror the `dman-search` types on purpose (this crate performs no I/O
//! and does not depend on it); the daemon converts. Secrets never appear here:
//! a Torznab API key is referenced by name (`api_key_ref`) only.

use serde::{Deserialize, Serialize};

pub const SEARCH_START: &str = "search.start";
pub const SEARCH_CANCEL: &str = "search.cancel";
pub const SEARCH_RESULTS: &str = "search.results";
pub const SEARCH_RESOLVE: &str = "search.resolve";
pub const SEARCH_HISTORY: &str = "search.history";
pub const SEARCH_HISTORY_CLEAR: &str = "search.history_clear";
pub const PROVIDERS_LIST: &str = "providers.list";
pub const PROVIDERS_SET: &str = "providers.set";
pub const PROVIDERS_TEST: &str = "providers.test";
pub const PROVIDERS_IMPORT: &str = "providers.import";
pub const PROVIDERS_EXPORT: &str = "providers.export";
pub const PROVIDERS_REMOVE: &str = "providers.remove";
pub const TRACKERS_STATUS: &str = "trackers.status";

/// Most results carried by one `search.update` event / `search.results` reply.
pub const MAX_RESULTS: usize = 500;

/// `search.start`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchStartParams {
    pub query: String,
    /// Restrict to these providers (case-insensitive names); `None` = every
    /// enabled provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// Result of `search.start`: the search runs in the background.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchStarted {
    pub search_id: u64,
    /// Providers being queried (empty = none is enabled; the search is
    /// already finished). Unknown requested names are included and fail.
    #[serde(default)]
    pub providers: Vec<String>,
}

/// `search.cancel`, `search.results`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchIdParams {
    pub search_id: u64,
}

/// One normalized, de-duplicated hit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResultDto {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leechers: Option<u32>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_url: Option<String>,
    /// Page to resolve a magnet from (`search.resolve`) when `magnet` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    /// Providers that returned this result.
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// Progress of one provider within a search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderStatus {
    Running,
    Done { count: u32, elapsed_ms: u64 },
    Failed { error: String },
}

/// `search.results`: the latest merged list of one of the last searches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub search_id: u64,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `true` once every provider finished (or the search was cancelled).
    pub finished: bool,
    /// Latest status per provider, in the order they were first seen.
    pub providers: Vec<(String, ProviderStatus)>,
    /// Ranked, merged, at most [`MAX_RESULTS`].
    pub results: Vec<SearchResultDto>,
}

/// `search.resolve`: read the magnet link from a result's detail page. Give
/// either `index` (into `search.results` of `search_id`) or `detail_url`
/// together with `provider`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResolveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_url: Option<String>,
    /// Provider whose definition fetches the page. With `index` it defaults to
    /// the result's first source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// Result of `search.resolve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMagnet {
    pub magnet: String,
}

/// One entry of `search.history` (newest first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub query: String,
    /// Providers the search was restricted to (empty = all enabled).
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Unix seconds.
    pub at: i64,
    pub result_count: u32,
}

/// What a provider fetches from, without secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    /// `torznab`, `feed`, `html` or `json`.
    pub kind: String,
    pub enabled: bool,
    /// Sort order (ascending).
    pub order: i32,
    pub timeout_secs: u64,
    /// Confirmed against the live site.
    pub verified: bool,
    /// Shipped with dman (can be disabled but not removed).
    pub builtin: bool,
    /// Endpoint / URL template with query string and fragment stripped.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_min: Option<u32>,
    pub max_results: usize,
    /// Name of the secret holding the API key (Torznab); never its value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
}

/// `providers.set`: change fields of one provider; applied immediately and
/// saved to `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSetParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// `providers.test`, `providers.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderNameParams {
    pub name: String,
}

/// Result of `providers.test` (a failing provider is `ok: false`, not an RPC error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTestReport {
    pub name: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub results: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<SearchResultDto>,
    pub elapsed_ms: u64,
}

/// `providers.import`: a TOML file of `[[provider]]` tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersImportParams {
    pub toml: String,
}

/// Result of `providers.import`: names of the providers added or replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersImported {
    pub names: Vec<String>,
}

/// `providers.export`: `names` empty/absent = every custom provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersExportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
}

/// Result of `trackers.status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackersStatus {
    pub enabled: bool,
    /// Whether the list is added to new torrents and magnets.
    pub attach_to_new: bool,
    pub count: u32,
    /// Unix seconds of the last successful refresh (or cache load).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_formats() {
        let s = ProviderStatus::Done {
            count: 3,
            elapsed_ms: 40,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"state":"done","count":3,"elapsed_ms":40})
        );
        let p: SearchStartParams = serde_json::from_str(r#"{"query":"x"}"#).unwrap();
        assert_eq!(p.providers, None);
        let r: SearchResultDto = serde_json::from_str(r#"{"title":"t"}"#).unwrap();
        assert!(r.sources.is_empty());
    }
}
