//! Drawing of the detail view.

use dman_proto::{Job, JobDetail, PieceMap};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{DetailState, Tab, option_rows};
use crate::app::State;
use crate::format;
use crate::layout::Hit;
use crate::theme::{IconSet, Theme};
use crate::widgets::bar;
use crate::widgets::table::{self, Col};

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
    // Full labels with counts, then without counts, then a strip scrolled to
    // the selected tab with ‹ › marking hidden tabs.
    let label = |tab: Tab, counts: bool| {
        let count = if counts {
            tab_count(d, tab)
                .map(|n| format!(" {n}"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        format!(" {} {}{count} ", tab.index() + 1, tab.label())
    };
    let room = usize::from(area.width).saturating_sub(1);
    let total = |counts: bool| {
        shown
            .iter()
            .map(|t| format::width(&label(*t, counts)) + 1)
            .sum::<usize>()
    };
    let counts = total(true) <= room;
    let labels: Vec<(Tab, String)> = shown.iter().map(|t| (*t, label(*t, counts))).collect();
    let sel = labels
        .iter()
        .position(|(tab, _)| *tab == d.tab)
        .unwrap_or(0);
    // First tab of the window: as far left as keeps the selected one in view.
    let mut first = 0;
    let fits = |first: usize| {
        let marks = usize::from(first > 0) * 2 + 2;
        labels[first..=sel]
            .iter()
            .map(|(_, l)| format::width(l) + 1)
            .sum::<usize>()
            + marks
            <= room
    };
    while first < sel && !fits(first) {
        first += 1;
    }
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    if first > 0 {
        spans.push(Span::styled("‹ ", t.faint));
        x += 2;
    }
    let mut last = first;
    for (i, (tab, text)) in labels.iter().enumerate().skip(first) {
        let w = format::width(text);
        let more = usize::from(i + 1 < labels.len()) * 2;
        if usize::from(x - area.x) + w + more > usize::from(area.width) && i > sel {
            break;
        }
        let style = if *tab == d.tab { t.mode } else { t.dim };
        spans.push(Span::styled(text.clone(), style));
        spans.push(Span::raw(" "));
        let w16 = u16::try_from(w).unwrap_or(0);
        state.hit(Rect::new(x, area.y, w16, 1), Hit::DetailTab(*tab));
        x = x.saturating_add(w16 + 1);
        last = i;
    }
    if last + 1 < labels.len() {
        spans.push(Span::styled("›", t.faint));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Rows the summary card takes for a body of `height` rows: the full card
/// (border, summary, bar, speeds) when there is room, else two bare lines
/// (summary with the name, bar), else one.
fn card_height(height: u16) -> u16 {
    match height {
        18.. => 5,
        9..=17 => 2,
        _ => 1,
    }
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
    let card_h = card_height(area.height);
    let card = Rect {
        height: card_h.min(area.height),
        ..area
    };
    let tabs = Rect {
        y: card.bottom(),
        height: u16::from(area.height > card_h),
        ..area
    };
    let body = Rect {
        y: tabs.bottom(),
        height: area.height.saturating_sub(card_h + 1),
        ..area
    };
    let name = job.name.trim_start_matches("[METADATA]");
    if card_h == 5 {
        let engine = match job.engine {
            dman_proto::Engine::Aria2 => "aria2",
            dman_proto::Engine::YtDlp => "yt-dlp",
        };
        let right = format!(" #{} · {engine} ", d.id);
        let title_room = usize::from(area.width)
            .saturating_sub(format::width(&right) + 6)
            .max(8);
        let block = t
            .block()
            .title(Span::styled(
                format!(" {} ", format::middle(name, title_room)),
                t.accent,
            ))
            .title(
                Line::from(Span::styled(right, t.faint))
                    .alignment(ratatui::layout::Alignment::Right),
            );
        let card_inner = block.inner(card).inner(Margin::new(1, 0));
        frame.render_widget(block, card);
        frame.render_widget(
            Paragraph::new(card_lines(state, job, d, usize::from(card_inner.width))),
            card_inner,
        );
    } else {
        // Compact: the breadcrumb already names the download; the bar and
        // what is happening matter most.
        let inner = card.inner(Margin::new(1, 0));
        let mut lines = card_lines(state, job, d, usize::from(inner.width));
        lines.truncate(2);
        if card_h == 1 {
            lines.remove(0);
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }
    render_tabs(frame, tabs, state, d);
    let block = t.block_focus(true);
    let inner = block.inner(body);
    frame.render_widget(block, body);
    let mut body = inner;
    if let Some(e) = &d.error {
        let warn = Rect {
            height: body.height.min(1),
            ..body
        };
        let rest = Rect {
            y: warn.bottom(),
            height: body.height.saturating_sub(1),
            ..body
        };
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
            Tab::Speed => speed_tab(frame, body, state, &detail.speed_history),
            Tab::Options => options(frame, body, t, d, detail),
            Tab::Info => {}
        },
    }
    if !state.connected() {
        // Stale data: grey everything out.
        frame.buffer_mut().set_style(area, t.dim);
    }
}

/// The Speed tab: window labels on top, the chart below.
fn speed_tab(
    frame: &mut Frame<'_>,
    body: Rect,
    state: &State,
    history: &[dman_proto::SpeedSample],
) {
    use crate::widgets::graph;
    let t = &state.theme;
    let (shown, available) = graph::windowed(history, state.graph.window);
    let chart = if body.height >= 4 {
        let [bar, chart] = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Fill(1),
        ])
        .areas(body);
        let mut spans = vec![Span::styled(" Window ", t.faint)];
        let used: usize = spans.iter().map(Span::width).sum();
        spans.extend(graph::selector(
            state,
            bar.x + u16::try_from(used).unwrap_or(0),
            bar.y,
            available,
            usize::from(bar.width).saturating_sub(used),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), bar);
        chart
    } else {
        body
    };
    graph::speed_chart(
        frame,
        chart,
        t,
        shown,
        "No speed samples yet (history is kept while the download runs).",
    );
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
    let bar_w: u16 = match area.width {
        100.. => 16,
        60..=99 => 8,
        _ => 0,
    };
    let cols = [
        Col::fixed("", 1, 0),
        Col::fixed("#", 4, 3),
        Col::fill("Path", 10, 0),
        Col::fixed("Size", 10, 2).right(),
        Col::fixed("Done", bar_w + 5, 1),
    ];
    let dir = state
        .jobs
        .get(&d.id)
        .map(|j| j.dir.as_str())
        .unwrap_or_default();
    let shown = table::layout(&cols, area.width);
    let path_w = shown
        .iter()
        .find(|(i, _)| *i == 2)
        .map_or(10, |(_, w)| usize::from(*w));
    let height = usize::from(area.height.saturating_sub(1));
    let start = table::window_start(detail.files.len(), d.cursor, height);
    let head: Vec<table::Cell> = cols
        .iter()
        .map(|c| (c.title.to_owned(), t.header))
        .collect();
    let mut lines = vec![table::line(&cols, &shown, &head, Style::new())];
    for (i, f) in detail.files.iter().enumerate().skip(start).take(height) {
        let base = if i == d.cursor {
            t.cursor
        } else if f.selected {
            Style::new()
        } else {
            t.dim
        };
        let check = if f.selected { "☑" } else { "☐" };
        let cells: Vec<table::Cell> = vec![
            (check.to_owned(), if f.selected { t.ok } else { t.faint }),
            (f.index.to_string(), t.faint),
            (
                format::middle(&display_path(&f.path, dir), path_w),
                Style::new(),
            ),
            (format::size_or_dash(f.length), Style::new()),
            (String::new(), Style::new()),
        ];
        let mut line = table::line(&cols, &shown, &cells, base);
        // The Done column is drawn as a bar plus a percentage.
        if shown.iter().any(|(c, _)| *c == 4) {
            line.spans.pop();
            #[allow(clippy::cast_precision_loss)]
            let frac = if f.length == 0 {
                0.0
            } else {
                f.completed as f64 / f.length as f64
            };
            if f.selected {
                if bar_w > 0 {
                    line.spans
                        .extend(bar::bar(t, usize::from(bar_w), frac, t.info));
                }
                line.spans.push(Span::styled(
                    format!(" {:>4}", pct(f.completed, f.length)),
                    base.patch(t.text),
                ));
            } else {
                line.spans.push(Span::styled(
                    format::fit("skipped", usize::from(bar_w) + 5),
                    base.patch(t.faint),
                ));
            }
        }
        lines.push(line.style(base));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn peers(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    if detail.peers.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "No peers (only active torrents have peers).",
                t.dim,
            ))
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let cols = [
        Col::fill("Peer", 15, 0),
        Col::fixed("Has", 4, 2).right(),
        Col::fixed("Down", 11, 0).right(),
        Col::fixed("Up", 11, 1).right(),
        Col::fixed("", 4, 3),
    ];
    let rows: Vec<(Vec<table::Cell>, Style)> = detail
        .peers
        .iter()
        .map(|p| {
            let plain = |s: String| (s, Style::new());
            (
                vec![
                    plain(format!("{}:{}", p.ip, p.port)),
                    plain(format!("{:.0}%", p.progress * 100.0)),
                    plain(format::speed_or_dash(p.download_speed)),
                    plain(format::speed_or_dash(p.upload_speed)),
                    (if p.seeder { "seed" } else { "" }.to_owned(), t.faint),
                ],
                Style::new(),
            )
        })
        .collect();
    table::render(frame, area, t, &cols, &rows, Some(d.cursor));
}

fn trackers(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    let note = Rect {
        height: area.height.min(1),
        ..area
    };
    let list = Rect {
        y: note.bottom(),
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            format::truncate(
                "aria2 does not report tracker status, so it is always \"unknown\".",
                usize::from(note.width),
            ),
            t.dim,
        )),
        note,
    );
    if detail.trackers.is_empty() {
        frame.render_widget(Paragraph::new("No trackers."), list);
        return;
    }
    let cols = [
        Col::fixed("Tier", 4, 1),
        Col::fixed("Status", 8, 2),
        Col::fill("URL", 12, 0),
    ];
    let rows: Vec<(Vec<table::Cell>, Style)> = detail
        .trackers
        .iter()
        .map(|k| {
            (
                vec![
                    (k.tier.to_string(), Style::new()),
                    ("unknown".to_owned(), t.faint),
                    (k.url.clone(), Style::new()),
                ],
                Style::new(),
            )
        })
        .collect();
    table::render(frame, list, t, &cols, &rows, Some(d.cursor));
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
            ))
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    let n = usize::try_from(*num_pieces).unwrap_or(0);
    let bits = have_bits(bitfield_hex, n);
    let have = bits.iter().filter(|b| **b).count();
    let info = Rect {
        height: area.height.min(1),
        ..area
    };
    let legend = Rect {
        y: info.bottom(),
        height: u16::from(area.height >= 3),
        ..area
    };
    let grid = Rect {
        y: legend.bottom(),
        height: area.height.saturating_sub(info.height + legend.height),
        ..area
    };
    frame.render_widget(
        Paragraph::new(format::truncate(
            &format!(
                "{n} pieces of {}  ·  {have} have ({})",
                format::size(*piece_length),
                pct(have as u64, n as u64)
            ),
            usize::from(area.width),
        )),
        info,
    );
    let map = PieceGrid::new(
        &bits,
        grid.width,
        grid.height,
        t.icons.set == IconSet::Ascii,
    );
    frame.render_widget(Paragraph::new(Line::from(map.legend(t))), legend);
    frame.render_widget(Paragraph::new(map.lines(t)), grid);
}

