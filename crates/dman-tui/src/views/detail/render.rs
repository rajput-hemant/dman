//! Drawing of the detail view.

use dman_proto::{Job, JobDetail, PieceMap};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Wrap};

use super::{DetailState, Tab, option_rows};
use crate::app::State;
use crate::format;
use crate::layout::Hit;
use crate::theme::Theme;
use crate::widgets::bar;

fn field(t: &Theme, name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<11}"), t.faint),
        Span::styled(value, t.text),
    ])
}

/// The live summary card: what is happening, the bar, speeds.
fn card_lines(state: &State, j: &Job, d: &DetailState, width: usize) -> Vec<Line<'static>> {
    let t = &state.theme;
    let now = super::summary(state, j, d.detail.as_ref());
    let st = t.state(j.state);
    let mut out = vec![Line::from(vec![
        Span::styled(format!("{} ", t.icons.state(j.state)), st),
        Span::styled(
            format::truncate(&now, width.saturating_sub(2)),
            st.add_modifier(Modifier::BOLD),
        ),
    ])];
    let sizes = if j.total_bytes > 0 {
        format!(
            "  {} / {}",
            format::size(j.completed_bytes),
            format::size(j.total_bytes)
        )
    } else {
        format!("  {} / ?", format::size(j.completed_bytes))
    };
    let pct = if j.total_bytes > 0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let p = (j.progress().clamp(0.0, 1.0) * 100.0).floor() as u32;
        format!(" {p:>3}%")
    } else {
        String::new()
    };
    let bar_w = width
        .saturating_sub(sizes.chars().count() + pct.len() + 1)
        .max(4);
    let mut bar_line = if j.total_bytes == 0 && j.state == dman_proto::JobState::Active {
        bar::shuttle(t, bar_w, state.anim_ms(), st)
    } else {
        bar::bar(t, bar_w, state.shown_progress(j), st)
    };
    bar_line.push(Span::styled(pct, t.text.add_modifier(Modifier::BOLD)));
    bar_line.push(Span::styled(sizes, t.dim));
    out.push(Line::from(bar_line));
    let history = state.job_speeds.get(&j.id).map_or(&[][..], Vec::as_slice);
    let (dn, du) = format::split_size(j.download_speed, true, false);
    let (un, uu) = format::split_size(j.upload_speed, true, false);
    let mut speed = vec![
        Span::styled(format!("{} ", t.icons.down), t.graph_down),
        Span::styled(dn, t.text),
        Span::styled(du, t.faint),
        Span::raw(" "),
        Span::styled(bar::sparkline(history, 12, 0), t.graph_down),
        Span::raw("   "),
        Span::styled(format!("{} ", t.icons.up), t.graph_up),
        Span::styled(un, t.text),
        Span::styled(uu, t.faint),
    ];
    let mut extra = |k: &str, v: String| {
        speed.push(Span::styled(format!("   {k} "), t.faint));
        speed.push(Span::styled(v, t.text));
    };
    if j.seeders.is_some() || j.connections > 0 {
        extra("peers", format::peers(j));
    }
    if j.uploaded_bytes > 0 {
        extra("ratio", format::ratio(j));
    }
    out.push(Line::from(speed));
    out
}

fn info_lines(state: &State, j: &Job, d: &DetailState, width: usize) -> Vec<Line<'static>> {
    let t = &state.theme;
    let room = width.saturating_sub(11);
    let mut v = vec![
        field(
            t,
            "Name",
            format::middle(j.name.trim_start_matches("[METADATA]"), room),
        ),
        field(t, "Folder", format::middle(&j.dir, room)),
        field(t, "Source", format::middle(&j.source, room)),
        field(
            t,
            "Engine",
            match j.engine {
                dman_proto::Engine::Aria2 => "aria2".to_owned(),
                dman_proto::Engine::YtDlp => "yt-dlp (video)".to_owned(),
            },
        ),
        field(t, "Added", format::ago(state.now_secs() - j.added_at)),
    ];
    if let Some(at) = j.completed_at {
        v.push(field(t, "Finished", format::ago(state.now_secs() - at)));
    }
    let hash = j
        .info_hash
        .clone()
        .or_else(|| d.detail.as_ref().and_then(|x| x.info_hash.clone()));
    if let Some(h) = hash {
        v.push(field(t, "Info hash", h));
    }
    if let Some(c) = d.detail.as_ref().and_then(|x| x.comment.as_ref()) {
        v.push(field(t, "Comment", c.clone()));
    }
    if let Some(e) = &j.error {
        v.push(Line::from(vec![
            Span::styled(format!("{:<11}", "Error"), t.faint),
            Span::styled(format::truncate(e, room.max(10) * 3), t.error),
        ]));
    }
    v
}

