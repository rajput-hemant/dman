//! Torznab (Prowlarr / Jackett / Newznab-style) support: request building and
//! response parsing. Responses are RSS with `torznab:attr` elements; `peers`
//! counts seeders plus leechers, so leechers = peers - seeders.

use url::Url;

use crate::{error::ProviderError, result::SearchResult};

/// Parse a Torznab search response (also reports `<error .../>` documents).
pub fn parse(xml: &str) -> Result<Vec<SearchResult>, ProviderError> {
    crate::feed::parse(xml)
}

/// Map a free-form category name to Torznab category ids.
pub fn category_ids(category: &str) -> Vec<u32> {
    let c = category.trim().to_ascii_lowercase();
    if let Ok(n) = c.parse::<u32>() {
        return vec![n];
    }
    let id = match c.as_str() {
        "console" | "games" => 1000,
        "movies" | "movie" => 2000,
        "audio" | "music" => 3000,
        "pc" | "software" | "apps" => 4000,
        "tv" | "series" => 5000,
        "xxx" | "adult" => 6000,
        "books" | "ebooks" => 7000,
        "other" => 8000,
        _ => return vec![],
    };
    vec![id]
}

/// Build the search request URL. `api_key` is only ever placed in the URL
/// handed to the HTTP client, never logged.
pub fn search_url(
    endpoint: &str,
    query: &str,
    categories: &[u32],
    api_key: Option<&str>,
    limit: usize,
) -> Result<Url, ProviderError> {
    let mut url = Url::parse(endpoint)
        .map_err(|e| ProviderError::Definition(format!("bad Torznab url: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("t", "search");
        if let Some(k) = api_key {
            q.append_pair("apikey", k);
        }
        q.append_pair("q", query);
        if !categories.is_empty() {
            let cats: Vec<String> = categories.iter().map(u32::to_string).collect();
            q.append_pair("cat", &cats.join(","));
        }
        q.append_pair("limit", &limit.to_string());
        q.append_pair("extended", "1");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_building() {
        let u = search_url("http://h:9696/api", "a b", &[2000, 5000], Some("k"), 50).unwrap();
        let s = u.as_str();
        assert!(s.contains("t=search") && s.contains("apikey=k") && s.contains("q=a+b"));
        assert!(s.contains("cat=2000%2C5000") && s.contains("limit=50"));
        assert!(search_url("nope", "x", &[], None, 1).is_err());
    }

    #[test]
    fn categories() {
        assert_eq!(category_ids("Movies"), vec![2000]);
        assert_eq!(category_ids("5030"), vec![5030]);
        assert!(category_ids("zzz").is_empty());
    }
}
