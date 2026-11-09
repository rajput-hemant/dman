//! Drawing of the downloads view: the sectioned list (one- or two-line rows
//! with state glyphs, sub-cell bars and dim units), the preview pane and the
//! empty states.

use dman_proto::{Job, JobId, JobState};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::action::Action;
use crate::app::State;
use crate::format;
use crate::keymap::Context;
use crate::layout::{Hit, split_preview};
use crate::list::{Row, Section};
use crate::theme::Theme;
use crate::widgets::{bar, scrollbar};

/// Right-hand fields of a row, most important first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Progress,
    Speed,
    Eta,
    Size,
    Peers,
    Up,
    Ratio,
}

/// Which fields fit next to a name of at least [`MIN_NAME`] columns, with
/// their widths; `compact` units below 100 columns.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Columns {
    compact: bool,
    bar: usize,
    cols: Vec<(Col, usize)>,
}

const MIN_NAME: usize = 18;
/// Cursor bar, mark, state glyph and a space.
const GUTTER: usize = 4;

fn columns(width: usize) -> Columns {
    let compact = width < 100;
    let bar = if width >= 140 {
        16
    } else if width >= 100 {
        12
    } else {
        10
    };
    let w = |c: Col| match (c, compact) {
        (Col::Progress, _) => bar + 5,
        (Col::Speed | Col::Up, true) => 7,
        (Col::Speed | Col::Up, false) => 11,
        (Col::Eta, _) => 7,
        (Col::Size, true) => 7,
        (Col::Size, false) => 10,
        (Col::Peers, _) => 8,
        (Col::Ratio, _) => 6,
    };
    let mut used = GUTTER + MIN_NAME;
    let mut cols = Vec::new();
    for c in [
        Col::Progress,
        Col::Speed,
        Col::Eta,
        Col::Size,
        Col::Peers,
        Col::Up,
        Col::Ratio,
    ] {
        let need = w(c) + 1;
        // Peers, upload and ratio only on wider lists (two-line rows and
        // the preview show them otherwise).
        let wide_only = match c {
            Col::Peers => width < 88,
            Col::Up | Col::Ratio => width < 130,
            _ => false,
        };
        if !wide_only && used + need <= width {
            used += need;
            cols.push((c, w(c)));
        }
    }
    Columns { compact, bar, cols }
}

/// A right-aligned number with a dim unit (`12.0` + ` MiB/s`).
fn num_unit(
    t: &Theme,
    num: String,
    unit: String,
    width: usize,
    style: Style,
) -> Vec<Span<'static>> {
    let len = num.chars().count() + unit.chars().count();
    vec![
        Span::raw(" ".repeat(width.saturating_sub(len))),
        Span::styled(num, style),
        Span::styled(unit, t.faint),
    ]
}

fn dash(t: &Theme, width: usize) -> Vec<Span<'static>> {
    vec![Span::styled(format!("{:>width$}", "–"), t.faint)]
}

fn speed_cell(
    t: &Theme,
    bps: u64,
    width: usize,
    compact: bool,
    style: Style,
) -> Vec<Span<'static>> {
    if bps == 0 {
        return dash(t, width);
    }
    let (n, u) = format::split_size(bps, true, compact);
    num_unit(t, n, u, width, style)
}

fn size_cell(t: &Theme, bytes: u64, width: usize, compact: bool) -> Vec<Span<'static>> {
    if bytes == 0 {
        return dash(t, width);
    }
    let (n, u) = format::split_size(bytes, false, compact);
    num_unit(t, n, u, width, t.text)
}

/// The progress cell: bar plus percentage, or a shuttle for unknown sizes.
fn progress_cell(state: &State, j: &Job, bar_w: usize) -> Vec<Span<'static>> {
    let t = &state.theme;
    let fill = t.state(j.state);
    let mut out;
    if j.total_bytes == 0 && j.state == JobState::Active {
        out = bar::shuttle(t, bar_w, state.anim_ms(), fill);
        out.push(Span::styled("    ?", t.faint));
    } else if j.total_bytes == 0 {
        out = bar::bar(t, bar_w, 0.0, fill);
        out.push(Span::styled("    –", t.faint));
    } else {
        let p = state.shown_progress(j);
        out = bar::bar(t, bar_w, p, fill);
        // Truncation intended: 99.9% is not done.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (j.progress().clamp(0.0, 1.0) * 100.0).floor() as u32;
        out.push(Span::styled(format!(" {pct:>3}%"), t.text));
    }
    out
}

