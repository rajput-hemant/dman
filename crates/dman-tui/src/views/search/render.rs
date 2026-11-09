//! Drawing of the search view.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table};

use super::filter::SortKey;
use super::{Focus, Prov, result_key};
use crate::action::Action;
use crate::app::State;
use crate::format::{self, truncate};

/// Braille spinner frames.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const LABEL_W: usize = 10;
/// Lines the provider status may use.
const MAX_STATUS_LINES: usize = 3;

struct Areas {
    form: Rect,
    status: Rect,
    filters: Rect,
    table: Rect,
}

/// Coloured text pieces of the per-provider status line.
fn status_segments(state: &State) -> Vec<(String, Style)> {
    let (s, theme) = (&state.search, &state.theme);
    let Some(run) = &s.run else {
        return Vec::new();
    };
    let spin = SPINNER[s.spin % SPINNER.len()];
    let mut out = Vec::new();
    for (name, p) in &run.providers {
        out.push(match p {
            Prov::Running => (format!("{name} {spin}"), theme.warn),
            Prov::Done { count, elapsed_ms } => {
                (format!("{name} ✓ {count} in {elapsed_ms} ms"), theme.ok)
            }
            Prov::Failed(e) => (format!("{name} ✗ failed: {}", truncate(e, 48)), theme.error),
            Prov::Cancelled => (format!("{name} – cancelled"), theme.dim),
        });
    }
    if let Some(e) = &run.error {
        out.push((format!("search failed: {}", truncate(e, 60)), theme.error));
    } else if run.providers.is_empty() && run.finished {
        out.push((
            format!(
                "no provider was queried — press {} to enable some",
                super::key_hint(state, Action::SearchProviders)
            ),
            theme.warn,
        ));
    } else if run.providers.is_empty() {
        out.push((format!("starting {spin}"), theme.warn));
    }
    out
}

/// Pack segments into at most [`MAX_STATUS_LINES`] lines of `width`.
fn status_lines(segments: &[(String, Style)], width: usize) -> Vec<Line<'static>> {
    let mut done: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut used = 0;
    let mut cut = false;
    for (text, style) in segments {
        let text = truncate(text, width.saturating_sub(3).max(1));
        let w = text.chars().count();
        let sep = if used == 0 { 0 } else { 3 };
        if used > 0 && used + sep + w > width.saturating_sub(1) {
            if done.len() + 1 == MAX_STATUS_LINES {
                cut = true;
                break;
            }
            done.push(std::mem::replace(&mut cur, vec![Span::raw(" ")]));
            used = 0;
        }
        if used > 0 {
            cur.push(Span::raw("   "));
            used += 3;
        }
        used += w;
        cur.push(Span::styled(text, *style));
    }
    if cut {
        cur.push(Span::raw(" …"));
    }
    done.push(cur);
    done.into_iter().map(Line::from).collect()
}

fn areas(area: Rect, state: &State) -> Areas {
    let s = &state.search;
    let status_h = if s.run.is_some() {
        u16::try_from(status_lines(&status_segments(state), usize::from(area.width)).len())
            .unwrap_or(1)
    } else {
        0
    };
    let [form, status, filters, table] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(status_h),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    Areas {
        form,
        status,
        filters,
        table,
    }
}

/// Table body rows that fit below the form (the header row is not counted).
pub fn table_rows(body: Rect, state: &State) -> usize {
    usize::from(areas(body, state).table.height.saturating_sub(1))
}

fn provider_line(state: &State, focused: bool, width: usize) -> Line<'static> {
    let t = &state.theme;
    let s = &state.search;
    let enabled: Vec<&str> = crate::views::providers::enabled(state)
        .map(|p| p.name.as_str())
        .collect();
    if enabled.is_empty() {
        let hint = if state.providers.loaded {
            format!(
                "none enabled — press {} to open the providers settings",
                super::key_hint(state, Action::SearchProviders)
            )
        } else {
            "loading…".to_owned()
        };
        return Line::styled(hint, t.dim);
    }
    let items: Vec<String> = enabled
        .iter()
        .map(|n| format!("[{}] {n}", if s.off.contains(*n) { " " } else { "x" }))
        .collect();
    let cursor = s.sel_cursor.min(items.len() - 1);
    let widths: Vec<usize> = items.iter().map(|i| i.chars().count() + 2).collect();
    let mut start = 0;
    while start < cursor && widths[start..=cursor].iter().sum::<usize>() > width {
        start += 1;
    }
    let mut spans = Vec::new();
    let mut used = 0;
    if start > 0 {
        spans.push(Span::styled("‹ ", t.dim));
        used += 2;
    }
    for (i, item) in items.iter().enumerate().skip(start) {
        if used + widths[i] > width {
            spans.push(Span::styled("›", t.dim));
            break;
        }
        let style = if focused && i == cursor {
            t.cursor
        } else if s.off.contains(enabled[i]) {
            t.dim
        } else {
            t.text
        };
        spans.push(Span::styled(item.clone(), style));
        spans.push(Span::raw("  "));
        used += widths[i];
    }
    Line::from(spans)
}

