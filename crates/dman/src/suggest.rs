//! "Did you mean ...?" suggestions for names the user mistyped (job ids,
//! providers, profiles, config keys). A small edit distance is enough; no
//! extra dependency.

/// Edit distance between `a` and `b` in characters: insertions, deletions,
/// substitutions and swaps of two neighbours each cost 1 (so `wrok` is one
/// edit from `work`).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows: Vec<Vec<usize>> = vec![(0..=b.len()).collect()];
    for i in 1..=a.len() {
        let mut row = vec![i];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j - 1] + cost)
                .min(rows[i - 1][j] + 1)
                .min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            row.push(best);
        }
        rows.push(row);
    }
    rows[a.len()][b.len()]
}

/// At most `limit` candidates close to `input`, nearest first (ties keep the
/// candidates' order). Matching ignores case. A candidate is close when the
/// distance is at most a third of the input length (at least 1, at most 3), or
/// when one contains the other.
pub fn closest<'a>(
    input: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let needle = input.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let max = (needle.chars().count() / 3).clamp(1, 3);
    let mut scored: Vec<(usize, &str)> = candidates
        .into_iter()
        .filter_map(|c| {
            let hay = c.to_lowercase();
            if hay == needle {
                return None;
            }
            let d = edit_distance(&needle, &hay);
            let related = needle.len() >= 3 && (hay.contains(&needle) || needle.contains(&hay));
            (d <= max || related).then_some((if d <= max { d } else { max + 1 }, c))
        })
        .collect();
    scored.sort_by_key(|(d, _)| *d);
    scored
        .into_iter()
        .take(limit)
        .map(|(_, c)| c.to_owned())
        .collect()
}

/// "did you mean `a`?" / "did you mean `a` or `b`?"; `None` without suggestions.
pub fn did_you_mean(suggestions: &[String]) -> Option<String> {
    let quoted: Vec<String> = suggestions.iter().map(|s| format!("`{s}`")).collect();
    match quoted.as_slice() {
        [] => None,
        [one] => Some(format!("did you mean {one}?")),
        [init @ .., last] => Some(format!("did you mean {} or {last}?", init.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("limits", "limist"), 1);
        assert_eq!(edit_distance("ab", "ba"), 1);
        assert_eq!(edit_distance("ü", "u"), 1);
    }

    #[test]
    fn closest_ranks_and_filters() {
        let names = ["nyaa", "yts", "1337x", "limetorrents"];
        assert_eq!(closest("nyaaa", names, 3), vec!["nyaa"]);
        assert!(closest("YTS", names, 3).is_empty(), "exact match");
        assert_eq!(closest("lime", names, 3), vec!["limetorrents"]);
        assert!(closest("zzzz", names, 3).is_empty());
        assert!(closest("", names, 3).is_empty());
        assert_eq!(closest("12", ["11", "13", "99"], 2), vec!["11", "13"]);
    }

    #[test]
    fn phrasing() {
        assert_eq!(did_you_mean(&[]), None);
        assert_eq!(did_you_mean(&["a".into()]).unwrap(), "did you mean `a`?");
        assert_eq!(
            did_you_mean(&["a".into(), "b".into(), "c".into()]).unwrap(),
            "did you mean `a`, `b` or `c`?"
        );
    }
}