/// How the piece map is drawn: one cell per piece when they fit, else
/// braille dots (2 × 4 per cell, one piece or one group of pieces per dot,
/// in reading order), grouped further only beyond that.
struct PieceGrid<'a> {
    bits: &'a [bool],
    width: usize,
    height: usize,
    /// One cell per piece (block characters).
    blocks: bool,
    /// Pieces per drawn unit (cell or dot).
    group: usize,
}

impl<'a> PieceGrid<'a> {
    fn new(bits: &'a [bool], width: u16, height: u16, ascii: bool) -> Self {
        let (width, height) = (usize::from(width), usize::from(height));
        let cells = width * height;
        let n = bits.len();
        let blocks = ascii || n <= cells;
        let units = if blocks { cells } else { cells * 8 };
        Self {
            bits,
            width,
            height,
            blocks,
            group: n.div_ceil(units.max(1)).max(1),
        }
    }

    /// (pieces had, pieces) of unit `u`.
    fn unit(&self, u: usize) -> (usize, usize) {
        let from = (u * self.group).min(self.bits.len());
        let to = (from + self.group).min(self.bits.len());
        let slice = &self.bits[from..to];
        (slice.iter().filter(|b| **b).count(), slice.len())
    }

    fn legend(&self, t: &Theme) -> Vec<Span<'static>> {
        let mut spans = if self.blocks {
            vec![
                Span::styled("█", t.info),
                Span::styled(" have   ", t.dim),
                Span::styled("░", t.faint),
                Span::styled(" missing", t.dim),
            ]
        } else {
            let unit = if self.group == 1 {
                "a piece".to_owned()
            } else {
                format!("{} pieces", self.group)
            };
            vec![
                Span::styled("⣿", t.info),
                Span::styled(format!(" each dot is {unit} you have"), t.dim),
            ]
        };
        if self.blocks && self.group > 1 {
            spans.extend([
                Span::raw("   "),
                Span::styled("▓", t.info),
                Span::raw(format!(" partly (≈{} pieces per cell)", self.group)),
            ]);
        }
        spans
    }

    fn lines(&self, t: &Theme) -> Vec<Line<'static>> {
        if self.bits.is_empty() || self.width == 0 {
            return Vec::new();
        }
        let units = self.bits.len().div_ceil(self.group);
        let mut out = Vec::new();
        if self.blocks {
            for row in 0..self.height {
                let mut spans = Vec::new();
                for col in 0..self.width {
                    let u = row * self.width + col;
                    if u >= units {
                        break;
                    }
                    let (got, len) = self.unit(u);
                    spans.push(if got == len {
                        Span::styled("█", t.info)
                    } else if got == 0 {
                        Span::styled("░", t.faint)
                    } else {
                        Span::styled("▓", t.info)
                    });
                }
                if spans.is_empty() {
                    break;
                }
                out.push(Line::from(spans));
            }
            return out;
        }
        // Braille: dot (x, y) of the whole grid shows unit y * dots_w + x.
        const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        let dots_w = self.width * 2;
        for row in 0..self.height {
            let mut spans = Vec::new();
            for col in 0..self.width {
                let (mut lit, mut first) = (0u32, true);
                for (dx, column) in BITS.iter().enumerate() {
                    for (dy, bit) in column.iter().enumerate() {
                        let u = (row * 4 + dy) * dots_w + col * 2 + dx;
                        if u >= units {
                            continue;
                        }
                        let (got, len) = self.unit(u);
                        if got == len {
                            lit |= bit;
                        }
                        first = false;
                    }
                }
                if first {
                    break;
                }
                let ch = char::from_u32(0x2800 + lit).unwrap_or(' ');
                spans.push(Span::styled(ch.to_string(), t.info));
            }
            if spans.is_empty() {
                break;
            }
            out.push(Line::from(spans));
        }
        out
    }
}