fn render_form(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let s = &state.search;
    let form_focus = s.focus != Focus::Results && !s.history_open;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if form_focus { t.accent } else { t.border })
        .title(Span::styled(" Search torrents ", t.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width).saturating_sub(LABEL_W + 2);
    let label = |text: &str, focused: bool| {
        Span::styled(
            format!("{}{text:<LABEL_W$}", if focused { "▸ " } else { "  " }),
            if focused { t.accent } else { t.text },
        )
    };
    let q_focus = form_focus && s.focus == Focus::Query;
    let c_focus = form_focus && s.focus == Focus::Category;
    let p_focus = form_focus && s.focus == Focus::Providers;
    let mut query = vec![label("Query", q_focus)];
    if s.query.text().is_empty() && !q_focus {
        query.push(Span::styled("type what to look for", t.dim));
    } else {
        query.extend(s.query.line(t, q_focus).spans);
    }
    let mut cat = vec![label("Category", c_focus)];
    if s.category.text().is_empty() && !c_focus {
        cat.push(Span::styled("(optional, e.g. movies, tv, software)", t.dim));
    } else {
        cat.extend(s.category.line(t, c_focus).spans);
    }
    let mut prov = vec![label("Providers", p_focus)];
    prov.extend(provider_line(state, p_focus, width).spans);
    frame.render_widget(
        Paragraph::new(vec![Line::from(query), Line::from(cat), Line::from(prov)]),
        inner,
    );
}

fn arrow(key: SortKey, reverse: bool) -> &'static str {
    let natural_desc = !matches!(key, SortKey::Title | SortKey::Source);
    if natural_desc != reverse {
        "▼"
    } else {
        "▲"
    }
}

