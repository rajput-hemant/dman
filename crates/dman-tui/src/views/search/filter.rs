//! Sorting and filtering of search results (pure functions over the list the
//! daemon sent; the daemon's own ranking is only the starting point).

use std::cmp::Ordering;
use std::str::FromStr;

use bytesize::ByteSize;
use dman_proto::search::SearchResultDto;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::format::size;

/// What the results are ordered by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    /// Most seeders first (the default).
    #[default]
    Seeders,
    /// Most leechers first.
    Leechers,
    /// Biggest first.
    Size,
    /// Newest first.
    Date,
    /// A to Z.
    Title,
    /// By the first provider that returned it, A to Z.
    Source,
}

impl SortKey {
    /// Cycle order.
    pub const ALL: [SortKey; 6] = [
        SortKey::Seeders,
        SortKey::Leechers,
        SortKey::Size,
        SortKey::Date,
        SortKey::Title,
        SortKey::Source,
    ];

    /// The next key in the cycle.
    pub fn next(self) -> SortKey {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Name shown in the filter line.
    pub const fn label(self) -> &'static str {
        match self {
            SortKey::Seeders => "seeders",
            SortKey::Leechers => "leechers",
            SortKey::Size => "size",
            SortKey::Date => "date",
            SortKey::Title => "title",
            SortKey::Source => "source",
        }
    }

    /// Natural order: the "best" result first (missing values last).
    pub fn compare(self, a: &SearchResultDto, b: &SearchResultDto) -> Ordering {
        let primary = match self {
            SortKey::Seeders => b.seeders.cmp(&a.seeders),
            SortKey::Leechers => b.leechers.cmp(&a.leechers),
            SortKey::Size => b.size.cmp(&a.size),
            SortKey::Date => b.date.cmp(&a.date),
            SortKey::Title => lower(&a.title).cmp(&lower(&b.title)),
            SortKey::Source => first_source(a).cmp(&first_source(b)),
        };
        primary
            .then_with(|| b.seeders.cmp(&a.seeders))
            .then_with(|| lower(&a.title).cmp(&lower(&b.title)))
    }
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

fn first_source(r: &SearchResultDto) -> String {
    r.sources.first().map(|s| lower(s)).unwrap_or_default()
}

/// The user's result filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    /// Hide results with fewer seeders (unknown counts as 0).
    pub min_seeders: u32,
    /// Smallest size in bytes (results of unknown size are hidden when set).
    pub size_min: Option<u64>,
    /// Largest size in bytes.
    pub size_max: Option<u64>,
    /// Provider name (substring, case-insensitive) that must be a source.
    pub source: String,
    /// Fuzzy title filter.
    pub title: String,
}

impl Filters {
    /// No filter is set.
    pub fn is_empty(&self) -> bool {
        *self == Filters::default()
    }

    /// `size 100 MiB–4 GiB` style text, `None` without a size filter.
    pub fn size_label(&self) -> Option<String> {
        match (self.size_min, self.size_max) {
            (None, None) => None,
            (Some(a), None) => Some(format!("≥ {}", size(a))),
            (None, Some(b)) => Some(format!("≤ {}", size(b))),
            (Some(a), Some(b)) => Some(format!("{} – {}", size(a), size(b))),
        }
    }

    /// The same, as the user would type it again (prompt pre-fill).
    pub fn size_text(&self) -> String {
        let f = |v: Option<u64>| {
            v.map(|b| {
                ByteSize::b(b)
                    .display()
                    .iec()
                    .to_string()
                    .replace(".0 ", " ")
            })
        };
        match (f(self.size_min), f(self.size_max)) {
            (None, None) => String::new(),
            (Some(a), None) => format!("{a}-"),
            (None, Some(b)) => format!("-{b}"),
            (Some(a), Some(b)) => format!("{a}-{b}"),
        }
        .replace(' ', "")
    }
}

/// Parse `100MiB-4GiB`, `1GiB-` (at least), `-700MB` (at most) or `500MiB`
/// (at least). Empty clears the filter. A bare number is bytes.
///
/// # Errors
/// A message for the status line.
pub fn parse_size_range(text: &str) -> Result<(Option<u64>, Option<u64>), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok((None, None));
    }
    let one = |s: &str| -> Result<Option<u64>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        ByteSize::from_str(s)
            .map(|b| Some(b.as_u64()))
            .map_err(|_| format!("'{s}' is not a size (try 100MiB-4GiB)"))
    };
    let (min, max) = match text.split_once('-') {
        Some((a, b)) => (one(a)?, one(b)?),
        None => (one(text)?, None),
    };
    if let (Some(a), Some(b)) = (min, max)
        && a > b
    {
        return Err("The smallest size is bigger than the largest".into());
    }
    Ok((min, max))
}

