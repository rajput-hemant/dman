//! Normalized result and query types.

use serde::{Deserialize, Serialize};

use crate::util;

/// One search hit, normalized across providers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub size: Option<u64>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    /// Unix timestamp (seconds).
    pub date: Option<i64>,
    pub magnet: Option<String>,
    pub torrent_url: Option<String>,
    pub detail_url: Option<String>,
    /// Lowercase hex, 40 characters.
    pub info_hash: Option<String>,
    /// Names of the providers that returned this result.
    pub sources: Vec<String>,
    pub category: Option<String>,
}

impl SearchResult {
    /// Fill derived fields: canonical info hash (from the field or the magnet),
    /// a bare magnet when only a hash is known, trimmed title.
    pub fn normalize(&mut self) {
        self.title = util::collapse_ws(&self.title);
        self.info_hash = self
            .info_hash
            .as_deref()
            .and_then(util::normalize_info_hash)
            .or_else(|| self.magnet.as_deref().and_then(util::info_hash_from_magnet));
        for f in [
            &mut self.magnet,
            &mut self.torrent_url,
            &mut self.detail_url,
            &mut self.category,
        ] {
            if f.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *f = None;
            }
        }
        if self.magnet.is_none()
            && let Some(h) = &self.info_hash
        {
            self.magnet = Some(util::build_magnet(h, &self.title, &[]));
        }
    }
}

/// A search request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    /// Free-form category (`movies`, `tv`, `audio`, `software`, `books`, `xxx`,
    /// `other`, or a numeric Torznab category id).
    pub category: Option<String>,
    /// Restrict to these provider names. `None` means every enabled provider.
    pub providers: Option<Vec<String>>,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}
