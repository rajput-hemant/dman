//! Detail view (placeholder until M3: files, peers, trackers, piece map,
//! speed history and option editing).

use dman_proto::{Job, JobId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::format;
use crate::theme::Theme;

/// Detail view state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailState {
    /// The job shown.
    pub id: JobId,
}

/// Detail-view actions (only the global ones and `Back` for now).
pub fn apply(_state: &mut State, _action: Action) -> Option<Vec<Cmd>> {
    None
}

fn field(t: &Theme, name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:>10}  "), t.dim),
        Span::raw(value),
    ])
}

fn job_lines(t: &Theme, j: &Job) -> Vec<Line<'static>> {
    let mut v = vec![
        field(t, "Name", j.name.clone()),
        field(t, "Status", format::state(j.state).to_owned()),
        field(
            t,
            "Progress",
            format!(
                "{}  {} / {}",
                format::progress_bar(j, 20),
                format::size(j.completed_bytes),
                format::size_or_dash(j.total_bytes)
            ),
        ),
        field(
            t,
            "Speed",
            format!(
                "▼ {}  ▲ {}  ETA {}",
                format::speed(j.download_speed),
                format::speed(j.upload_speed),
                format::eta(j.eta_secs())
            ),
        ),
        field(
            t,
            "Peers",
            format!("{}  ratio {}", format::peers(j), format::ratio(j)),
        ),
        field(t, "Directory", j.dir.clone()),
        field(t, "Source", j.source.clone()),
    ];
    if let Some(h) = &j.info_hash {
        v.push(field(t, "Info hash", h.clone()));
    }
    if let Some(e) = &j.error {
        v.push(Line::from(vec![
            Span::styled(format!("{:>10}  ", "Error"), t.dim),
            Span::styled(e.clone(), t.error),
        ]));
    }
    v
}

/// Draw the detail view.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, d: &DetailState) {
    let t = &state.theme;
    let mut lines = match state.jobs.get(&d.id) {
        Some(j) => job_lines(t, j),
        None => vec![Line::styled(
            format!("Download #{} is no longer in the list.", d.id),
            t.warn,
        )],
    };
    lines.push(Line::default());
    lines.push(Line::styled(
        "Files, peers, trackers, piece map and speed history arrive in a later version.",
        t.dim,
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border)
        .title(Span::styled(format!(" Download #{} ", d.id), t.accent));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}