/// Indexes of the results that pass `filters`, ordered by `sort`.
pub fn visible(
    results: &[SearchResultDto],
    filters: &Filters,
    sort: SortKey,
    reverse: bool,
) -> Vec<usize> {
    let pattern = (!filters.title.trim().is_empty())
        .then(|| Pattern::parse(&filters.title, CaseMatching::Smart, Normalization::Smart));
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let source = filters.source.trim().to_lowercase();
    let mut out: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            if r.seeders.unwrap_or(0) < filters.min_seeders {
                return false;
            }
            if filters.size_min.is_some() || filters.size_max.is_some() {
                let Some(s) = r.size else { return false };
                if filters.size_min.is_some_and(|m| s < m)
                    || filters.size_max.is_some_and(|m| s > m)
                {
                    return false;
                }
            }
            if !source.is_empty() && !r.sources.iter().any(|s| s.to_lowercase().contains(&source)) {
                return false;
            }
            match &pattern {
                Some(p) => p
                    .score(Utf32Str::new(&r.title, &mut buf), &mut matcher)
                    .is_some(),
                None => true,
            }
        })
        .map(|(i, _)| i)
        .collect();
    out.sort_by(|a, b| {
        let o = sort.compare(&results[*a], &results[*b]);
        let o = if reverse { o.reverse() } else { o };
        o.then(a.cmp(b))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(title: &str, size: u64, seeders: u32, src: &str) -> SearchResultDto {
        SearchResultDto {
            title: title.into(),
            size: Some(size),
            seeders: Some(seeders),
            sources: vec![src.into()],
            ..SearchResultDto::default()
        }
    }

    #[test]
    fn size_ranges() {
        let mib = 1 << 20;
        assert_eq!(
            parse_size_range("100MiB-4GiB"),
            Ok((Some(100 * mib), Some(4 << 30)))
        );
        assert_eq!(parse_size_range("1GiB-"), Ok((Some(1 << 30), None)));
        assert_eq!(parse_size_range("-700MiB"), Ok((None, Some(700 * mib))));
        assert_eq!(parse_size_range("500 MiB"), Ok((Some(500 * mib), None)));
        assert_eq!(parse_size_range(" "), Ok((None, None)));
        assert!(parse_size_range("4GiB-100MiB").is_err());
        assert!(parse_size_range("big").is_err());
    }

    #[test]
    fn filters_and_sorts() {
        let list = vec![
            r("Debian 12", 600 << 20, 50, "nyaa"),
            r("Ubuntu 24.04", 5 << 30, 900, "tpb"),
            r("Ubuntu Server", 2 << 30, 20, "tpb, nyaa"),
        ];
        let all = visible(&list, &Filters::default(), SortKey::Seeders, false);
        assert_eq!(all, vec![1, 0, 2]);
        assert_eq!(
            visible(&list, &Filters::default(), SortKey::Seeders, true),
            vec![2, 0, 1]
        );
        assert_eq!(
            visible(&list, &Filters::default(), SortKey::Title, false),
            vec![0, 1, 2]
        );
        assert_eq!(
            visible(&list, &Filters::default(), SortKey::Size, false),
            vec![1, 2, 0]
        );
        let f = Filters {
            min_seeders: 30,
            ..Filters::default()
        };
        assert_eq!(visible(&list, &f, SortKey::Seeders, false), vec![1, 0]);
        let f = Filters {
            size_min: Some(1 << 30),
            size_max: Some(3 << 30),
            ..Filters::default()
        };
        assert_eq!(visible(&list, &f, SortKey::Seeders, false), vec![2]);
        let f = Filters {
            source: "NYAA".into(),
            ..Filters::default()
        };
        assert_eq!(visible(&list, &f, SortKey::Seeders, false), vec![0, 2]);
        let f = Filters {
            title: "ubsrv".into(),
            ..Filters::default()
        };
        assert_eq!(visible(&list, &f, SortKey::Seeders, false), vec![2]);
    }

    #[test]
    fn size_text_round_trips() {
        let f = Filters {
            size_min: parse_size_range("100MiB-4GiB").map(|r| r.0).unwrap_or(None),
            size_max: Some(4 << 30),
            ..Filters::default()
        };
        assert_eq!(f.size_text(), "100MiB-4GiB");
        assert_eq!(f.size_label().as_deref(), Some("100.0 MiB – 4.0 GiB"));
    }
}