/// A file path relative to the download folder when it lies inside it.
fn display_path(path: &str, dir: &str) -> String {
    let dir = dir.trim_end_matches('/');
    match path.strip_prefix(dir) {
        Some(rest) if !dir.is_empty() && rest.starts_with('/') => rest[1..].to_owned(),
        _ => path.to_owned(),
    }
}

fn tab_count(d: &DetailState, tab: Tab) -> Option<usize> {
    let detail = d.detail.as_ref()?;
    match tab {
        Tab::Files => Some(detail.files.len()),
        Tab::Peers => Some(detail.peers.len()),
        Tab::Trackers => Some(detail.trackers.len()),
        _ => None,
    }
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, state: &State, d: &DetailState) {
    let t = &state.theme;
    let shown = Tab::visible(super::is_torrent(state, d.id));
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    for tab in shown {
        let count = tab_count(d, tab)
            .map(|n| format!(" {n}"))
            .unwrap_or_default();
        let label = format!(" {} {}", tab.index() + 1, tab.label());
        let selected = tab == d.tab;
        let w = label.chars().count() + count.chars().count() + 1;
        if usize::from(x - area.x) + w > usize::from(area.width) {
            break;
        }
        let (ls, cs) = if selected {
            (t.mode, t.mode)
        } else {
            (t.dim, t.faint)
        };
        spans.push(Span::styled(label, ls));
        spans.push(Span::styled(count, cs));
        spans.push(Span::styled(" ", ls));
        spans.push(Span::raw(" "));
        let w16 = u16::try_from(w).unwrap_or(0);
        state.hit(Rect::new(x, area.y, w16, 1), Hit::DetailTab(tab));
        x += w16 + 1;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw the detail view: summary card, tab bar and the tab's body.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, d: &DetailState) {
    let t = &state.theme;
    let Some(job) = state.jobs.get(&d.id) else {
        let block = t
            .block()
            .title(Span::styled(format!(" Download #{} ", d.id), t.accent));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::styled("This download is no longer listed.", t.warn)),
            inner,
        );
        return;
    };
    let [card, tabs, body] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let engine = match job.engine {
        dman_proto::Engine::Aria2 => "aria2",
        dman_proto::Engine::YtDlp => "yt-dlp",
    };
    let title_room = usize::from(area.width).saturating_sub(24);
    let block = t
        .block()
        .title(Span::styled(
            format!(
                " {} ",
                format::middle(job.name.trim_start_matches("[METADATA]"), title_room)
            ),
            t.accent,
        ))
        .title(
            Line::from(Span::styled(format!(" #{} · {engine} ", d.id), t.faint))
                .alignment(ratatui::layout::Alignment::Right),
        );
    let card_inner = block.inner(card).inner(Margin::new(1, 0));
    frame.render_widget(block, card);
    frame.render_widget(
        Paragraph::new(card_lines(state, job, d, usize::from(card_inner.width))),
        card_inner,
    );
    render_tabs(frame, tabs, state, d);
    let block = t.block_focus(true);
    let inner = block.inner(body);
    frame.render_widget(block, body);
    let mut body = inner;
    if let Some(e) = &d.error {
        let [warn, rest] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(body);
        frame.render_widget(
            Paragraph::new(Line::styled(
                format::truncate(
                    &format!("Detail refresh failed: {e}"),
                    usize::from(warn.width),
                ),
                t.warn,
            )),
            warn,
        );
        body = rest;
    }
    let waiting_meta = super::awaiting_metadata(job, d.detail.as_ref())
        && matches!(d.tab, Tab::Files | Tab::Peers | Tab::Trackers | Tab::Pieces);
    match &d.detail {
        _ if d.tab == Tab::Info => {
            frame.render_widget(
                Paragraph::new(info_lines(state, job, d, usize::from(body.width)))
                    .wrap(Wrap { trim: false }),
                body,
            );
        }
        _ if waiting_meta => {
            let spin = state
                .anim_ms()
                .map_or(t.icons.spinner[0], |ms| t.icons.spin(ms));
            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{spin} "), t.accent),
                    Span::styled("Waiting for the torrent's metadata.", t.accent),
                ]),
                Line::default(),
                Line::styled(
                    "A magnet link only names the torrent; the list of files, its",
                    t.dim,
                ),
                Line::styled(
                    "size and pieces come from other peers. They appear here as soon",
                    t.dim,
                ),
                Line::styled(
                    "as they arrive (usually within a minute; longer with few peers).",
                    t.dim,
                ),
            ];
            frame.render_widget(Paragraph::new(lines), body);
        }
        None => {
            let text = if d.error.is_some() {
                "Detail is not available."
            } else {
                "Loading…"
            };
            frame.render_widget(Paragraph::new(Line::styled(text, t.dim)), body);
        }
        Some(detail) => match d.tab {
            Tab::Files => files(frame, body, state, d, detail),
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
            Tab::Info => {}
        },
    }
    if !state.connected() {
        // Stale data: grey everything out.
        frame.buffer_mut().set_style(area, t.dim);
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

fn files(frame: &mut Frame<'_>, area: Rect, state: &State, d: &DetailState, detail: &JobDetail) {
    let t = &state.theme;
    if detail.files.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No file list yet.", t.dim)),
            area,
        );
        return;
    }
    let height = usize::from(area.height.saturating_sub(1));
    let start = window_start(detail.files.len(), d.cursor, height);
    let bar_w = if area.width >= 100 { 16 } else { 8 };
    let head = Row::new(vec![
        Cell::from(""),
        Cell::from("#"),
        Cell::from("Path"),
        Cell::from(format!("{:>10}", "Size")),
        Cell::from("Done"),
    ])
    .style(t.header);
    let rows: Vec<Row<'_>> = detail
        .files
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, f)| {
            let check = if f.selected { "☑" } else { "☐" };
            #[allow(clippy::cast_precision_loss)]
            let frac = if f.length == 0 {
                0.0
            } else {
                f.completed as f64 / f.length as f64
            };
            let mut done = if f.selected {
                bar::bar(t, bar_w, frac, t.info)
            } else {
                vec![Span::styled(format!("{:<bar_w$}", "skipped"), t.faint)]
            };
            if f.selected {
                done.push(Span::styled(
                    format!(" {:>4}", pct(f.completed, f.length)),
                    t.text,
                ));
            }
            let style = if i == d.cursor {
                t.cursor
            } else if f.selected {
                Style::new()
            } else {
                t.dim
            };
            Row::new(vec![
                Cell::from(Span::styled(check, if f.selected { t.ok } else { t.faint })),
                Cell::from(Span::styled(f.index.to_string(), t.faint)),
                Cell::from(display_path(
                    &f.path,
                    &state
                        .jobs
                        .get(&d.id)
                        .map(|j| j.dir.clone())
                        .unwrap_or_default(),
                )),
                Cell::from(format!("{:>10}", format::size_or_dash(f.length))),
                Cell::from(Line::from(done)),
            ])
            .style(style)
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Fill(1),
        Constraint::Length(10),
        Constraint::Length(u16::try_from(bar_w + 5).unwrap_or(13)),
    ];
    frame.render_widget(Table::new(rows, widths).header(head), area);
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
        Span::styled("█", t.info),
        Span::styled(" have   ", t.dim),
        Span::styled("░", t.faint),
        Span::styled(" missing", t.dim),
    ];
    if grouped {
        spans.extend([
            Span::raw("   "),
            Span::styled("▓", t.info),
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
                            Span::styled("█", t.info)
                        } else if got == 0 {
                            Span::styled("░", t.faint)
                        } else {
                            Span::styled("▓", t.info)
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
