//! Header (speeds, counts, disk, connection), throughput card, context row
//! (filter chips or breadcrumb) and banners (disconnected, engine unavailable).
//! Transient messages are toasts ([`crate::widgets::toast`]).

use dman_client::ConnState;
use dman_proto::JobState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::State;
use crate::format::truncate;
use crate::layout::Hit;
use crate::list::Chip;
use crate::views::View;
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

/// Global (down, up) speeds for the header. The daemon's figures can lag
/// behind the job states (the last sample of a finished download), so with no
/// transferring job the speed shown is 0.
pub fn shown_speeds(state: &State) -> (u64, u64) {
    use dman_proto::JobState;
    let any = |f: fn(JobState) -> bool| state.jobs.values().any(|j| f(j.state));
    let down = if any(|s| s == JobState::Active) {
        state.stats.download_speed
    } else {
        0
    };
    let up = if any(|s| matches!(s, JobState::Active | JobState::Seeding)) {
        state.stats.upload_speed
    } else {
        0
    };
    (down, up)
}

fn conn_label(state: &State) -> (String, Style) {
    let t = &state.theme;
    let i = &t.icons;
    match &state.conn {
        ConnState::Connected { .. } => (format!("{} online", i.online), t.ok),
        ConnState::Connecting => {
            let spin = state.anim_ms().map_or(i.offline, |ms| i.spin(ms));
            (format!("{spin} connecting"), t.warn)
        }
        ConnState::Disconnected { .. } => (format!("{} offline", i.offline), t.error),
        ConnState::Failed { .. } => (format!("{} failed", i.error), t.error),
    }
}

/// (active, waiting, done, failed) counts of the listed jobs.
fn counts(state: &State) -> [usize; 4] {
    let mut c = [0; 4];
    for j in state.jobs.values() {
        match j.state {
            JobState::Active | JobState::Seeding => c[0] += 1,
            JobState::Waiting | JobState::Paused => c[1] += 1,
            JobState::Complete => c[2] += 1,
            JobState::Error => c[3] += 1,
            JobState::Removed => {}
        }
    }
    c
}

/// A segment of the header: priority (lower = kept longer) and spans.
type Segment = (u8, Vec<Span<'static>>);

fn speed_spans(
    state: &State,
    icon: &'static str,
    bps: u64,
    style: Style,
    compact: bool,
) -> Vec<Span<'static>> {
    let t = &state.theme;
    let (n, u) = if bps == 0 {
        ("0".to_owned(), String::new())
    } else {
        crate::format::split_size(bps, true, compact)
    };
    vec![
        Span::styled(format!("{icon} "), style),
        Span::styled(n, if bps == 0 { t.dim } else { t.text }),
        Span::styled(u, t.faint),
    ]
}

