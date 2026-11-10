//! Word wrapping of styled lines by display width (wide characters count two
//! columns), so dialogs can measure exactly how many rows their text takes.

use ratatui::text::{Line, Span};

use crate::format::width;

/// Wrap `line` to `cols` columns at spaces; words longer than a row are cut
/// hard. Styles of the spans are kept; the line style is kept on every row.
/// An empty line stays one empty row.
pub fn wrap(line: &Line<'_>, cols: usize) -> Vec<Line<'static>> {
    let cols = cols.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    // Rows after a wrap drop the spaces that ended the previous row.
    let mut continuation = false;
    for span in &line.spans {
        let style = span.style;
        for word in span.content.split_inclusive(' ') {
            let bare = word.trim_end_matches(' ');
            if used > 0 && used + width(bare) > cols {
                rows.push(Vec::new());
                used = 0;
                continuation = true;
            }
            let word = if used == 0 && continuation {
                word.trim_start_matches(' ')
            } else {
                word
            };
            if used == 0 && width(word.trim_end_matches(' ')) > cols {
                // Hard-cut a word that does not fit on a row of its own.
                let mut chunk = String::new();
                for c in word.chars() {
                    let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                    if used + w > cols && c != ' ' {
                        push(&mut rows, Span::styled(std::mem::take(&mut chunk), style));
                        rows.push(Vec::new());
                        used = 0;
                        continuation = true;
                    }
                    chunk.push(c);
                    used += w;
                }
                push(&mut rows, Span::styled(chunk, style));
                continue;
            }
            used += width(word);
            push(&mut rows, Span::styled(word.to_owned(), style));
        }
    }
    rows.into_iter()
        .map(|mut spans| {
            // Trailing spaces never push a row past `cols`.
            while let Some(last) = spans.last_mut() {
                let t = last.content.trim_end_matches(' ');
                if t.is_empty() {
                    spans.pop();
                } else {
                    last.content = t.to_owned().into();
                    break;
                }
            }
            Line::from(spans).style(line.style)
        })
        .collect()
}

fn push(rows: &mut [Vec<Span<'static>>], span: Span<'static>) {
    if let Some(row) = rows.last_mut()
        && !span.content.is_empty()
    {
        row.push(span);
    }
}

/// Wrap every line; see [`wrap`].
pub fn wrap_all(lines: &[Line<'_>], cols: usize) -> Vec<Line<'static>> {
    lines.iter().flat_map(|l| wrap(l, cols)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Line<'_>]) -> Vec<String> {
        rows.iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wraps_at_spaces_and_cuts_long_words() {
        let rows = wrap(&Line::from("one two three four"), 9);
        assert_eq!(text(&rows), ["one two", "three", "four"]);
        let rows = wrap(&Line::from("  a   indented"), 20);
        assert_eq!(text(&rows), ["  a   indented"]);
        let rows = wrap(&Line::from("abcdefghijkl xy"), 5);
        assert_eq!(text(&rows), ["abcde", "fghij", "kl xy"]);
        assert_eq!(wrap(&Line::default(), 5).len(), 1);
        for r in wrap(&Line::from("日本語のファイル名 です"), 5) {
            assert!(r.width() <= 5, "{r:?}");
        }
    }

    #[test]
    fn keeps_span_styles() {
        use ratatui::style::Style;
        let bold = Style::new().bold();
        let rows = wrap(
            &Line::from(vec![Span::raw("aa "), Span::styled("bb cc", bold)]),
            5,
        );
        assert_eq!(text(&rows), ["aa bb", "cc"]);
        assert_eq!(rows[1].spans[0].style, bold);
    }
}