/// Text that replaces the speed / ETA fields for jobs that do not transfer.
fn state_note(state: &State, j: &Job) -> Option<(String, Style)> {
    let t = &state.theme;
    match j.state {
        JobState::Active | JobState::Seeding => None,
        JobState::Waiting => Some((
            match j.queue_position {
                Some(n) => format!("queued #{}", n + 1),
                None => "waiting".into(),
            },
            t.dim,
        )),
        JobState::Paused => Some(("paused".into(), t.warn)),
        JobState::Complete => {
            let when = j
                .completed_at
                .map(|at| format!(" · {}", format::ago(state.now_secs() - at)))
                .unwrap_or_default();
            Some((format!("done{when}"), t.ok))
        }
        JobState::Error => Some((j.error.clone().unwrap_or_else(|| "failed".into()), t.error)),
        JobState::Removed => Some(("removed".into(), t.dim)),
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn fit(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let w = spans_width(&spans);
    if w < width {
        spans.push(Span::raw(" ".repeat(width - w)));
    }
    spans
}

/// The first line of a job row (without the gutter).
fn job_line(state: &State, j: &Job, width: usize, layout: &Columns) -> Vec<Span<'static>> {
    let t = &state.theme;
    let fixed: usize = layout.cols.iter().map(|(_, w)| w + 1).sum();
    let name_w = width.saturating_sub(fixed).max(4);
    let video = j.kind == dman_proto::SourceKind::Video || j.engine == dman_proto::Engine::YtDlp;
    let mut name = Vec::new();
    let mut nw = name_w;
    if video {
        name.push(Span::styled(format!("{} ", t.icons.video), t.marked));
        nw = nw.saturating_sub(2);
    }
    let shown_name = j.name.trim_start_matches("[METADATA]");
    let name_style = if j.state == JobState::Removed {
        t.dim
    } else {
        t.text
    };
    name.push(Span::styled(format::middle(shown_name, nw), name_style));
    let mut spans = fit(name, name_w);
    let note = state_note(state, j);
    let mut i = 0;
    while i < layout.cols.len() {
        let (col, w) = layout.cols[i];
        spans.push(Span::raw(" "));
        // Errors replace progress and the transfer figures with the reason.
        if let Some((text, style)) = &note {
            let replaces = |c: Col| {
                matches!(c, Col::Speed | Col::Eta)
                    || (j.state == JobState::Error && c == Col::Progress)
            };
            if replaces(col) {
                let mut span_w = w;
                let mut k = i + 1;
                while k < layout.cols.len() && replaces(layout.cols[k].0) {
                    span_w += layout.cols[k].1 + 1;
                    k += 1;
                }
                let text = format::truncate(text, span_w);
                let pad = span_w.saturating_sub(text.chars().count());
                if j.state == JobState::Error {
                    spans.push(Span::styled(text, *style));
                    spans.push(Span::raw(" ".repeat(pad)));
                } else {
                    spans.push(Span::raw(" ".repeat(pad)));
                    spans.push(Span::styled(text, *style));
                }
                i = k;
                continue;
            }
        }
        let cell = match col {
            Col::Progress => progress_cell(state, j, layout.bar),
            Col::Speed if j.state == JobState::Seeding => {
                speed_cell(t, j.upload_speed, w, layout.compact, t.ok)
            }
            Col::Speed => speed_cell(t, j.download_speed, w, layout.compact, t.info),
            Col::Eta if j.state == JobState::Seeding => {
                vec![Span::styled(
                    format!("{:>w$}", format!("r{}", format::ratio(j))),
                    t.ok,
                )]
            }
            Col::Eta => match j.eta_secs() {
                Some(_) => vec![Span::styled(
                    format!("{:>w$}", format::eta(j.eta_secs())),
                    t.text,
                )],
                None => dash(t, w),
            },
            Col::Size => size_cell(t, j.total_bytes, w, layout.compact),
            Col::Peers => {
                let p = format::peers(j);
                if p == "-" {
                    dash(t, w)
                } else {
                    vec![Span::styled(format!("{p:>w$}"), t.dim)]
                }
            }
            Col::Up => speed_cell(t, j.upload_speed, w, layout.compact, t.ok),
            Col::Ratio => {
                let r = format::ratio(j);
                if r == "-" {
                    dash(t, w)
                } else {
                    vec![Span::styled(format!("{r:>w$}"), t.dim)]
                }
            }
        };
        spans.extend(fit(cell, w));
        i += 1;
    }
    spans
}

/// The second line of a tall row: sparkline and secondary facts.
fn detail_line(state: &State, j: &Job, width: usize) -> Vec<Span<'static>> {
    let t = &state.theme;
    let mut spans = Vec::new();
    let spark_w = if width >= 90 { 16 } else { 10 };
    let history = state.job_speeds.get(&j.id).map_or(&[][..], Vec::as_slice);
    if history.iter().any(|v| *v > 0) {
        let style = if j.state == JobState::Seeding {
            t.graph_up
        } else {
            t.graph_down
        };
        spans.push(Span::styled(bar::sparkline(history, spark_w, 0), style));
        spans.push(Span::raw("  "));
    }
    let mut facts: Vec<String> = Vec::new();
    if j.total_bytes > 0 && j.state != JobState::Complete {
        facts.push(format!(
            "{} of {}",
            format::size(j.completed_bytes),
            format::size(j.total_bytes)
        ));
    } else if j.total_bytes > 0 {
        facts.push(format::size(j.total_bytes));
    }
    match j.state {
        JobState::Active if j.connections > 0 => {
            let what = if j.seeders.is_some() {
                "peer"
            } else {
                "connection"
            };
            facts.push(format::plural(j.connections as usize, what));
        }
        JobState::Seeding => facts.push(format!("ratio {}", format::ratio(j))),
        _ => {}
    }
    if j.engine == dman_proto::Engine::YtDlp {
        facts.push("video".into());
    } else if matches!(
        j.kind,
        dman_proto::SourceKind::Magnet | dman_proto::SourceKind::Torrent
    ) {
        facts.push("torrent".into());
    } else if let Some(host) = url::Url::parse(&j.source)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
    {
        facts.push(host);
    }
    if j.state != JobState::Complete {
        facts.push(format!(
            "added {}",
            format::ago(state.now_secs() - j.added_at)
        ));
    }
    let used = spans_width(&spans);
    let text = format::truncate(&facts.join(" · "), width.saturating_sub(used));
    spans.push(Span::styled(text, t.faint));
    spans
}

