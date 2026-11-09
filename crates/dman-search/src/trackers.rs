//! Public tracker lists (one URL per line), attached to new torrents.

use url::Url;

/// Default public tracker list (ngosang/trackerslist, "best" set). Fetching it
/// is opt-in: nothing in dman downloads it unless the user enables the
/// feature; callers pass this (or their own URL) to [`fetch_tracker_list`].
pub const DEFAULT_TRACKER_LIST_URL: &str =
    "https://raw.githubusercontent.com/ngosang/trackerslist/master/trackers_best.txt";

/// Parse a tracker list: one URL per line; blank lines and `#` comments are
/// skipped; only http, https, udp and wss URLs with a host are kept;
/// duplicates are removed (order preserved).
pub fn parse_tracker_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim().trim_start_matches('\u{feff}');
        if l.is_empty() || l.starts_with('#') || l.starts_with("//") {
            continue;
        }
        let Ok(u) = Url::parse(l) else { continue };
        if matches!(u.scheme(), "http" | "https" | "udp" | "wss")
            && u.host_str().is_some_and(|h| !h.is_empty())
        {
            let s = l.to_string();
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// Download and parse a tracker list with a default searcher. Use
/// [`Searcher::fetch_tracker_list`] to go through the configured proxy.
#[cfg(feature = "engine")]
pub async fn fetch_tracker_list(url: &str) -> Result<Vec<String>, crate::ProviderError> {
    crate::Searcher::new(crate::SearcherConfig::default())?
        .fetch_tracker_list(url)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let t = "# list\nudp://tracker.a:1337/announce\n\n  https://b.example/announce  \nftp://nope/x\nnot a url\nudp://tracker.a:1337/announce\nwss://c.example:443/ws\n";
        assert_eq!(
            parse_tracker_list(t),
            vec![
                "udp://tracker.a:1337/announce",
                "https://b.example/announce",
                "wss://c.example:443/ws"
            ]
        );
    }
}