fn options(frame: &mut Frame<'_>, area: Rect, t: &Theme, d: &DetailState, detail: &JobDetail) {
    let cols = [
        Col::fixed("Option", 22, 0),
        Col::fill("Value", 10, 0),
        Col::fill("Note", 16, 1),
    ];
    let rows: Vec<(Vec<table::Cell>, Style)> = option_rows(&detail.options)
        .into_iter()
        .map(|r| {
            let note = if r.field.is_some() { "" } else { r.note };
            let style = if r.unset || r.field.is_none() {
                t.dim
            } else {
                Style::new()
            };
            (
                vec![
                    (r.label.to_owned(), Style::new()),
                    (r.value, Style::new()),
                    (note.to_owned(), t.faint),
                ],
                style,
            )
        })
        .collect();
    table::render(frame, area, t, &cols, &rows, Some(d.cursor));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn piece_map_uses_blocks_then_braille_then_groups() {
        let t = Theme::default_theme();
        // Fits one cell per piece.
        let bits = [true, false, true];
        let g = PieceGrid::new(&bits, 10, 2, false);
        assert!(g.blocks);
        assert_eq!(text(&g.lines(&t)), ["█░█"]);
        // 16 pieces in 2 cells: one braille dot each, in reading order
        // (the first dot row is pieces 0-3 across both cells).
        let mut bits = vec![false; 16];
        bits[0] = true; // cell 0, left column, top dot
        bits[3] = true; // cell 1, right column, top dot
        let g = PieceGrid::new(&bits, 2, 1, false);
        assert!(!g.blocks && g.group == 1);
        assert_eq!(text(&g.lines(&t)), ["\u{2801}\u{2808}"]);
        // More pieces than dots: groups, complete groups only are lit.
        let g = PieceGrid::new(&[true; 40], 2, 1, false);
        assert_eq!(g.group, 3);
        // 14 groups: the last cell has no third and fourth dot of the bottom row.
        assert_eq!(text(&g.lines(&t)), ["⣿⠿"]);
        // ASCII icon sets keep the block map.
        assert!(PieceGrid::new(&[true; 40], 2, 1, true).blocks);
    }
}