/// Draw the one-line header: brand, (compact) throughput, counters, disk,
/// limits, mouse and connection.
pub fn render_header(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let width = usize::from(area.width);
    let card = crate::layout::card_shown(state.size, state);
    let (down, up) = shown_speeds(state);
    let mut left: Vec<Segment> = vec![(
        0,
        vec![
            Span::styled(" dman", t.accent),
            Span::styled(" ▸ ", t.faint),
            Span::styled(state.profile.clone(), t.dim),
        ],
    )];
    if !card {
        let mut s = vec![Span::raw("  ")];
        s.extend(speed_spans(state, t.icons.down, down, t.graph_down, true));
        left.push((0, s));
        let history: Vec<u64> = state.graph.history.iter().map(|x| x.down).collect();
        if history.iter().any(|v| *v > 0) {
            left.push((
                4,
                vec![
                    Span::raw(" "),
                    Span::styled(
                        crate::widgets::bar::sparkline(&history, 12, 0),
                        t.graph_down,
                    ),
                ],
            ));
        }
        let mut s = vec![Span::raw(" ")];
        s.extend(speed_spans(state, t.icons.up, up, t.graph_up, true));
        left.push((1, s));
    }
    let [a, w, d, f] = counts(state);
    let any = a + w + d + f > 0;
    let mut c = vec![Span::raw("  ")];
    let short = width < 100;
    let mut counter = |n: usize, icon: &'static str, style: Style, label: &str| {
        if n == 0 && (label != "active" || !any) {
            return;
        }
        if short {
            c.push(Span::styled(icon, style));
            c.push(Span::styled(format!("{n} "), t.text));
        } else {
            c.push(Span::styled(format!("{icon} "), style));
            c.push(Span::styled(n.to_string(), t.text));
            c.push(Span::styled(format!(" {label}  "), t.faint));
        }
    };
    counter(a, t.icons.active, t.info, "active");
    counter(w, t.icons.waiting, t.dim, "waiting");
    counter(d, t.icons.done, t.ok, "done");
    counter(f, t.icons.error, t.error, "failed");
    left.push((2, c));
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
        left.push((3, vec![Span::styled(text, style), Span::raw("  ")]));
    }
    let (conn, conn_style) = conn_label(state);
    let mut right: Vec<Segment> = Vec::new();
    if let Some(free) = state.stats.disk_free {
        right.push((
            5,
            vec![
                Span::styled(crate::format::size(free), t.dim),
                Span::styled(" free  ", t.faint),
            ],
        ));
    }
    let mouse = if state.mouse { "mouse on" } else { "mouse off" };
    right.push((
        6,
        vec![
            Span::styled(mouse, if state.mouse { t.dim } else { t.faint }),
            Span::raw("  "),
        ],
    ));
    right.push((0, vec![Span::styled(conn, conn_style), Span::raw(" ")]));
    // Drop segments by priority (highest number first) until it all fits.
    let mut segs: Vec<(bool, Segment)> = left.into_iter().map(|x| (false, x)).collect();
    segs.extend(right.into_iter().map(|x| (true, x)));
    let total = |segs: &[(bool, Segment)]| -> usize {
        segs.iter()
            .flat_map(|(_, (_, s))| s.iter())
            .map(Span::width)
            .sum()
    };
    let mut prios: Vec<u8> = segs.iter().map(|(_, s)| s.0).collect();
    prios.sort_unstable();
    prios.dedup();
    while total(&segs) + 1 > width {
        let Some(p) = prios.pop() else { break };
        if p == 0 {
            break;
        }
        segs.retain(|(_, s)| s.0 != p);
    }
    let side = |right: bool| -> Vec<Span<'static>> {
        segs.iter()
            .filter(|(r, _)| *r == right)
            .flat_map(|(_, (_, s))| s.iter().cloned())
            .collect()
    };
    let mut spans = side(false);
    let spans_right = side(true);
    let used: usize = spans.iter().map(Span::width).sum();
    let fitted_right_w: usize = spans_right.iter().map(Span::width).sum();
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + fitted_right_w)),
    ));
    // The mouse indicator is clickable.
    let before: usize = spans_right
        .iter()
        .take_while(|s| s.content != mouse)
        .map(Span::width)
        .sum();
    if spans_right.iter().any(|s| s.content == mouse) {
        let x = area.x + u16::try_from(width.saturating_sub(fitted_right_w) + before).unwrap_or(0);
        state.hit(
            Rect::new(x, area.y, u16::try_from(mouse.len()).unwrap_or(0), 1),
            Hit::Mouse,
        );
    }
    spans.extend(spans_right);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The throughput card: big numbers, peaks and braille graphs of the last