fn section_line(
    t: &Theme,
    section: Section,
    total: usize,
    page: usize,
    pages: usize,
    width: usize,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled("── ", t.section),
        Span::styled(section.title().to_owned(), t.header),
        Span::styled(format!(" {total}"), t.faint),
    ];
    if pages > 1 {
        spans.push(Span::styled(
            format!(" · page {}/{pages} · [ ] to page", page + 1),
            t.faint,
        ));
    }
    let used = spans_width(&spans) + 1;
    spans.push(Span::styled(
        format!(" {}", "─".repeat(width.saturating_sub(used))),
        t.section,
    ));
    Line::from(spans)
}

/// Flash colour of a row whose state just changed.
fn flash_bg(state: &State, id: JobId) -> Option<Style> {
    let (strength, to) = state.anim.flash(id)?;
    let t = &state.theme;
    let tok = match to {
        JobState::Error => t.palette.danger,
        JobState::Complete | JobState::Seeding => t.palette.success,
        JobState::Paused => t.palette.warning,
        _ => t.palette.accent,
    };
    let base = t.palette.surface.0;
    let c = t.rgb(base.mix(tok.0, 0.55 * strength))?;
    Some(Style::new().bg(c))
}

/// Draw the downloads view: the list and, on wide terminals, the preview.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let (list, preview) = split_preview(area, state.ui.preview);
    render_list(frame, list, state);
    if let Some(p) = preview {
        render_preview(frame, p, state);
    }
}

