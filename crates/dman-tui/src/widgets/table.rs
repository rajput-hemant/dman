//! Responsive text tables: columns drop by priority when the width runs out,
//! and every cell is cut with an ellipsis to its column instead of being
//! clipped mid-word.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::format;
use crate::theme::Theme;

/// One column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Col {
    /// Header text.
    pub title: &'static str,
    /// Fixed width, or the minimum width of a fill column.
    pub width: u16,
    /// Takes the room left over (shared equally between fill columns).
    pub fill: bool,
    /// Right-aligned (numbers).
    pub right: bool,
    /// Lower is kept longer; 0 is never dropped.
    pub priority: u8,
}

impl Col {
    /// A fixed-width, left-aligned column.
    pub const fn fixed(title: &'static str, width: u16, priority: u8) -> Self {
        Self {
            title,
            width,
            fill: false,
            right: false,
            priority,
        }
    }

    /// A column taking the remaining room (at least `min`).
    pub const fn fill(title: &'static str, min: u16, priority: u8) -> Self {
        Self {
            title,
            width: min,
            fill: true,
            right: false,
            priority,
        }
    }

    /// Right-align this column.
    #[must_use]
    pub const fn right(mut self) -> Self {
        self.right = true;
        self
    }
}

/// Columns shown in `width` cells as `(index into cols, width)`, separated
/// by one space. Columns with the highest priority number go first until the
/// rest fits.
pub fn layout(cols: &[Col], width: u16) -> Vec<(usize, u16)> {
    let mut keep: Vec<usize> = (0..cols.len()).collect();
    let need = |keep: &[usize]| -> u16 {
        let gaps = u16::try_from(keep.len().saturating_sub(1)).unwrap_or(u16::MAX);
        keep.iter()
            .map(|&i| cols[i].width)
            .fold(gaps, u16::saturating_add)
    };
    while need(&keep) > width {
        let Some(pos) = keep
            .iter()
            .enumerate()
            .filter(|(_, i)| cols[**i].priority > 0)
            .max_by_key(|(n, i)| (cols[**i].priority, *n))
            .map(|(n, _)| n)
        else {
            break;
        };
        keep.remove(pos);
    }
    let spare = width.saturating_sub(need(&keep));
    let fills = u16::try_from(keep.iter().filter(|i| cols[**i].fill).count()).unwrap_or(0);
    let mut extra = spare;
    let mut out = Vec::with_capacity(keep.len());
    let mut fill_seen = 0;
    for &i in &keep {
        let mut w = cols[i].width;
        if cols[i].fill && fills > 0 {
            fill_seen += 1;
            // The last fill column takes the rounding remainder.
            let share = if fill_seen == fills {
                extra
            } else {
                spare / fills
            };
            extra = extra.saturating_sub(share);
            w = w.saturating_add(share);
        }
        out.push((i, w));
    }
    // Nothing fits at all: show the first kept column cut to the width.
    if need(&keep) > width {
        out.truncate(1);
        if let Some(first) = out.first_mut() {
            first.1 = width;
        }
    }
    out
}

/// One cell: text and style.
pub type Cell = (String, Style);

/// The text of `cells` laid out per `shown` (see [`layout`]).
pub fn line(cols: &[Col], shown: &[(usize, u16)], cells: &[Cell], base: Style) -> Line<'static> {
    let mut spans = Vec::with_capacity(shown.len() * 2);
    for (n, &(i, w)) in shown.iter().enumerate() {
        if n > 0 {
            spans.push(Span::styled(" ", base));
        }
        let w = usize::from(w);
        let (text, style) = cells.get(i).cloned().unwrap_or_default();
        let cut = format::truncate(&text, w);
        let pad = " ".repeat(w.saturating_sub(format::width(&cut)));
        let text = if cols[i].right {
            format!("{pad}{cut}")
        } else {
            format!("{cut}{pad}")
        };
        spans.push(Span::styled(text, base.patch(style)));
    }
    Line::from(spans)
}

/// Draw a table with a header row; `rows` is windowed so that `cursor`
/// stays visible. Returns the first row drawn.
pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    t: &Theme,
    cols: &[Col],
    rows: &[(Vec<Cell>, Style)],
    cursor: Option<usize>,
) -> usize {
    if area.height == 0 || area.width == 0 {
        return 0;
    }
    let shown = layout(cols, area.width);
    let head: Vec<Cell> = cols
        .iter()
        .map(|c| (c.title.to_owned(), t.header))
        .collect();
    let mut lines = vec![line(cols, &shown, &head, Style::new())];
    let height = usize::from(area.height.saturating_sub(1));
    let start = window_start(rows.len(), cursor.unwrap_or(0), height);
    for (i, (cells, style)) in rows.iter().enumerate().skip(start).take(height) {
        let base = if Some(i) == cursor { t.cursor } else { *style };
        lines.push(line(cols, &shown, cells, base));
    }
    frame.render_widget(Paragraph::new(lines), area);
    if let Some(c) = cursor
        && let Some(y) = c.checked_sub(start)
        && y < height
    {
        let row = Rect::new(
            area.x,
            area.y + 1 + u16::try_from(y).unwrap_or(0),
            area.width,
            1,
        );
        frame.buffer_mut().set_style(row, t.cursor);
    }
    start
}

/// First row to draw so that `cursor` stays visible in `height` rows.
pub fn window_start(len: usize, cursor: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        return 0;
    }
    cursor.saturating_sub(height / 2).min(len - height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLS: [Col; 4] = [
        Col::fill("Name", 8, 0),
        Col::fixed("Size", 9, 1),
        Col::fixed("Peers", 6, 3),
        Col::fixed("Note", 10, 2),
    ];

    #[test]
    fn columns_drop_by_priority_and_fill_takes_the_rest() {
        let all = layout(&COLS, 60);
        assert_eq!(all.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(all.iter().map(|c| c.1).sum::<u16>() + 3, 60);
        let mid = layout(&COLS, 30);
        assert_eq!(mid.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1, 3]);
        let small = layout(&COLS, 18);
        assert_eq!(small.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1]);
        let tiny = layout(&COLS, 5);
        assert_eq!(tiny, [(0, 5)]);
        assert!(layout(&COLS, 0).iter().all(|c| c.1 == 0));
    }

    #[test]
    fn cells_are_cut_with_an_ellipsis() {
        let shown = layout(&COLS, 18);
        let l = line(
            &COLS,
            &shown,
            &[
                ("a-very-long-file-name.iso".into(), Style::new()),
                ("12.0 MiB".into(), Style::new()),
            ],
            Style::new(),
        );
        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "a-very-… 12.0 MiB ");
        assert_eq!(l.width(), 18);
    }
}
