//! Speed charts: the per-download chart of the detail view and the optional
//! global graph panel of the downloads view.

use std::time::Duration;

use dman_proto::SpeedSample;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph};

use crate::api::ApiCall;
use crate::api::ApiReply;
use crate::app::{Cmd, State};
use crate::format;
use crate::layout::Hit;
use crate::theme::Theme;

/// How often the global history is re-read while the panel is visible.
pub const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Samples kept in memory (the daemon samples about once a second).
const KEEP: usize = 600;

/// Nominal spacing of samples appended from live `Stats` events (ms).
const LIVE_STEP_MS: i64 = 1000;

/// How much history a speed chart shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Window {
    /// The last minute.
    #[default]
    Min1,
    /// The last five minutes.
    Min5,
    /// The last fifteen minutes.
    Min15,
}

impl Window {
    /// All windows, shortest first.
    pub const ALL: [Window; 3] = [Window::Min1, Window::Min5, Window::Min15];

    /// Length in seconds.
    pub const fn secs(self) -> i64 {
        match self {
            Window::Min1 => 60,
            Window::Min5 => 300,
            Window::Min15 => 900,
        }
    }

    /// Short label.
    pub const fn label(self) -> &'static str {
        match self {
            Window::Min1 => "1 min",
            Window::Min5 => "5 min",
            Window::Min15 => "15 min",
        }
    }

    /// Position in [`Window::ALL`].
    pub fn index(self) -> usize {
        Window::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    /// The next (or previous) window, stopping at the ends.
    pub fn step(self, forward: bool) -> Window {
        let i = self.index();
        let i = if forward {
            (i + 1).min(Window::ALL.len() - 1)
        } else {
            i.saturating_sub(1)
        };
        Window::ALL[i]
    }
}

/// The samples inside `window` (counted back from the newest) and how many
/// seconds of history there are in total.
pub fn windowed(history: &[SpeedSample], window: Window) -> (&[SpeedSample], i64) {
    let (Some(first), Some(last)) = (history.first(), history.last()) else {
        return (history, 0);
    };
    let available = ((last.t - first.t) / 1000).max(0);
    let from = last.t - window.secs() * 1000;
    let start = history.partition_point(|s| s.t < from);
    (&history[start..], available)
}

fn duration_text(secs: i64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", (secs + 30) / 60)
    }
}

/// What a chart really shows, e.g. `last 5 min (2 min available)`.
pub fn describe(window: Window, available: i64) -> String {
    if available + 3 >= window.secs() {
        format!("last {}", window.label())
    } else if available < 2 {
        format!("last {} (no history yet)", window.label())
    } else {
        format!(
            "last {} ({} available)",
            window.label(),
            duration_text(available)
        )
    }
}

/// Average `values` down to at most `points` values (newest kept right).
pub fn resample(values: &[u64], points: usize) -> Vec<u64> {
    if points == 0 || values.len() <= points {
        return values.to_vec();
    }
    let n = values.len();
    (0..points)
        .map(|i| {
            let (a, b) = (
                i * n / points,
                ((i + 1) * n / points).max(i * n / points + 1),
            );
            let chunk = &values[a..b.min(n)];
            chunk.iter().sum::<u64>() / chunk.len().max(1) as u64
        })
        .collect()
}

/// The window labels as clickable spans starting at column `x`, row `y`,
/// followed by the honest note when it fits in `max_w` columns.
pub fn selector(state: &State, x: u16, y: u16, available: i64, max_w: usize) -> Vec<Span<'static>> {
    let t = &state.theme;
    let window = state.graph.window;
    let mut spans = Vec::new();
    let mut used = 0usize;
    let mut at = x;
    for w in Window::ALL {
        let label = format!(" {} ", w.label());
        let width = label.chars().count();
        if used + width + 1 > max_w {
            return spans;
        }
        let style = if w == window { t.mode } else { t.dim };
        let cell = u16::try_from(width).unwrap_or(0);
        state.hit(Rect::new(at, y, cell, 1), Hit::SpeedWindow(w));
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
        used += width + 1;
        at = at.saturating_add(cell + 1);
    }
    let note = describe(window, available);
    if used + 1 + note.chars().count() <= max_w {
        spans.push(Span::styled(note, t.dim));
    }
    spans
}

/// State of the global speed graph; kept in memory only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphState {
    /// The panel is shown (in the downloads view).
    pub visible: bool,
    /// Samples, oldest first.
    pub history: Vec<SpeedSample>,
    /// Time since the history was last requested.
    pub since_refresh: Duration,
    /// A `stats.history` call is in flight.
    pub inflight: bool,
    /// How much history the charts show.
    pub window: Window,
}

impl GraphState {
    /// Append the speeds of a live `Stats` event.
    pub fn push_live(&mut self, down: u64, up: u64) {
        let t = self.history.last().map_or(0, |s| s.t + LIVE_STEP_MS);
        self.history.push(SpeedSample { t, down, up });
        self.trim();
    }

    fn trim(&mut self) {
        let extra = self.history.len().saturating_sub(KEEP);
        self.history.drain(..extra);
    }
}

