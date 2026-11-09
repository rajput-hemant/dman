//! Header (speeds, counts, disk, connection), info bar (sort, filter,
//! selection, limits), banners (disconnected, engine unavailable) and the status
//! line (last result, last error).

use dman_client::ConnState;
use dman_proto::method::Level;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::State;
use crate::format::{limit, plural, size, speed, truncate};
use crate::list::Section;
use crate::widgets::Modal;

/// Severity of a banner line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// Something is broken.
    Error,
    /// Transient (connecting).
    Warn,
}

/// Banner lines to show above the list (at most two).
pub fn banners(state: &State) -> Vec<(BannerKind, String)> {
    let mut out = Vec::new();
    match &state.conn {
        ConnState::Connected { .. } => {}
        ConnState::Connecting => {
            out.push((BannerKind::Warn, "Connecting to the daemon…".to_owned()));
        }
        ConnState::Disconnected { last_error, .. } => {
            let secs = state.retry_left.as_secs_f64().ceil();
            out.push((
                BannerKind::Error,
                format!("Disconnected — retrying in {secs:.0}s: {last_error}"),
            ));
        }
        ConnState::Failed { error } => out.push((
            BannerKind::Error,
            format!("Connection failed, not retrying (q quits): {error}"),
        )),
    }
    let engines: Vec<String> = state
        .unavailable_engines()
        .map(|e| {
            let name = match e.engine {
                dman_proto::Engine::Aria2 => "aria2",
                dman_proto::Engine::YtDlp => "yt-dlp",
            };
            match &e.reason {
                Some(r) => format!("{name} unavailable: {r}"),
                None => format!("{name} unavailable"),
            }
        })
        .collect();
    if !engines.is_empty() {
        out.push((BannerKind::Error, format!("Engine {}", engines.join("; "))));
    }
    out.truncate(2);
    out
}