/// minutes, plus the limits.
pub fn render_card(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let block = t.block().title(Span::styled(" Throughput ", t.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (down, up) = shown_speeds(state);
    let hist = &state.graph.history;
    let downs: Vec<u64> = hist.iter().map(|s| s.down).collect();
    let ups: Vec<u64> = hist.iter().map(|s| s.up).collect();
    let width = usize::from(inner.width);
    let right_w = 26usize;
    let left_w = 30usize;
    let graph_w = width.saturating_sub(left_w + right_w + 2);
    let shown = graph_w * 2;
    let peak = |v: &[u64]| {
        v[v.len().saturating_sub(shown)..]
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
    };
    let max = peak(&downs).max(peak(&ups));
    let row = |icon: &'static str, now: u64, series: &[u64], style: Style, limit: u64| {
        let (n, u) = crate::format::split_size(now, true, false);
        let (pn, pu) = match peak(series) {
            0 => ("–".to_owned(), String::new()),
            p => crate::format::split_size(p, true, true),
        };
        let mut spans = vec![
            Span::styled(format!(" {icon} "), style),
            Span::styled(
                format!("{n:>6}"),
                if now == 0 {
                    t.dim
                } else {
                    t.text.add_modifier(Modifier::BOLD)
                },
            ),
            Span::styled(format!("{u:<7}"), t.faint),
            Span::styled(format!("  peak {pn:>6}{pu:<2}"), t.faint),
        ];
        let used: usize = spans.iter().map(Span::width).sum();
        spans.push(Span::raw(" ".repeat(left_w.saturating_sub(used) + 1)));
        if graph_w > 0 {
            spans.push(Span::styled(
                crate::widgets::bar::braille(series, graph_w, max),
                style,
            ));
        }
        spans.push(Span::raw(" "));
        let lim = if limit == 0 {
            vec![Span::styled("no limit", t.faint)]
        } else {
            vec![
                Span::styled("limit ", t.faint),
                Span::styled(crate::format::speed(limit), t.warn),
            ]
        };
        let lim_w: usize = lim.iter().map(Span::width).sum();
        let used: usize = spans.iter().map(Span::width).sum();
        spans.push(Span::raw(
            " ".repeat(width.saturating_sub(used + lim_w + 1)),
        ));
        spans.extend(lim);
        Line::from(spans)
    };
    let lines = vec![
        row(
            t.icons.down,
            down,
            &downs,
            t.graph_down,
            state.limits.download,
        ),
        row(t.icons.up, up, &ups, t.graph_up, state.limits.upload),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The context row: filter chips, filter, sort and selection in the list;
/// a breadcrumb in other views.
pub fn render_context(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    if !matches!(state.view(), View::Downloads) {
        let mut spans = vec![Span::raw(" ")];
        let crumbs = breadcrumb(state);
        let n = crumbs.len();
        for (i, c) in crumbs.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" › ", t.faint));
            }
            spans.push(Span::styled(c, if i + 1 == n { t.accent } else { t.dim }));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let l = &state.list;
    let counts = crate::views::downloads::chip_counts(state);
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    let compact = area.width < 100;
    for (i, chip) in Chip::ALL.iter().enumerate() {
        let label = if compact && *chip != Chip::All && counts[i] == 0 {
            continue;
        } else {
            format!(" {} {} ", chip.label(), counts[i])
        };
        let style = if l.chip == *chip { t.mode } else { t.dim };
        let w = u16::try_from(label.chars().count()).unwrap_or(0);
        state.hit(Rect::new(x, area.y, w + 1, 1), Hit::Chip(*chip));
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
        x += w + 1;
    }
    let mut right: Vec<Span<'static>> = Vec::new();
    let editing = match &state.modal {
        Some(Modal::Filter(f)) => Some(f),
        _ => None,
    };
    if let Some(f) = editing {
        right.push(Span::styled(format!("{} /", t.icons.search), t.accent));
        right.extend(f.input.line(t, true).spans);
        right.push(Span::raw("  "));
    } else if !l.filter.is_empty() {
        right.push(Span::styled("filter ", t.faint));
        right.push(Span::styled(l.filter.clone(), t.marked));
        right.push(Span::raw("  "));
    }
    if !l.selected.is_empty() {
        right.push(Span::styled(
            format!("{} {} marked  ", t.icons.marked, l.selected.len()),
            t.marked,
        ));
    }
    let dir = if l.reverse { "▲" } else { "▼" };
    right.push(Span::styled("sort ", t.faint));
    right.push(Span::styled(format!("{} {dir} ", l.sort.label()), t.dim));
    let used: usize = spans.iter().map(Span::width).sum();
    let rw: usize = right.iter().map(Span::width).sum();
    let width = usize::from(area.width);
    if used + rw < width {
        spans.push(Span::raw(" ".repeat(width - used - rw)));
        spans.extend(right);
    } else if editing.is_some() {
        // While typing the filter it wins over the chips.
        spans = vec![Span::raw(" ")];
        spans.extend(right);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn breadcrumb(state: &State) -> Vec<String> {
    let mut out = Vec::new();
    for v in &state.views {
        out.push(match v {
            View::Downloads => "Downloads".to_owned(),
            View::Detail(d) => match state.jobs.get(&d.id) {
                Some(j) => format!(
                    "{} {}",
                    state.theme.icons.state(j.state),
                    crate::format::middle(j.name.trim_start_matches("[METADATA]"), 48)
                ),
                None => format!("Download #{}", d.id),
            },
            View::Help(_) => "Help".to_owned(),
            View::Add(_) => "Add downloads".to_owned(),
            View::Search => "Search".to_owned(),
            View::Providers => "Search providers".to_owned(),
        });
    }
    out
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
