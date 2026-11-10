//! "Which key": after a prefix key a small card lists what the next key can
//! do. The prefix key keeps its own meaning (e.g. `s` still cycles the sort
//! order); the card only offers direct choices for a short while. Any key
//! that is not offered closes the card and works as usual.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{Cmd, State};
use crate::list::SortKey;

/// How long the card stays without a key press.
pub const SHOW_FOR: Duration = Duration::from_millis(2500);

/// Which prefix opened the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    /// The sort key (`s`): pick a sort order directly.
    Sort,
}

/// The open card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhichKey {
    /// The prefix.
    pub prefix: Prefix,
    /// Time shown so far.
    pub age: Duration,
}

/// Direct choices after `s`.
pub const SORT_KEYS: &[(char, SortKey)] = &[
    ('q', SortKey::Queue),
    ('n', SortKey::Name),
    ('z', SortKey::Size),
    ('p', SortKey::Progress),
    ('d', SortKey::Speed),
    ('e', SortKey::Eta),
    ('a', SortKey::Added),
    ('r', SortKey::Ratio),
];

/// Open the card for `prefix`.
pub fn open(state: &mut State, prefix: Prefix) {
    state.whichkey = Some(WhichKey {
        prefix,
        age: Duration::ZERO,
    });
}

/// Tick: close the card after [`SHOW_FOR`]; `true` when it closed.
pub fn on_tick(state: &mut State) -> bool {
    let tick = state.tick;
    if let Some(w) = &mut state.whichkey {
        w.age += tick;
        if w.age >= SHOW_FOR {
            state.whichkey = None;
            return true;
        }
    }
    false
}

/// A key while the card is open: an offered key acts and closes it; any
/// other key closes it and is handled as usual (`None`).
pub fn handle_key(state: &mut State, key: KeyEvent) -> Option<Vec<Cmd>> {
    let w = state.whichkey.take()?;
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match (w.prefix, key.code) {
        (Prefix::Sort, KeyCode::Char(c)) if plain => {
            let (_, sort) = SORT_KEYS.iter().find(|(k, _)| *k == c)?;
            state.list.sort = *sort;
            let rev = if state.list.reverse { ", reversed" } else { "" };
            state
                .status
                .info(format!("Sorted by {}{rev}", sort.label()));
            state.refresh_list();
            Some(Vec::new())
        }
        _ => None,
    }
}

/// Draw the card centred at the bottom of `area`; returns where.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) -> Option<Rect> {
    let w = state.whichkey.as_ref()?;
    let t = &state.theme;
    let mut items: Vec<Vec<Span<'static>>> = Vec::new();
    match w.prefix {
        Prefix::Sort => {
            for (k, sort) in SORT_KEYS {
                let current = *sort == state.list.sort;
                let style = if current {
                    t.accent.patch(t.surface_bg())
                } else {
                    t.surface
                };
                items.push(vec![
                    Span::styled(format!(" {k} "), t.pill),
                    Span::styled(format!(" {:<9}", sort.label()), style),
                ]);
            }
            items.push(vec![
                Span::styled(" s ", t.pill),
                Span::styled(" next     ", t.dim.patch(t.surface_bg())),
            ]);
        }
    }
    let max_w = usize::from(area.width.saturating_sub(4)).max(14);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for item in items {
        let iw: usize = item.iter().map(Span::width).sum::<usize>() + 1;
        if used > 0 && used + iw > max_w {
            lines.push(Line::from(std::mem::take(&mut cur)));
            used = 0;
        }
        cur.push(Span::styled(" ", t.surface));
        cur.extend(item);
        used += iw;
    }
    if !cur.is_empty() {
        lines.push(Line::from(cur));
    }
    let width = lines.iter().map(Line::width).max().unwrap_or(0) + 3;
    let height = lines.len() + 2;
    if usize::from(area.height) < height + 1 {
        return None;
    }
    let block = t.popup().title(Span::styled(
        " sort by — or keep going ",
        t.accent.patch(t.surface_bg()),
    ));
    let w = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let h = u16::try_from(height).unwrap_or(u16::MAX);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + area.height - h,
        width: w,
        height: h,
    };
    crate::widgets::clear_wide_edge(frame, rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
    Some(rect)
}
