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
use crate::theme::Theme;

/// How often the global history is re-read while the panel is visible.
pub const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Samples kept in memory (the daemon samples about once a second).
const KEEP: usize = 600;

/// Samples drawn (about two minutes).
const SHOWN: usize = 120;

/// Nominal spacing of samples appended from live `Stats` events (ms).
const LIVE_STEP_MS: i64 = 1000;

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
    let title = Line::from(vec![
        Span::styled(" Speed ", t.accent),
        Span::styled(format!("{} ", t.icons.down), t.graph_down),
        Span::raw(format::speed(state.stats.download_speed)),
        Span::raw("  "),
        Span::styled(format!("{} ", t.icons.up), t.graph_up),
        Span::raw(format::speed(state.stats.upload_speed)),
        Span::styled("  last 2 min ", t.dim),
    ]);
    let block = t.block().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let from = state.graph.history.len().saturating_sub(SHOWN);
    speed_chart(
        frame,
        inner,
        t,
        &state.graph.history[from..],
        "No speed samples yet.",
    );
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
    let secs = format!("{:.0}s ago", -span);
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