fn list_title(state: &State) -> Line<'static> {
    let t = &state.theme;
    let l = &state.list;
    let mut spans = vec![Span::styled(" Downloads ", t.accent)];
    if l.chip != crate::list::Chip::All {
        spans.push(Span::styled(format!("· {} ", l.chip.label()), t.dim));
    }
    if !l.filter.is_empty() {
        spans.push(Span::styled(format!("· /{} ", l.filter), t.marked));
    }
    Line::from(spans)
}

fn render_list(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let block = t.block_focus(true).title(list_title(state));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let l = &state.list;
    let tall = state.tall_rows();
    let width = usize::from(inner.width);
    let layout = columns(width.saturating_sub(GUTTER));
    let offline = !state.connected();
    let mut y = inner.y;
    let bottom = inner.bottom();
    for (i, row) in l.rows.iter().enumerate().skip(l.offset) {
        let h = u16::try_from(l.row_lines(i, tall)).unwrap_or(1);
        if y + h > bottom {
            break;
        }
        let rect = Rect::new(inner.x, y, inner.width, h);
        match *row {
            Row::Section {
                section,
                total,
                page,
                pages,
            } => {
                frame.render_widget(
                    Paragraph::new(section_line(t, section, total, page, pages, width)),
                    rect,
                );
            }
            Row::Job(id) => {
                if let Some(j) = state.jobs.get(&id) {
                    render_job(frame, rect, state, j, &layout, tall);
                }
            }
            Row::HiddenRemoved(n) => {
                let toggle = state
                    .keymap
                    .label(Context::Downloads, Action::ToggleRemoved);
                let purge = state.keymap.label(Context::Downloads, Action::Purge);
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("    Removed ({n}) hidden — {toggle} shows, {purge} clears"),
                        t.faint,
                    )),
                    rect,
                );
            }
        }
        state.hit(rect, Hit::ListRow(i));
        y += h;
    }
    if l.order.is_empty() {
        render_empty(frame, inner, state);
    }
    if offline {
        frame.buffer_mut().set_style(inner, t.dim);
    }
    let shown = l.visible_rows(usize::from(inner.height), tall);
    if let Some(r) = scrollbar(
        frame,
        area.inner(Margin::new(0, 1)),
        t,
        l.rows.len(),
        l.offset,
        shown,
    ) {
        state.hit(r, Hit::ListScroll);
    }
}

fn render_job(
    frame: &mut Frame<'_>,
    rect: Rect,
    state: &State,
    j: &Job,
    layout: &Columns,
    tall: bool,
) {
    let t = &state.theme;
    let cursor = state.list.cursor == Some(j.id);
    let marked = state.list.selected.contains(&j.id);
    let width = usize::from(rect.width);
    let mut gutter = vec![
        Span::styled(if cursor { t.icons.cursor } else { " " }, t.cursor_bar),
        Span::styled(if marked { t.icons.marked } else { " " }, t.marked),
        Span::styled(t.icons.state(j.state), t.state(j.state)),
        Span::raw(" "),
    ];
    let mut first = std::mem::take(&mut gutter);
    first.extend(job_line(state, j, width.saturating_sub(GUTTER), layout));
    let mut lines = vec![Line::from(first)];
    if tall {
        let mut second = vec![Span::raw(" ".repeat(GUTTER))];
        second.extend(detail_line(state, j, width.saturating_sub(GUTTER)));
        lines.push(Line::from(second));
    }
    let mut style = Style::new();
    if let Some(bg) = flash_bg(state, j.id) {
        style = style.patch(bg);
    }
    if cursor {
        style = style.patch(t.cursor);
    }
    frame.buffer_mut().set_style(rect, style);
    frame.render_widget(Paragraph::new(lines), rect);
    if cursor && t.depth == crate::theme::ColorDepth::Mono {
        frame.buffer_mut().set_style(rect, t.cursor);
    }
}