/// Draw banner lines.
pub fn render_banners(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let lines: Vec<Line<'_>> = banners(state)
        .into_iter()
        .map(|(kind, text)| {
            let style = match kind {
                BannerKind::Error => state.theme.banner_error,
                BannerKind::Warn => state.theme.banner_warn,
            };
            let w = usize::from(area.width);
            let text = truncate(&format!(" {text}"), w);
            Line::styled(format!("{text:<w$}"), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn conn_label(state: &State) -> (String, Style) {
    let t = &state.theme;
    match &state.conn {
        ConnState::Connected { .. } => ("● online".into(), t.ok),
        ConnState::Connecting => ("◌ connecting".into(), t.warn),
        ConnState::Disconnected { .. } => ("○ offline".into(), t.error),
        ConnState::Failed { .. } => ("✖ failed".into(), t.error),
    }
}

fn counts(state: &State) -> [usize; 3] {
    let mut c = [0; 3];
    for j in state.jobs.values() {
        match Section::of(j.state) {
            Section::Active => c[0] += 1,
            Section::Waiting => c[1] += 1,
            Section::Stopped => c[2] += 1,
        }
    }
    c
}

/// Draw the one-line header.
pub fn render_header(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let [a, w, s] = counts(state);
    let mut segments: Vec<(u8, Vec<Span<'_>>)> = vec![
        (0, vec![Span::styled(" dman", t.accent)]),
        (
            3,
            vec![Span::styled(format!(" [{}]", state.profile), t.dim)],
        ),
        (
            0,
            vec![
                Span::raw("  ▼ "),
                Span::styled(speed(state.stats.download_speed), t.ok),
                Span::raw(" ▲ "),
                Span::styled(speed(state.stats.upload_speed), t.ok),
            ],
        ),
        (
            1,
            vec![Span::raw(format!(
                "  {a} active · {w} waiting · {s} stopped"
            ))],
        ),
    ];
    if let Some(text) = state
        .stats
        .schedule
        .as_ref()
        .and_then(|b| brief_text(b, &state.time_zone, state.now_secs()))
    {
        let style = if state.stats.schedule.as_ref().is_some_and(|b| b.paused) {
            t.warn
        } else {
            t.dim
        };
        segments.push((2, vec![Span::styled(format!("  {text}"), style)]));
    }
    let (conn, conn_style) = conn_label(state);
    let right = conn.chars().count() + 1;
    let width = usize::from(area.width);
    // Drop segments by priority (3 first) until everything fits.
    let len = |segs: &[(u8, Vec<Span<'_>>)]| -> usize {
        segs.iter()
            .flat_map(|(_, s)| s.iter())
            .map(|s| s.content.chars().count())
            .sum()
    };
    for prio in [3, 1, 2] {
        if len(&segments) + right < width {
            break;
        }
        segments.retain(|(p, _)| *p != prio);
    }
    let mut spans: Vec<Span<'_>> = segments.into_iter().flat_map(|(_, s)| s).collect();
    let pad = width.saturating_sub(len_spans(&spans) + right);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(conn, conn_style));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `HH:MM`, prefixed with the weekday when `at` is not today (in `tz`).
fn clock_label(at: i64, tz: &jiff::tz::TimeZone, now: i64) -> Option<String> {
    let z = |s| {
        jiff::Timestamp::from_second(s)
            .ok()
            .map(|t| t.to_zoned(tz.clone()))
    };
    let (at, now) = (z(at)?, z(now)?);
    let fmt = if at.date() == now.date() {
        "%H:%M"
    } else {
        "%a %H:%M"
    };
    Some(at.strftime(fmt).to_string())
}

/// Header text for the schedule: `⏸ work until 17:00`, `night until 07:00`,
/// or `next night 22:00` outside every window; `None` when nothing is known.
pub fn brief_text(
    b: &dman_proto::ScheduleBrief,
    tz: &jiff::tz::TimeZone,
    now: i64,
) -> Option<String> {
    let when = b.next_change.and_then(|t| clock_label(t, tz, now));
    match (&b.active, &when) {
        (Some(name), Some(w)) => {
            let icon = if b.paused { "⏸ " } else { "" };
            Some(format!("{icon}{name} until {w}"))
        }
        (Some(name), None) => Some(format!("{}{name}", if b.paused { "⏸ " } else { "" })),
        (None, Some(w)) => b.next_window.as_ref().map(|n| format!("next {n} {w}")),
        (None, None) => None,
    }
}

fn len_spans(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Draw the info bar: sort, filter, selection, limits.
pub fn render_info(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let l = &state.list;
    let dir = if l.reverse { " (rev)" } else { "" };
    let mut spans = vec![
        Span::styled(" sort ", t.dim),
        Span::raw(format!("{}{dir}", l.sort.label())),
    ];
    let editing = match &state.modal {
        Some(Modal::Filter(f)) => Some(f),
        _ => None,
    };
    if let Some(f) = editing {
        spans.push(Span::styled("  filter /", t.accent));
        spans.extend(f.input.line(t, true).spans);
    } else if !l.filter.is_empty() {
        spans.push(Span::styled("  filter ", t.dim));
        spans.push(Span::styled(l.filter.clone(), t.marked));
    }
    if !l.selected.is_empty() {
        spans.push(Span::styled(
            format!("  {} selected", l.selected.len()),
            t.marked,
        ));
    }
    spans.push(Span::styled("  limit ", t.dim));
    spans.push(Span::raw(format!(
        "▼ {} ▲ {}",
        limit(state.limits.download),
        limit(state.limits.upload)
    )));
    spans.push(Span::styled("  max ", t.dim));
    spans.push(Span::raw(state.limits.max_concurrent.to_string()));
    if state.pending > 0 {
        spans.push(Span::styled(format!("  ⧗ {}", state.pending), t.warn));
    }
    if let Some(free) = state.stats.disk_free {
        let disk = format!("{} free ", size(free));
        let used = len_spans(&spans);
        let width = usize::from(area.width);
        if used + disk.chars().count() < width {
            let pad = width - used - disk.chars().count();
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(disk, t.dim));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw the status line: last message, plus the last error when the message
/// is not that error.
pub fn render_status(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let st = &state.status;
    let mut spans = Vec::new();
    match &st.last {
        Some(m) => {
            let style = match m.level {
                Level::Info => t.text,
                Level::Warn => t.warn,
                Level::Error => t.error,
            };
            spans.push(Span::styled(format!(" {}", m.text), style));
            if let Some(e) = &st.last_error
                && m.level == Level::Info
            {
                spans.push(Span::styled("  last error: ", t.dim));
                spans.push(Span::styled(e.clone(), t.error));
            }
        }
        None => spans.push(Span::styled(
            format!(
                " {} · press ? for help",
                plural(state.jobs.len(), "download")
            ),
            t.dim,
        )),
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