/// `[` / `]`: a shorter or longer window for every speed chart.
pub fn step_window(state: &mut State, forward: bool) -> Vec<Cmd> {
    let w = state.graph.window.step(forward);
    if w == state.graph.window {
        state.status.info(format!(
            "Already the {} window ({})",
            if forward { "longest" } else { "shortest" },
            w.label()
        ));
    } else {
        state.graph.window = w;
        let (_, available) = windowed(&state.graph.history, w);
        state
            .status
            .info(format!("Speed charts show the {}", describe(w, available)));
    }
    Vec::new()
}

/// Request the history now (nothing while disconnected or already asking).
pub fn fetch(state: &mut State) -> Vec<Cmd> {
    if !state.connected() || state.graph.inflight {
        return Vec::new();
    }
    state.graph.inflight = true;
    state.graph.since_refresh = Duration::ZERO;
    vec![Cmd::Api(ApiCall::StatsHistory)]
}

/// Show or hide the panel; showing it loads the history.
pub fn toggle(state: &mut State) -> Vec<Cmd> {
    state.graph.visible = !state.graph.visible;
    if state.graph.visible {
        state.status.info("Speed graph on (w hides it)");
        fetch(state)
    } else {
        state.status.info("Speed graph off");
        Vec::new()
    }
}

/// Tick: re-read the history every [`REFRESH_EVERY`] while the panel is
/// visible; while another view covers the downloads list the refresh is held
/// back and happens as soon as the list is on top again.
pub fn on_tick(state: &mut State) -> Vec<Cmd> {
    if !state.graph.visible {
        return Vec::new();
    }
    state.graph.since_refresh += state.tick;
    let on_screen = matches!(state.view(), crate::views::View::Downloads);
    if on_screen && state.graph.since_refresh >= REFRESH_EVERY {
        fetch(state)
    } else {
        Vec::new()
    }
}

/// The connection dropped: a call in flight will never answer.
pub fn on_disconnect(state: &mut State) {
    state.graph.inflight = false;
}

/// A `stats.history` call finished.
pub fn on_result(state: &mut State, result: &Result<ApiReply, String>) {
    state.graph.inflight = false;
    if let Ok(ApiReply::History(h)) = result {
        state.graph.history = h.clone();
        state.graph.trim();
    }
}

/// Draw the global panel (a bordered box with the current speeds in its title).
pub fn render_panel(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let (shown, available) = windowed(&state.graph.history, state.graph.window);
    let mut spans = vec![
        Span::styled(" Speed ", t.accent),
        Span::styled(format!("{} ", t.icons.down), t.graph_down),
        Span::raw(format::speed(state.stats.download_speed)),
        Span::raw("  "),
        Span::styled(format!("{} ", t.icons.up), t.graph_up),
        Span::raw(format::speed(state.stats.upload_speed)),
        Span::raw("  "),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    let x = area.x + 1 + u16::try_from(used).unwrap_or(0);
    let room = usize::from(area.width).saturating_sub(used + 3);
    spans.extend(selector(state, x, area.y, available, room));
    spans.push(Span::raw(" "));
    let block = t.block().title(Line::from(spans));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    speed_chart(frame, inner, t, shown, "No speed samples yet.");
}

/// A down/up line chart of `history` (oldest first); `empty` is shown when
/// there are no samples.
pub fn speed_chart(
    frame: &mut Frame<'_>,
    area: Rect,
    t: &Theme,
    history: &[SpeedSample],
    empty: &str,
) {
    let (Some(first), Some(last)) = (history.first(), history.last()) else {
        frame.render_widget(Paragraph::new(Line::styled(empty.to_owned(), t.dim)), area);
        return;
    };
    let at = |s: &SpeedSample| (s.t - last.t) as f64 / 1000.0;
    let down: Vec<(f64, f64)> = history.iter().map(|s| (at(s), s.down as f64)).collect();
    let up: Vec<(f64, f64)> = history.iter().map(|s| (at(s), s.up as f64)).collect();
    let max = history
        .iter()
        .map(|s| s.down.max(s.up))
        .max()
        .unwrap_or(0)
        .max(1);
    let top = max as f64 * 1.1;
    let span = at(first).min(-10.0);
    let secs = if -span >= 90.0 {
        format!("{:.0} min ago", -span / 60.0)
    } else {
        format!("{:.0}s ago", -span)
    };
    // Truncation intended: labels are rounded bytes per second.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let labels = vec![
        Span::raw("0"),
        Span::raw(format::speed((top / 2.0) as u64)),
        Span::raw(format::speed(top as u64)),
    ];
    let datasets = vec![
        Dataset::default()
            .name("down")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(t.graph_down)
            .data(&down),
        Dataset::default()
            .name("up")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(t.graph_up)
            .data(&up),
    ];
    let chart = Chart::new(datasets)
        .x_axis(
            Axis::default()
                .style(t.dim)
                .bounds([span, 0.0])
                .labels(vec![Span::raw(secs), Span::raw("now")]),
        )
        .y_axis(
            Axis::default()
                .style(t.dim)
                .bounds([0.0, top])
                .labels(labels),
        );
    frame.render_widget(chart, area);
}