fn pill_spans(state: &State, a: Action, what: &str) -> Option<Vec<Span<'static>>> {
    let t = &state.theme;
    let key = state
        .keymap
        .keys_with_global(Context::Downloads, a)
        .first()
        .map(|k| crate::keymap::key_label(*k))?;
    Some(vec![
        Span::styled(format!(" {key} "), t.pill),
        Span::styled(format!(" {what}"), t.dim),
    ])
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let key = |a: Action| {
        state
            .keymap
            .keys_with_global(Context::Downloads, a)
            .first()
            .map(|k| crate::keymap::key_label(*k))
            .unwrap_or_default()
    };
    let mut actions: Vec<(Action, Vec<Span<'static>>)> = Vec::new();
    let lines: Vec<Line<'static>> = if !state.list.filter.is_empty() {
        vec![Line::from(vec![
            Span::styled(
                format!("No downloads match '{}' — ", state.list.filter),
                t.dim,
            ),
            Span::styled(key(Action::SelectNone), t.key),
            Span::styled(" clears", t.dim),
        ])]
    } else if state.list.chip != crate::list::Chip::All && !state.jobs.is_empty() {
        vec![Line::from(vec![
            Span::styled(
                format!("Nothing under '{}' — ", state.list.chip.label()),
                t.dim,
            ),
            Span::styled(key(Action::ShowAll), t.key),
            Span::styled(" shows all", t.dim),
        ])]
    } else if state.connected() {
        let mut keys = Vec::new();
        for (a, what) in [
            (Action::Add, "add a link or torrent"),
            (Action::Search, "search torrents"),
            (Action::Help, "all keys"),
        ] {
            if !keys.is_empty() {
                keys.push(Span::raw("   "));
            }
            keys.push(Span::styled(key(a), t.key));
            keys.push(Span::styled(format!("  {what}"), t.dim));
            if let Some(p) = pill_spans(state, a, what) {
                actions.push((a, p));
            }
        }
        vec![
            Line::styled(t.icons.down.to_owned(), t.faint),
            Line::styled("No downloads yet", t.accent),
            Line::default(),
            Line::from(keys),
        ]
    } else {
        let spin = state
            .anim_ms()
            .map_or(t.icons.spinner[0], |ms| t.icons.spin(ms));
        vec![Line::from(vec![
            Span::styled(format!("{spin} "), t.accent),
            Span::styled("No data — waiting for the daemon", t.dim),
        ])]
    };
    if area.height < 3 {
        return;
    }
    let h = u16::try_from(lines.len()).unwrap_or(1).min(area.height);
    let msg = Rect {
        y: area.y + area.height.saturating_sub(h) / 2,
        height: h,
        ..area
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), msg);
    // The key hints are clickable.
    if actions.len() == 3 {
        let line_y = msg.y + msg.height - 1;
        let total: usize = 6 + [
            "a  add a link or torrent",
            "f  search torrents",
            "?  all keys",
        ]
        .iter()
        .map(|s| s.chars().count())
        .sum::<usize>();
        let start =
            msg.x + u16::try_from(usize::from(msg.width).saturating_sub(total) / 2).unwrap_or(0);
        let mut x = start;
        for (a, what) in [
            (Action::Add, "add a link or torrent"),
            (Action::Search, "search torrents"),
            (Action::Help, "all keys"),
        ] {
            let w = u16::try_from(key(a).chars().count() + 2 + what.chars().count()).unwrap_or(0);
            state.hit(Rect::new(x, line_y, w, 1), Hit::Action(a));
            x += w + 3;
        }
    }
}

