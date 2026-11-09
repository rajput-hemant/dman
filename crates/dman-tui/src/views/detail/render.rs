//! Drawing of the detail view.

use dman_proto::{Job, JobDetail, PieceMap};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs};

use super::{DetailState, Tab, option_rows};
use crate::app::State;
use crate::format;
use crate::theme::Theme;

fn field(t: &Theme, name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:>9}  "), t.dim),
        Span::raw(value),
    ])
}

fn header_lines(t: &Theme, j: &Job, d: &DetailState, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(11);
    let mut v = vec![
        Line::from(vec![
            Span::styled(format!("{:>9}  ", "Name"), t.dim),
            Span::styled(format::truncate(&j.name, room), t.accent),
        ]),
        field(
            t,
            "Status",
            format!(
                "{}  {}  {} / {}",
                format::state(j.state),
                format::progress_bar(j, 16),
                format::size(j.completed_bytes),
                format::size_or_dash(j.total_bytes)
            ),
        ),
        field(
            t,
            "Speed",
            format!(
                "▼ {}  ▲ {}  ETA {}  peers {}  ratio {}",
                format::speed(j.download_speed),
                format::speed(j.upload_speed),
                format::eta(j.eta_secs()),
                format::peers(j),
                format::ratio(j)
            ),
        ),
        field(t, "Dir", format::truncate(&j.dir, room)),
        field(t, "Source", format::truncate(&j.source, room)),
    ];
    let hash = j
        .info_hash
        .clone()
        .or_else(|| d.detail.as_ref().and_then(|x| x.info_hash.clone()));
    if let Some(h) = hash {
        v.push(field(t, "Info hash", h));
    }
    if let Some(c) = d.detail.as_ref().and_then(|x| x.comment.as_ref()) {
        v.push(field(t, "Comment", format::truncate(c, room)));
    }
    if let Some(e) = &j.error {
        v.push(Line::from(vec![
            Span::styled(format!("{:>9}  ", "Error"), t.dim),
            Span::styled(format::truncate(e, room), t.error),
        ]));
    }
    if let Some(e) = &d.error {
        v.push(Line::styled(
            format::truncate(&format!("Detail refresh failed: {e}"), width),
            t.warn,
        ));
    }
    v
}

/// Draw the detail view.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, d: &DetailState) {
    let t = &state.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border)
        .title(Span::styled(format!(" Download #{} ", d.id), t.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(job) = state.jobs.get(&d.id) else {
        frame.render_widget(
            Paragraph::new(Line::styled("This download is no longer listed.", t.warn)),
            inner,
        );
        return;
    };
    let header = header_lines(t, job, d, usize::from(inner.width));
    let h = u16::try_from(header.len()).unwrap_or(u16::MAX);
    let [head, tabs, body] = Layout::vertical([
        Constraint::Length(h),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(header), head);
    let shown = Tab::visible(super::is_torrent(state, d.id));
    let titles: Vec<String> = shown
        .iter()
        .map(|tab| format!("{} {}", tab.index() + 1, tab.label()))
        .collect();
    frame.render_widget(
        Tabs::new(titles)
            .select(shown.iter().position(|t| *t == d.tab).unwrap_or(0))
            .style(t.dim)
            .highlight_style(t.accent)
            .divider("│"),
        tabs,
    );
    match &d.detail {
        None => {
            let text = if d.error.is_some() {
                "Detail is not available."
            } else {
                "Loading…"
            };
            frame.render_widget(Paragraph::new(Line::styled(text, t.dim)), body);
        }
        Some(detail) => match d.tab {
            Tab::Files => files(frame, body, t, d, detail),
            Tab::Peers => peers(frame, body, t, d, detail),
            Tab::Trackers => trackers(frame, body, t, d, detail),
            Tab::Pieces => pieces(frame, body, t, detail),
            Tab::Speed => crate::widgets::graph::speed_chart(
                frame,
                body,
                t,
                &detail.speed_history,
                "No speed samples yet (history is kept while the download runs).",
            ),
            Tab::Options => options(frame, body, t, d, detail),
        },
    }
    if !state.connected() {
        // Stale data: grey everything out.
        frame.buffer_mut().set_style(inner, t.dim);
    }
}

/// First row to draw so that `cursor` stays visible in `height` rows.
fn window_start(len: usize, cursor: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        return 0;
    }
    cursor.saturating_sub(height / 2).min(len - height)
}

/// Rows of a table with a header, windowed around the cursor.
fn table<const N: usize>(
    frame: &mut Frame<'_>,
    area: Rect,
    t: &Theme,
    cursor: usize,
    header: [&str; N],
    widths: [Constraint; N],
    rows: Vec<([String; N], bool)>,
) {
    let height = usize::from(area.height.saturating_sub(1));
    let start = window_start(rows.len(), cursor, height);
    let body: Vec<Row<'_>> = rows
        .into_iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, (cells, dim))| {
            let style = if i == cursor {
                t.cursor
            } else if dim {
                t.dim
            } else {
                Style::new()
            };
            Row::new(cells.into_iter().map(Cell::from)).style(style)
        })
        .collect();
    let head = Row::new(header.into_iter().map(Cell::from)).style(t.header);
    frame.render_widget(Table::new(body, widths).header(head), area);
}

