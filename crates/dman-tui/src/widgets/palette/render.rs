//! Drawing of the command palette: query, group tabs, rows, detail line.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Entry, PaletteState};
use crate::action::Action;
use crate::app::State;
use crate::format;
use crate::layout::Hit;
use crate::theme::Theme;
use crate::widgets::{modal_frame, scrollbar};

impl PaletteState {
    /// Draw the palette in `area` (the screen minus the footer).
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, state: &State) {
        let theme = &state.theme;
        if area.width < 10 || area.height < 5 {
            return;
        }
        // Wide terminals get a centred card; small ones the whole area.
        let width = if area.width >= 70 {
            (area.width * 4 / 5).clamp(66, 104)
        } else {
            area.width
        };
        let top = match area.height {
            0..=21 => 0,
            h => (h / 10).min(4),
        };
        let height = area.height.saturating_sub(top * 2).min(34);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + top,
            width,
            height,
        };
        let title = Line::from(Span::styled(
            " Commands ",
            theme.accent.patch(theme.surface_bg()),
        ));
        let inner = modal_frame(frame, rect, theme, title, theme.border_focus);
        let count = if self.input.text().trim().is_empty() {
            format!(
                " {} commands ",
                self.counts.get(self.tab).copied().unwrap_or(0)
            )
        } else {
            format!(" {} found ", self.items().count())
        };
        let cw = u16::try_from(format::width(&count)).unwrap_or(0);
        if rect.width > cw + 14 {
            frame.render_widget(
                Paragraph::new(Span::styled(count, theme.faint.patch(theme.surface_bg()))),
                Rect::new(rect.right().saturating_sub(cw + 2), rect.y, cw, 1),
            );
        }
        let rows = inner.height;
        // Query, tabs, rule, list, detail line; the rule and the detail line
        // give way on short terminals.
        let rule = rows >= 9;
        let detail = rows >= 8;
        let mut y = inner.y;
        let mut take = |h: u16| {
            let r = Rect {
                y,
                height: h,
                ..inner
            };
            y = y.saturating_add(h);
            r
        };
        let query = take(1);
        let tabs = take(1);
        let rule_area = take(u16::from(rule));
        let list_h = rows.saturating_sub(2 + u16::from(rule) + u16::from(detail));
        let list = take(list_h);
        let detail_area = take(u16::from(detail));
        self.render_query(frame, query, theme);
        self.render_tabs(frame, tabs, state);
        if rule {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "─".repeat(usize::from(rule_area.width)),
                    theme.border.patch(theme.surface_bg()),
                )),
                rule_area,
            );
        }
        self.render_list(frame, list, rect, state);
        if detail {
            self.render_detail(frame, detail_area, theme);
        }
    }

    fn render_query(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut q = vec![Span::styled(
            format!(" {} ", theme.icons.search),
            theme.accent.patch(theme.surface_bg()),
        )];
        let room = usize::from(area.width).saturating_sub(4);
        if self.input.text().is_empty() {
            q.push(Span::styled(" ", theme.cursor));
            q.push(Span::styled(
                format::truncate("Type to search every group", room.saturating_sub(1)),
                theme.faint.patch(theme.surface_bg()),
            ));
        } else {
            q.extend(self.input.line_fit(theme, true, room).spans);
        }
        frame.render_widget(Paragraph::new(Line::from(q)).style(theme.surface), area);
    }

    fn render_tabs(&self, frame: &mut Frame<'_>, area: Rect, state: &State) {
        let theme = &state.theme;
        let searching = !self.input.text().trim().is_empty();
        let make = |short: bool| {
            move |i: usize| {
                let name = if short {
                    Self::tab_short(i)
                } else {
                    Self::tab_label(i)
                };
                match self.counts.get(i) {
                    Some(n) if searching => format!(" {name} {n} "),
                    _ => format!(" {name} "),
                }
            }
        };
        let total = |short: bool| -> usize {
            (0..Self::tabs())
                .map(|i| format::width(&make(short)(i)) + 1)
                .sum()
        };
        let room = usize::from(area.width);
        let short = total(false) >= room;
        let label = make(short);
        let full = total(short);
        let mut spans = vec![Span::styled(" ", theme.surface)];
        let mut x = area.x + 1;
        if full < usize::from(area.width) {
            for i in 0..Self::tabs() {
                let text = label(i);
                let w = u16::try_from(format::width(&text)).unwrap_or(0);
                let style = if i == self.tab {
                    theme.mode
                } else if searching && self.counts.get(i) == Some(&0) {
                    theme.faint.patch(theme.surface_bg())
                } else {
                    theme.dim.patch(theme.surface_bg())
                };
                spans.push(Span::styled(text, style));
                spans.push(Span::styled(" ", theme.surface));
                state.hit(Rect::new(x, area.y, w, 1), Hit::PaletteTab(i));
                x = x.saturating_add(w + 1);
            }
        } else {
            // Narrow: a switcher showing the current tab, with arrows that
            // are click targets too.
            let n = Self::tabs();
            let text = label(self.tab);
            let pos = format!(" {}/{n}", self.tab + 1);
            spans.push(Span::styled("‹", theme.accent.patch(theme.surface_bg())));
            state.hit(
                Rect::new(x, area.y, 1, 1),
                Hit::PaletteTab((self.tab + n - 1) % n),
            );
            spans.push(Span::styled(" ", theme.surface));
            spans.push(Span::styled(text.clone(), theme.mode));
            spans.push(Span::styled(" ", theme.surface));
            let after = x + 2 + u16::try_from(format::width(&text)).unwrap_or(0) + 1;
            spans.push(Span::styled("›", theme.accent.patch(theme.surface_bg())));
            state.hit(
                Rect::new(after, area.y, 1, 1),
                Hit::PaletteTab((self.tab + 1) % n),
            );
            spans.push(Span::styled(pos, theme.faint.patch(theme.surface_bg())));
            let used: usize = spans.iter().map(Span::width).sum();
            let hint = "  Tab switches";
            if used + hint.len() <= usize::from(area.width) {
                spans.push(Span::styled(hint, theme.faint.patch(theme.surface_bg())));
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(theme.surface), area);
    }

    fn render_list(&self, frame: &mut Frame<'_>, list: Rect, frame_rect: Rect, state: &State) {
        let theme = &state.theme;
        if list.height == 0 {
            return;
        }
        if self.entries.is_empty() {
            let text = if self.input.text().trim().is_empty() {
                "  Nothing here".to_owned()
            } else if self.tab != 0 && self.counts.first().is_some_and(|n| *n > 0) {
                format!(
                    "  No match in {} — Tab shows the other groups",
                    Self::tab_label(self.tab)
                )
            } else {
                "  No command matches — Esc clears the search".to_owned()
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format::truncate(&text, usize::from(list.width)),
                    theme.dim.patch(theme.surface_bg()),
                )),
                list,
            );
            return;
        }
        let height = usize::from(list.height);
        // Keep the highlighted row visible; show its heading when it fits.
        let mut start = self.selected.saturating_sub(height.saturating_sub(1));
        if start > 0
            && self.selected - start + 1 < height
            && matches!(self.entries.get(start - 1), Some(Entry::Header(_)))
        {
            start -= 1;
        }
        // Scroll by whole pages so the list does not jump on every key.
        let start = start.min(self.entries.len().saturating_sub(height));
        let searching = !self.input.text().trim().is_empty();
        let show_group = searching && self.tab == 0 && list.width >= 56;
        for (row, (i, entry)) in self
            .entries
            .iter()
            .enumerate()
            .skip(start)
            .take(height)
            .enumerate()
        {
            let y = list.y + u16::try_from(row).unwrap_or(0);
            let area = Rect::new(list.x, y, list.width, 1);
            let line = match entry {
                Entry::Header(name) => header_line(theme, name, usize::from(list.width)),
                Entry::Item { action, hits } => {
                    let selected = i == self.selected;
                    state.hit(area, Hit::PaletteItem(i));
                    item_line(
                        state,
                        *action,
                        hits,
                        self.reason(*action),
                        selected,
                        show_group,
                        usize::from(list.width),
                    )
                }
            };
            frame.render_widget(Paragraph::new(line), area);
        }
        scrollbar(
            frame,
            Rect {
                y: list.y,
                height: list.height,
                ..frame_rect
            },
            theme,
            self.entries.len(),
            start,
            height,
        );
    }

    fn render_detail(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let surface = theme.surface;
        let w = usize::from(area.width).saturating_sub(2);
        let line = if let Some(n) = &self.notice {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", theme.icons.warn),
                    theme.warn.patch(theme.surface_bg()),
                ),
                Span::styled(
                    format::truncate(n, w.saturating_sub(2)),
                    theme.warn.patch(theme.surface_bg()),
                ),
            ])
        } else if let Some(a) = self.current() {
            let (_, desc) = a.palette_text();
            match (self.reason(a), desc) {
                (Some(r), _) => Line::styled(
                    format::truncate(&format!(" Not available: {r}"), w + 1),
                    theme.faint.patch(theme.surface_bg()),
                ),
                (None, Some(d)) => Line::styled(
                    format::truncate(&format!(" {d}"), w + 1),
                    theme.dim.patch(theme.surface_bg()),
                ),
                (None, None) => Line::styled(
                    format::truncate(" Enter runs it · Tab switches groups · Esc closes", w + 1),
                    theme.faint.patch(theme.surface_bg()),
                ),
            }
        } else {
            Line::default()
        };
        frame.render_widget(Paragraph::new(line).style(surface), area);
    }
}

