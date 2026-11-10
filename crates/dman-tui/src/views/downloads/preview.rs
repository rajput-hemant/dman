//! The preview pane beside the downloads list and the empty-list state.

use dman_proto::{Job, JobState};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::render::spans_width;
use crate::action::Action;
use crate::app::State;
use crate::format;
use crate::keymap::Context;
use crate::layout::Hit;
use crate::theme::Theme;
use crate::widgets::bar;

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

/// What to do on an empty list, as clickable key hints.
const EMPTY_HINTS: [(Action, &str); 3] = [
    (Action::Add, "add a link or torrent"),
    (Action::Search, "search torrents"),
    (Action::Help, "all keys"),
];

pub(super) fn render_empty(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let key = |a: Action| {
        state
            .keymap
            .keys_with_global(Context::Downloads, a)
            .first()
            .map(|k| crate::keymap::key_label(*k))
            .unwrap_or_default()
    };
    let width = usize::from(area.width);
    let fit = |s: String| format::truncate(&s, width);
    // The hints go on one line when they fit, else one per line.
    let hints: Vec<(Action, String)> = EMPTY_HINTS
        .iter()
        .map(|(a, what)| (*a, format!("{}  {what}", key(*a))))
        .collect();
    let one_line_w = hints.iter().map(|(_, h)| format::width(h)).sum::<usize>() + 6;
    let one_line = one_line_w <= width;
    let mut hint_rows = 0;
    let lines: Vec<Line<'static>> = if !state.list.filter.is_empty() {
        vec![Line::from(vec![
            Span::styled(
                fit(format!("No downloads match '{}' — ", state.list.filter)),
                t.dim,
            ),
            Span::styled(key(Action::SelectNone), t.key),
            Span::styled(" clears", t.dim),
        ])]
    } else if state.list.chip != crate::list::Chip::All && !state.jobs.is_empty() {
        vec![Line::from(vec![
            Span::styled(
                fit(format!("Nothing under '{}' — ", state.list.chip.label())),
                t.dim,
            ),
            Span::styled(key(Action::ShowAll), t.key),
            Span::styled(" shows all", t.dim),
        ])]
    } else if state.connected() {
        let mut out = vec![
            Line::styled(t.icons.down.to_owned(), t.faint),
            Line::styled("No downloads yet", t.accent),
            Line::default(),
        ];
        let styled = |a: Action, what: &str| {
            vec![
                Span::styled(key(a), t.key),
                Span::styled(format!("  {what}"), t.dim),
            ]
        };
        if one_line {
            let mut keys = Vec::new();
            for (a, what) in EMPTY_HINTS {
                if !keys.is_empty() {
                    keys.push(Span::raw("   "));
                }
                keys.extend(styled(a, what));
            }
            out.push(Line::from(keys));
            hint_rows = 1;
        } else {
            for (a, what) in EMPTY_HINTS {
                out.push(Line::from(styled(a, what)));
            }
            hint_rows = 3;
        }
        out
    } else {
        let spin = state
            .anim_ms()
            .map_or(t.icons.spinner[0], |ms| t.icons.spin(ms));
        vec![Line::from(vec![
            Span::styled(format!("{spin} "), t.accent),
            Span::styled(fit("No data — waiting for the daemon".into()), t.dim),
        ])]
    };
    if area.height < 3 {
        return;
    }
    // Drop the icon and the gap when rows are short.
    let mut lines = lines;
    while lines.len() > usize::from(area.height) && lines.len() > hint_rows + 1 {
        lines.remove(0);
    }
    let h = u16::try_from(lines.len()).unwrap_or(1).min(area.height);
    let msg = Rect {
        y: area.y + area.height.saturating_sub(h) / 2,
        height: h,
        ..area
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), msg);
    // The key hints are clickable.
    if hint_rows == 1 {
        let line_y = msg.bottom().saturating_sub(1);
        let start = msg.x + u16::try_from(width.saturating_sub(one_line_w) / 2).unwrap_or(0);
        let mut x = start;
        for (a, text) in &hints {
            let w = u16::try_from(format::width(text)).unwrap_or(0);
            state.hit(Rect::new(x, line_y, w, 1), Hit::Action(*a));
            x = x.saturating_add(w + 3);
        }
    } else if hint_rows == 3 {
        for (i, (a, text)) in hints.iter().enumerate() {
            let w = u16::try_from(format::width(text))
                .unwrap_or(0)
                .min(msg.width);
            let x = msg.x + (msg.width - w) / 2;
            let y = msg.bottom().saturating_sub(3) + u16::try_from(i).unwrap_or(0);
            state.hit(Rect::new(x, y, w, 1), Hit::Action(*a));
        }
    }
}

/// The preview pane: what is under the cursor (or the marked downloads).
pub(super) fn render_preview(frame: &mut Frame<'_>, area: Rect, state: &State) {
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