fn render_filters(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let s = &state.search;
    let f = &s.filters;
    let mut spans = vec![
        Span::styled(" sort ", t.dim),
        Span::raw(format!("{}{}", s.sort.label(), arrow(s.sort, s.reverse))),
    ];
    let mut add = |label: &str, value: String| {
        spans.push(Span::styled(format!("  {label} "), t.dim));
        spans.push(Span::styled(value, t.marked));
    };
    if f.min_seeders > 0 {
        add("seeders ≥", f.min_seeders.to_string());
    }
    if let Some(l) = f.size_label() {
        add("size", l);
    }
    if !f.source.is_empty() {
        add("source", f.source.clone());
    }
    if !f.title.is_empty() {
        add("title", f.title.clone());
    }
    if let Some(run) = &s.run {
        let shown = if s.rows.len() == run.results.len() {
            plural_results(run.results.len())
        } else {
            format!("{}/{} shown", s.rows.len(), run.results.len())
        };
        spans.push(Span::styled(format!("  {shown}"), t.dim));
    }
    if !s.selected.is_empty() {
        spans.push(Span::styled(
            format!("  {} marked", s.selected.len()),
            t.marked,
        ));
    }
    let resolving = s.resolving_keys().len();
    if resolving > 0 {
        let spin = SPINNER[s.spin % SPINNER.len()];
        spans.push(Span::styled(
            format!("  {spin} resolving {resolving}…"),
            t.warn,
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn plural_results(n: usize) -> String {
    format::plural(n, "result")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Mark,
    Num,
    Title,
    Size,
    Seeders,
    Leechers,
    Source,
    Date,
}

const DISPLAY: &[(Col, &str, u16)] = &[
    (Col::Mark, "", 1),
    (Col::Num, "#", 3),
    (Col::Title, "Title", 0),
    (Col::Size, "Size", 10),
    (Col::Seeders, "S", 5),
    (Col::Leechers, "L", 5),
    (Col::Source, "Sources", 16),
    (Col::Date, "Date", 10),
];

const PRIORITY: &[Col] = &[
    Col::Size,
    Col::Seeders,
    Col::Date,
    Col::Source,
    Col::Leechers,
];
const MIN_TITLE: u16 = 20;

fn width_of(c: Col) -> u16 {
    DISPLAY.iter().find(|d| d.0 == c).map_or(0, |d| d.2)
}

fn columns(width: u16) -> Vec<Col> {
    let mut chosen = vec![Col::Mark, Col::Num, Col::Title];
    let mut used = width_of(Col::Mark) + 1 + width_of(Col::Num) + 1;
    for c in PRIORITY {
        let need = width_of(*c) + 1;
        if width.saturating_sub(used + need) >= MIN_TITLE {
            used += need;
            chosen.push(*c);
        }
    }
    DISPLAY
        .iter()
        .map(|d| d.0)
        .filter(|c| chosen.contains(c))
        .collect()
}

fn right(s: String) -> Line<'static> {
    Line::from(s).alignment(Alignment::Right)
}

fn date_text(state: &State, secs: Option<i64>) -> String {
    secs.and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map_or_else(
            || "-".into(),
            |t| {
                t.to_zoned(state.time_zone.clone())
                    .strftime("%Y-%m-%d")
                    .to_string()
            },
        )
}

fn render_table(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let s = &state.search;
    let Some(run) = &s.run else {
        let text = format!(
            "Type a query and press Enter · {} providers · {} history",
            super::key_hint(state, Action::SearchProviders),
            super::key_hint(state, Action::SearchHistory)
        );
        render_hint(frame, area, state, &text);
        return;
    };
    let cols = columns(area.width);
    let header = TableRow::new(cols.iter().map(|c| {
        let name = DISPLAY.iter().find(|d| d.0 == *c).map_or("", |d| d.1);
        let w = usize::from(width_of(*c));
        match c {
            Col::Size | Col::Seeders | Col::Leechers => Cell::from(format!("{name:>w$}")),
            _ => Cell::from(name),
        }
    }))
    .style(t.header);
    let widths: Vec<Constraint> = cols
        .iter()
        .map(|c| match c {
            Col::Title => Constraint::Fill(1),
            other => Constraint::Length(width_of(*other)),
        })
        .collect();
    let others: u16 = cols.iter().map(|c| width_of(*c)).sum();
    let gaps = u16::try_from(cols.len().saturating_sub(1)).unwrap_or(0);
    let title_w = usize::from(area.width.saturating_sub(others + gaps));
    let height = usize::from(area.height.saturating_sub(1));
    let resolving = s.resolving_keys();
    let rows: Vec<TableRow<'_>> = s
        .rows
        .iter()
        .enumerate()
        .skip(s.offset)
        .take(height)
        .map(|(pos, idx)| {
            let r = &run.results[*idx];
            let key = result_key(r);
            let marked = s.selected.contains(&key);
            let busy = resolving.contains(&key.as_str());
            let cells = cols.iter().map(|c| match c {
                Col::Mark => Cell::from(if busy {
                    "…"
                } else if marked {
                    "●"
                } else {
                    " "
                })
                .style(if busy { t.warn } else { t.marked }),
                Col::Num => Cell::from(right((pos + 1).to_string())),
                Col::Title => Cell::from(truncate(&r.title, title_w)),
                Col::Size => Cell::from(right(r.size.map_or("-".into(), format::size))),
                Col::Seeders => Cell::from(right(r.seeders.map_or("-".into(), |n| n.to_string())))
                    .style(if r.seeders.unwrap_or(0) > 0 {
                        t.ok
                    } else {
                        t.dim
                    }),
                Col::Leechers => {
                    Cell::from(right(r.leechers.map_or("-".into(), |n| n.to_string())))
                }
                Col::Source => Cell::from(truncate(&r.sources.join(","), 16)),
                Col::Date => Cell::from(date_text(state, r.date)),
            });
            let row = TableRow::new(cells);
            let row = if marked { row.style(t.marked) } else { row };
            if pos == s.cursor && s.focus == Focus::Results {
                row.style(t.cursor)
            } else {
                row
            }
        })
        .collect();
    frame.render_widget(
        Table::new(rows, widths).header(header).column_spacing(1),
        area,
    );
    if s.rows.is_empty() {
        let hidden = run.results.len();
        let text = if hidden > 0 {
            format!(
                "{} hidden by the filters — {} clears them",
                format::plural(hidden, "result"),
                state.keymap.label(
                    crate::keymap::Context::Search,
                    crate::action::Action::ClearFilter
                )
            )
        } else if run.finished {
            "No results".to_owned()
        } else {
            "Waiting for results…".to_owned()
        };
        render_hint(frame, area, state, &text);
    }
}

fn render_hint(frame: &mut Frame<'_>, area: Rect, state: &State, text: &str) {
    if area.height < 3 {
        return;
    }
    let msg = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    frame.render_widget(
        Paragraph::new(Span::styled(text.to_owned(), state.theme.dim)).alignment(Alignment::Center),
        msg,
    );
}

fn render_history(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let s = &state.search;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(t.border)
        .title(Span::styled(
            " History · Enter re-runs · x clears · h closes ",
            t.accent,
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if s.history.is_empty() {
        render_hint(frame, inner, state, "No searches yet");
        return;
    }
    let height = usize::from(inner.height);
    let top = (s.hist_cursor + 1).saturating_sub(height);
    let w = usize::from(inner.width);
    let lines: Vec<Line<'static>> = s
        .history
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(i, e)| {
            let when = jiff::Timestamp::from_second(e.at)
                .map(|t| {
                    t.to_zoned(state.time_zone.clone())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            let mut what = e.query.clone();
            if let Some(c) = &e.category {
                what.push_str(&format!("  [{c}]"));
            }
            if !e.providers.is_empty() {
                what.push_str(&format!("  @{}", e.providers.join(",")));
            }
            let tail = format!("{:>4} results  {when}", e.result_count);
            let left = w.saturating_sub(tail.chars().count() + 1);
            let text = format!("{:<left$} {tail}", truncate(&what, left));
            let style = if i == s.hist_cursor { t.cursor } else { t.text };
            Line::styled(text, style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Draw the search view.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let a = areas(area, state);
    render_form(frame, a.form, state);
    let lines = status_lines(&status_segments(state), usize::from(a.status.width));
    frame.render_widget(Paragraph::new(lines), a.status);
    render_filters(frame, a.filters, state);
    if state.search.history_open {
        render_history(frame, a.table, state);
    } else {
        render_table(frame, a.table, state);
    }
}