fn header_line(theme: &Theme, name: &str, width: usize) -> Line<'static> {
    let label = format!(" {} ", name.to_uppercase());
    let rest = width.saturating_sub(format::width(&label) + 1);
    Line::from(vec![
        Span::styled(" ", theme.surface),
        Span::styled(label, theme.section.patch(theme.surface_bg())),
        Span::styled("╌".repeat(rest), theme.border.patch(theme.surface_bg())),
    ])
}

/// The key shown for `a`: its key in the current view, else in the view it
/// belongs to.
fn key_text(state: &State, a: Action) -> String {
    let here = state.keymap.keys_with_global(state.context(), a);
    let keys = if here.is_empty() {
        state.keymap.keys_with_global(a.home_context(), a)
    } else {
        here
    };
    keys.first()
        .map(|k| crate::keymap::key_label(*k))
        .unwrap_or_default()
}

/// One command row: cursor bar, name (matches highlighted), description or
/// the reason it is unavailable, group (search results), key pill.
fn item_line(
    state: &State,
    a: Action,
    hits: &[u32],
    reason: Option<&str>,
    selected: bool,
    show_group: bool,
    width: usize,
) -> Line<'static> {
    let theme = &state.theme;
    let base = if selected {
        theme.cursor
    } else {
        theme.surface
    };
    let bg = |s: Style| match base.bg {
        Some(c) => s.bg(c),
        None => s,
    };
    let off = reason.is_some();
    let (mut name, desc) = a.palette_text();
    if let (Action::VideoQualityToggle, Some(p)) = (a, state.video_prefs.as_ref()) {
        name.push_str(if p.ask_quality {
            " · now: ask"
        } else {
            " · now: defaults"
        });
    }
    let name_style = if off { bg(theme.faint) } else { base };
    // Right side: group label (search results) and key pill; the group
    // goes first, then the pill, when the name needs the room.
    let keys = key_text(state, a);
    let group =
        show_group.then(|| Span::styled(format!("{:>13} ", a.group().label()), bg(theme.faint)));
    let pill = (!keys.is_empty()).then(|| {
        let style = if off { bg(theme.faint) } else { theme.pill };
        Span::styled(format!(" {keys} "), style)
    });
    let left = 2;
    let need = left + format::width(&name).min(16) + 1;
    let w = |s: &Option<Span<'static>>| s.as_ref().map_or(0, Span::width);
    let group = if need + w(&group) + w(&pill) <= width {
        group
    } else {
        None
    };
    let pill = if need + w(&pill) <= width { pill } else { None };
    let mut right: Vec<Span<'static>> = group.into_iter().chain(pill).collect();
    right.push(Span::styled(" ", base));
    let right_w: usize = right.iter().map(Span::width).sum();
    let mut spans = vec![
        Span::styled(
            if selected { theme.icons.cursor } else { " " },
            bg(theme.cursor_bar),
        ),
        Span::styled(" ", base),
    ];
    let name_room = width.saturating_sub(left + right_w);
    let shown = format::truncate(&name, name_room);
    for (i, ch) in shown.chars().enumerate() {
        let hit = u32::try_from(i).is_ok_and(|i| hits.binary_search(&i).is_ok());
        let style = if hit && !off {
            bg(theme.matched)
        } else {
            name_style
        };
        spans.push(Span::styled(ch.to_string(), style));
    }
    let mut used = left + format::width(&shown);
    // The description (or why it is unavailable) in the room that is left.
    let room = width.saturating_sub(used + right_w + 3);
    let extra = match (reason, desc) {
        (Some(r), _) => Some((
            format!("⊘ {r}"),
            bg(theme.faint).add_modifier(Modifier::ITALIC),
        )),
        (None, Some(d)) => Some((d, bg(theme.faint))),
        (None, None) => None,
    };
    if let Some((text, style)) = extra
        && room >= 8
    {
        let text = format::truncate(&text, room);
        used += 2 + format::width(&text);
        spans.push(Span::styled("  ", base));
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used + right_w)),
        base,
    ));
    spans.extend(right);
    Line::from(spans)
}