/// The preview pane: what is under the cursor (or the marked downloads).
fn render_preview(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let marked = state.list.selected.len();
    let job = state.list.cursor.and_then(|id| state.jobs.get(&id));
    let title = match (marked, job) {
        (n, _) if n > 1 => format!(" {n} marked "),
        (_, Some(j)) => format!(
            " {} ",
            format::middle(
                j.name.trim_start_matches("[METADATA]"),
                usize::from(area.width).saturating_sub(6)
            )
        ),
        _ => " Preview ".into(),
    };
    let block = t.block().title(Span::styled(title, t.accent));
    let inner = block.inner(area).inner(Margin::new(1, 0));
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let lines = if marked > 1 {
        preview_marked(state, width)
    } else if let Some(j) = job {
        preview_job(state, j, width)
    } else {
        vec![Line::styled("Nothing under the cursor.", t.dim)]
    };
    let [body, hints] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    let mut h: Vec<Span<'static>> = Vec::new();
    let actions: &[(Action, &str)] = match job.map(|j| j.state) {
        Some(JobState::Paused | JobState::Error) => &[
            (Action::OpenDetail, "details"),
            (Action::Resume, "resume"),
            (Action::Remove, "remove"),
        ],
        Some(_) => &[
            (Action::OpenDetail, "details"),
            (Action::Pause, "pause"),
            (Action::Rename, "rename"),
            (Action::Remove, "remove"),
        ],
        None => &[(Action::Add, "add")],
    };
    let mut x = hints.x;
    for (a, what) in actions {
        if let Some(p) = pill_spans(state, *a, what) {
            let w = spans_width(&p);
            if usize::from(x - hints.x) + w + 1 > width {
                break;
            }
            state.hit(
                Rect::new(x, hints.y, u16::try_from(w).unwrap_or(0), 1),
                Hit::Action(*a),
            );
            x += u16::try_from(w + 2).unwrap_or(0);
            h.extend(p);
            h.push(Span::raw("  "));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(h)), hints);
    if !state.connected() {
        frame.buffer_mut().set_style(inner, t.dim);
    }
}

fn kv(t: &Theme, key: &str, value: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<8}"), t.faint),
        Span::styled(value, style),
    ])
}

fn preview_job(state: &State, j: &Job, width: usize) -> Vec<Line<'static>> {
    let t = &state.theme;
    let summary = crate::views::detail::summary(state, j, None);
    let mut out = vec![
        Line::from(vec![
            Span::styled(format!("{} ", t.icons.state(j.state)), t.state(j.state)),
            Span::styled(
                summary,
                t.state(j.state)
                    .patch(Style::new().add_modifier(ratatui::style::Modifier::BOLD)),
            ),
        ]),
        Line::default(),
    ];
    let bar_w = width.saturating_sub(6).max(4);
    let mut bar_line = if j.total_bytes == 0 && j.state == JobState::Active {
        bar::shuttle(t, bar_w, state.anim_ms(), t.state(j.state))
    } else {
        bar::bar(t, bar_w, state.shown_progress(j), t.state(j.state))
    };
    if j.total_bytes > 0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (j.progress().clamp(0.0, 1.0) * 100.0).floor() as u32;
        bar_line.push(Span::styled(format!(" {pct:>3}%"), t.text));
    }
    out.push(Line::from(bar_line));
    let mut sizes = vec![Span::styled(format::size(j.completed_bytes), t.text)];
    if j.total_bytes > 0 {
        sizes.push(Span::styled(
            format!(" of {}", format::size(j.total_bytes)),
            t.dim,
        ));
    } else {
        sizes.push(Span::styled(" of unknown size", t.dim));
    }
    if j.state == JobState::Active && j.eta_secs().is_some() {
        sizes.push(Span::styled(
            format!("   ETA {}", format::eta(j.eta_secs())),
            t.text,
        ));
    }
    out.push(Line::from(sizes));
    out.push(Line::default());
    let history = state.job_speeds.get(&j.id).map_or(&[][..], Vec::as_slice);
    let spark_w = width.saturating_sub(16).min(40);
    let (dn, du) = format::split_size(j.download_speed, true, false);
    out.push(Line::from(vec![
        Span::styled(format!("{} ", t.icons.down), t.graph_down),
        Span::styled(format!("{dn:>6}"), t.text),
        Span::styled(format!("{du:<7}"), t.faint),
        Span::styled(
            if j.state == JobState::Seeding {
                " ".repeat(spark_w)
            } else {
                bar::sparkline(history, spark_w, 0)
            },
            t.graph_down,
        ),
    ]));
    let (un, uu) = format::split_size(j.upload_speed, true, false);
    out.push(Line::from(vec![
        Span::styled(format!("{} ", t.icons.up), t.graph_up),
        Span::styled(format!("{un:>6}"), t.text),
        Span::styled(format!("{uu:<7}"), t.faint),
        Span::styled(
            if j.state == JobState::Seeding {
                bar::sparkline(history, spark_w, 0)
            } else {
                String::new()
            },
            t.graph_up,
        ),
    ]));
    out.push(Line::default());
    if j.seeders.is_some() || j.connections > 0 {
        out.push(kv(t, "Peers", format::peers(j), t.text));
    }
    if j.uploaded_bytes > 0 {
        out.push(kv(t, "Ratio", format::ratio(j), t.text));
    }
    let engine = match j.engine {
        dman_proto::Engine::Aria2 => "aria2",
        dman_proto::Engine::YtDlp => "yt-dlp (video)",
    };
    out.push(kv(t, "Engine", engine.into(), t.dim));
    out.push(kv(
        t,
        "Folder",
        format::middle(&j.dir, width.saturating_sub(8)),
        t.dim,
    ));
    out.push(kv(
        t,
        "Source",
        format::middle(&j.source, width.saturating_sub(8)),
        t.dim,
    ));
    out.push(kv(
        t,
        "Added",
        format::ago(state.now_secs() - j.added_at),
        t.dim,
    ));
    if let Some(h) = &j.info_hash {
        out.push(kv(
            t,
            "Hash",
            format::middle(h, width.saturating_sub(8)),
            t.faint,
        ));
    }
    if let Some(e) = &j.error {
        out.push(Line::default());
        out.push(Line::styled(format!("{} {e}", t.icons.error), t.error));
    }
    out
}

