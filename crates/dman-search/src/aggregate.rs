//! Merging, deduplication and ranking of results from many providers.

use std::{cmp::Ordering, collections::HashMap};

#[cfg(feature = "engine")]
use crate::engine::ProviderEvent;
use crate::result::SearchResult;

/// Sort keys for result lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Title,
    Size,
    Seeders,
    Leechers,
    Date,
    Source,
}

fn normalize_title(t: &str) -> String {
    t.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn key(r: &SearchResult) -> String {
    match &r.info_hash {
        Some(h) => format!("h:{h}"),
        None => format!("t:{}|{}", normalize_title(&r.title), r.size.unwrap_or(0)),
    }
}

fn merge_into(dst: &mut SearchResult, src: SearchResult) {
    fn pick<T>(a: &mut Option<T>, b: Option<T>) {
        if a.is_none() {
            *a = b;
        }
    }
    dst.seeders = dst.seeders.max(src.seeders);
    dst.leechers = dst.leechers.max(src.leechers);
    pick(&mut dst.size, src.size);
    pick(&mut dst.date, src.date);
    pick(&mut dst.magnet, src.magnet);
    pick(&mut dst.torrent_url, src.torrent_url);
    pick(&mut dst.detail_url, src.detail_url);
    pick(&mut dst.info_hash, src.info_hash);
    pick(&mut dst.category, src.category);
    if dst.title.is_empty() {
        dst.title = src.title;
    }
    for s in src.sources {
        if !dst.sources.contains(&s) {
            dst.sources.push(s);
        }
    }
}

/// Incremental aggregator: feed batches as providers answer, read the ranked
/// list at any time.
#[derive(Debug, Default)]
pub struct Aggregator {
    items: Vec<SearchResult>,
    index: HashMap<String, usize>,
}

impl Aggregator {
    /// Add a batch of results (normalizing them first).
    pub fn add(&mut self, batch: Vec<SearchResult>) {
        for mut r in batch {
            r.normalize();
            if r.title.is_empty() {
                continue;
            }
            let k = key(&r);
            match self.index.get(&k) {
                Some(&i) => merge_into(&mut self.items[i], r),
                None => {
                    self.index.insert(k, self.items.len());
                    self.items.push(r);
                }
            }
        }
    }

    /// Number of distinct results so far.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Ranked copy: seeders descending (unknown last), then leechers, then title.
    pub fn results(&self) -> Vec<SearchResult> {
        let mut v = self.items.clone();
        rank(&mut v);
        v
    }
}

fn rank(v: &mut [SearchResult]) {
    v.sort_by(|a, b| {
        cmp_opt_desc(a.seeders, b.seeders)
            .then_with(|| cmp_opt_desc(a.leechers, b.leechers))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

/// Descending, `None` always last.
fn cmp_opt_desc<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_opt_asc<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Merge the `Results` events of a search into a deduplicated, ranked list.
#[cfg(feature = "engine")]
pub fn aggregate(events: impl IntoIterator<Item = ProviderEvent>) -> Vec<SearchResult> {
    let mut agg = Aggregator::default();
    for e in events {
        if let ProviderEvent::Results { results, .. } = e {
            agg.add(results);
        }
    }
    agg.results()
}

/// Sort in place. Results with an unknown value for `key` are always last.
/// Ties fall back to the default ranking.
pub fn sort_by(results: &mut [SearchResult], key: SortKey, ascending: bool) {
    rank(results);
    results.sort_by(|a, b| {
        let primary = |x: &SearchResult, y: &SearchResult| match key {
            SortKey::Title => (
                Some(x.title.to_lowercase()).cmp(&Some(y.title.to_lowercase())),
                false,
            ),
            SortKey::Size => (
                cmp_opt_asc(x.size, y.size),
                x.size.is_none() || y.size.is_none(),
            ),
            SortKey::Seeders => (
                cmp_opt_asc(x.seeders, y.seeders),
                x.seeders.is_none() || y.seeders.is_none(),
            ),
            SortKey::Leechers => (
                cmp_opt_asc(x.leechers, y.leechers),
                x.leechers.is_none() || y.leechers.is_none(),
            ),
            SortKey::Date => (
                cmp_opt_asc(x.date, y.date),
                x.date.is_none() || y.date.is_none(),
            ),
            SortKey::Source => (
                cmp_opt_asc(x.sources.first().cloned(), y.sources.first().cloned()),
                x.sources.is_empty() || y.sources.is_empty(),
            ),
        };
        let (ord, has_none) = primary(a, b);
        if has_none || ord == Ordering::Equal {
            // unknowns last regardless of direction; equal keeps ranking (stable sort)
            return ord;
        }
        if ascending { ord } else { ord.reverse() }
    });
}

#[cfg(all(test, feature = "engine"))]
mod tests {
    use super::*;

    fn r(title: &str, hash: Option<&str>, seeders: Option<u32>, src: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            info_hash: hash.map(str::to_string),
            seeders,
            sources: vec![src.into()],
            ..SearchResult::default()
        }
    }

    const H1: &str = "0123456789abcdef0123456789abcdef01234567";
    const H2: &str = "1123456789abcdef0123456789abcdef01234567";

    #[test]
    fn dedupe_by_hash_merges() {
        let mut a = r("Ubuntu 24.04", Some(H1), Some(10), "A");
        a.size = Some(5);
        let mut b = r("ubuntu 24.04 iso", Some(&H1.to_uppercase()), Some(50), "B");
        b.leechers = Some(3);
        b.date = Some(99);
        b.size = Some(6);
        let c = r("Other", Some(H2), Some(20), "A");
        let out = aggregate([
            ProviderEvent::Results {
                provider: "A".into(),
                results: vec![a, c],
            },
            ProviderEvent::Failed {
                provider: "X".into(),
                error: "boom".into(),
            },
            ProviderEvent::Results {
                provider: "B".into(),
                results: vec![b],
            },
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].seeders, Some(50));
        assert_eq!(out[0].title, "Ubuntu 24.04"); // first title wins
        assert_eq!(out[0].sources, vec!["A", "B"]);
        assert_eq!(out[0].size, Some(5));
        assert_eq!(out[0].leechers, Some(3));
        assert_eq!(out[0].date, Some(99));
        assert_eq!(out[1].title, "Other");
    }

    #[test]
    fn dedupe_without_hash_by_title_and_size() {
        let mut a = r("My  Movie (2020)", None, Some(1), "A");
        a.size = Some(100);
        let mut b = r("my movie 2020", None, Some(9), "B");
        b.size = Some(100);
        let mut c = r("my movie 2020", None, Some(2), "C");
        c.size = Some(200);
        let mut agg = Aggregator::default();
        agg.add(vec![a, b, c]);
        let out = agg.results();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].seeders, Some(9));
        assert_eq!(out[0].sources, vec!["A", "B"]);
    }

    #[test]
    fn magnet_gives_hash_for_dedupe() {
        let mut a = r("x", None, Some(1), "A");
        a.magnet = Some(format!("magnet:?xt=urn:btih:{H1}"));
        let b = r("x", Some(H1), Some(2), "B");
        let out = aggregate([ProviderEvent::Results {
            provider: "A".into(),
            results: vec![a, b],
        }]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn ranking_and_sorting() {
        let mut v = vec![
            r("b", Some(H1), None, "Z"),
            r("a", Some(H2), Some(5), "Y"),
            r(
                "c",
                Some("2123456789abcdef0123456789abcdef01234567"),
                Some(7),
                "X",
            ),
        ];
        rank(&mut v);
        assert_eq!(
            v.iter().map(|x| x.title.as_str()).collect::<String>(),
            "cab"
        );
        sort_by(&mut v, SortKey::Title, true);
        assert_eq!(
            v.iter().map(|x| x.title.as_str()).collect::<String>(),
            "abc"
        );
        sort_by(&mut v, SortKey::Title, false);
        assert_eq!(
            v.iter().map(|x| x.title.as_str()).collect::<String>(),
            "cba"
        );
        sort_by(&mut v, SortKey::Seeders, true);
        assert_eq!(
            v.iter().map(|x| x.title.as_str()).collect::<String>(),
            "acb"
        ); // None last
        sort_by(&mut v, SortKey::Seeders, false);
        assert_eq!(
            v.iter().map(|x| x.title.as_str()).collect::<String>(),
            "cab"
        );
        sort_by(&mut v, SortKey::Source, true);
        assert_eq!(
            v.iter().map(|x| x.title.as_str()).collect::<String>(),
            "cab"
        );
    }
}