fn pct(done: u64, total: u64) -> String {
    done.saturating_mul(100)
        .checked_div(total)
        .map_or_else(|| "-".into(), |p| format!("{p}%"))
}

fn files(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    if detail.files.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No file list yet.", t.dim)),
            area,
        );
        return;
    }
    let rows = detail
        .files
        .iter()
        .map(|f| {
            (
                [
                    if f.selected { "[x]" } else { "[ ]" }.to_owned(),
                    f.index.to_string(),
                    f.path.clone(),
                    format::size_or_dash(f.length),
                    pct(f.completed, f.length),
                ],
                !f.selected,
            )
        })
        .collect();
    table(
        frame,
        area,
        t,
        d.cursor,
        ["", "#", "Path", "Size", "Done"],
        [
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(5),
        ],
        rows,
    );
}

fn peers(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    if detail.peers.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "No peers (only active torrents have peers).",
                t.dim,
            )),
            area,
        );
        return;
    }
    let rows = detail
        .peers
        .iter()
        .map(|p| {
            (
                [
                    format!("{}:{}", p.ip, p.port),
                    format!("{:.0}%", p.progress * 100.0),
                    format::speed_or_dash(p.download_speed),
                    format::speed_or_dash(p.upload_speed),
                    if p.seeder { "seed" } else { "" }.to_owned(),
                ],
                false,
            )
        })
        .collect();
    table(
        frame,
        area,
        t,
        d.cursor,
        ["Peer", "Has", "Down", "Up", ""],
        [
            Constraint::Fill(1),
            Constraint::Length(5),
            Constraint::Length(11),
            Constraint::Length(11),
            Constraint::Length(5),
        ],
        rows,
    );
}

fn trackers(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    let [note, list] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            "aria2 does not report tracker status, so it is always \"unknown\".",
            t.dim,
        )),
        note,
    );
    if detail.trackers.is_empty() {
        frame.render_widget(Paragraph::new("No trackers."), list);
        return;
    }
    let rows = detail
        .trackers
        .iter()
        .map(|k| {
            (
                [k.tier.to_string(), "unknown".to_owned(), k.url.clone()],
                false,
            )
        })
        .collect();
    table(
        frame,
        list,
        t,
        d.cursor,
        ["Tier", "Status", "URL"],
        [
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Fill(1),
        ],
        rows,
    );
}

/// Decode aria2's hex bitfield (MSB of the first byte = piece 0).
pub fn have_bits(hex: &str, pieces: usize) -> Vec<bool> {
    let mut out = Vec::with_capacity(pieces);
    for c in hex.chars() {
        let n = c.to_digit(16).unwrap_or(0);
        for bit in (0..4).rev() {
            if out.len() < pieces {
                out.push(n >> bit & 1 == 1);
            }
        }
    }
    out.resize(pieces, false);
    out
}

fn pieces(frame: &mut Frame<'_>, area: Rect, t: &Theme, detail: &JobDetail) {
    let Some(PieceMap {
        num_pieces,
        piece_length,
        bitfield_hex,
    }) = &detail.pieces
    else {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "No piece map (only BitTorrent and HTTP downloads that have started report one).",
                t.dim,
            )),
            area,
        );
        return;
    };
    let n = usize::try_from(*num_pieces).unwrap_or(0);
    let bits = have_bits(bitfield_hex, n);
    let have = bits.iter().filter(|b| **b).count();
    let [info, legend, grid] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(format!(
            "{n} pieces of {}  ·  {have} have ({})",
            format::size(*piece_length),
            pct(have as u64, n as u64)
        )),
        info,
    );
    let cells = usize::from(grid.width) * usize::from(grid.height);
    let grouped = n > cells;
    let mut spans = vec![
        Span::styled("█", t.ok),
        Span::raw(" have   "),
        Span::styled("░", t.dim),
        Span::raw(" missing"),
    ];
    if grouped {
        spans.extend([
            Span::raw("   "),
            Span::styled("▒", t.warn),
            Span::raw(format!(
                " partly (≈{} pieces per cell)",
                n.div_ceil(cells.max(1))
            )),
        ]);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), legend);
    if n == 0 || cells == 0 {
        return;
    }
    let shown = n.min(cells);
    let width = usize::from(grid.width);
    let lines: Vec<Line<'static>> = (0..shown)
        .collect::<Vec<_>>()
        .chunks(width)
        .map(|row| {
            Line::from(
                row.iter()
                    .map(|&c| {
                        let (a, b) = (c * n / shown, ((c + 1) * n / shown).max(c * n / shown + 1));
                        let got = bits[a..b.min(n)].iter().filter(|x| **x).count();
                        let len = b.min(n) - a;
                        if got == len {
                            Span::styled("█", t.ok)
                        } else if got == 0 {
                            Span::styled("░", t.dim)
                        } else {
                            Span::styled("▒", t.warn)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), grid);
}

fn options(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    let rows = option_rows(&detail.options);
    let cells = rows
        .into_iter()
        .map(|r| {
            let note = if r.field.is_some() { "" } else { r.note };
            (
                [r.label.to_owned(), r.value, note.to_owned()],
                r.unset || r.field.is_none(),
            )
        })
        .collect();
    table(
        frame,
        area,
        t,
        d.cursor,
        ["Option", "Value", "Note"],
        [
            Constraint::Length(23),
            Constraint::Fill(1),
            Constraint::Fill(1),
        ],
        cells,
    );
}