fn preview_marked(state: &State, width: usize) -> Vec<Line<'static>> {
    let t = &state.theme;
    let jobs: Vec<&Job> = state
        .list
        .selected
        .iter()
        .filter_map(|id| state.jobs.get(id))
        .collect();
    let total: u64 = jobs.iter().map(|j| j.total_bytes).sum();
    let done: u64 = jobs.iter().map(|j| j.completed_bytes).sum();
    let speed: u64 = jobs.iter().map(|j| j.download_speed).sum();
    let mut out = vec![
        kv(
            t,
            "Marked",
            format::plural(jobs.len(), "download"),
            t.marked,
        ),
        kv(
            t,
            "Size",
            format!("{} of {}", format::size(done), format::size(total)),
            t.text,
        ),
        kv(t, "Speed", format::speed(speed), t.info),
        Line::default(),
    ];
    for j in jobs.iter().take(12) {
        out.push(Line::from(vec![
            Span::styled(format!("{} ", t.icons.state(j.state)), t.state(j.state)),
            Span::styled(format::middle(&j.name, width.saturating_sub(2)), t.text),
        ]));
    }
    if jobs.len() > 12 {
        out.push(Line::styled(
            format!("… and {} more", jobs.len() - 12),
            t.faint,
        ));
    }
    out
}

/// Counts per chip (for the chips row).
pub fn chip_counts(state: &State) -> [usize; 5] {
    let mut c = [0; 5];
    for j in state.jobs.values() {
        if j.state == JobState::Removed && !state.list.show_removed {
            continue;
        }
        for (i, chip) in crate::list::Chip::ALL.iter().enumerate() {
            if chip.admits(j.state) {
                c[i] += 1;
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_terminals_drop_low_priority_columns() {
        let has = |c: &Columns, col: Col| c.cols.iter().any(|(x, _)| *x == col);
        let c80 = columns(74);
        assert!(c80.compact);
        assert!(has(&c80, Col::Progress) && has(&c80, Col::Speed) && has(&c80, Col::Eta));
        assert!(!has(&c80, Col::Ratio) && !has(&c80, Col::Up));
        let c150 = columns(150);
        assert_eq!(c150.cols.len(), 7);
        let c40 = columns(40);
        assert!(c40.cols.len() <= 1);
        assert!(!has(&columns(84), Col::Peers) && has(&columns(100), Col::Peers));
    }
}
