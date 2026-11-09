//! The downloads view: sectioned table, list actions and confirmations.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use dman_proto::method::{LimitsUpdate, Reorder};
use dman_proto::{Job, JobId, JobState};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row as TableRow, Table};

use crate::action::Action;
use crate::api::ApiCall;
use crate::app::{Cmd, State};
use crate::format;
use crate::list::{Row, Section};
use crate::theme::Theme;
use crate::views::View;
use crate::widgets::Modal;
use crate::widgets::confirm::ConfirmState;
use crate::widgets::prompt::{PromptKind, PromptState};

/// Footer actions of this view.
pub const FOOTER: &[Action] = &[
    Action::Pause,
    Action::Resume,
    Action::Remove,
    Action::Add,
    Action::Filter,
    Action::Palette,
    Action::Help,
    Action::Quit,
    Action::SortNext,
    Action::ToggleSelect,
    Action::OpenDetail,
];

/// Global download limit steps in bytes/s (0 = unlimited sits above the top).
const LIMIT_STEPS: &[u64] = &[
    32 << 10,
    64 << 10,
    128 << 10,
    256 << 10,
    512 << 10,
    1 << 20,
    2 << 20,
    4 << 20,
    8 << 20,
    16 << 20,
    32 << 20,
    64 << 20,
    128 << 20,
];

/// Names shown in confirmation dialogs.
const MAX_LISTED: usize = 5;

/// Apply a downloads-view action; `None` when not handled here.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    if let Some(()) = navigate(state, action) {
        let h = state.list_height();
        state.list.ensure_visible(h);
        return Some(Vec::new());
    }
    let cmds = match action {
        Action::Pause => with_targets(state, ApiCall::Pause),
        Action::Resume => with_targets(state, ApiCall::Resume),
        Action::PauseAll => state.call(ApiCall::PauseAll),
        Action::ResumeAll => state.call(ApiCall::ResumeAll),
        Action::Remove => confirm_remove(state, false),
        Action::RemoveDelete => confirm_remove(state, true),
        Action::Purge => confirm_purge(state),
        Action::MoveUp => reorder(state, Reorder::By(-1)),
        Action::MoveDown => reorder(state, Reorder::By(1)),
        Action::MoveTop => reorder(state, Reorder::Top),
        Action::MoveBottom => reorder(state, Reorder::Bottom),
        Action::LimitUp => change_limit(state, true),
        Action::LimitDown => change_limit(state, false),
        Action::SetMaxConcurrent => {
            let current = state.limits.max_concurrent.to_string();
            state.modal = Some(Modal::Prompt(PromptState::new(
                PromptKind::MaxConcurrent,
                "Max concurrent downloads",
                "Number of downloads running at the same time (1-1000):",
                &current,
            )));
            Vec::new()
        }
        Action::OpenDetail => {
            match state.list.cursor {
                Some(id) => state
                    .views
                    .push(View::Detail(super::detail::DetailState { id })),
                None => state.status.info("No download under the cursor"),
            }
            Vec::new()
        }
        _ => return None,
    };
    Some(cmds)
}

/// List navigation, selection, sort and paging. `None` when not one of those.
fn navigate(state: &mut State, action: Action) -> Option<()> {
    let page = isize::try_from(state.list_height().max(1)).unwrap_or(1);
    let l = &mut state.list;
    match action {
        Action::Up => l.move_by(-1),
        Action::Down => l.move_by(1),
        Action::PageUp => l.move_by(-page),
        Action::PageDown => l.move_by(page),
        Action::Top => l.to_top(),
        Action::Bottom => l.to_bottom(),
        Action::ToggleSelect => {
            l.toggle_cursor();
            l.move_by(1);
        }
        Action::SelectAll => l.select_all(),
        Action::SelectNone => l.select_none(),
        Action::SelectRange => l.select_range(),
        Action::SortNext | Action::SortReverse => {
            if action == Action::SortNext {
                l.sort = l.sort.next();
            } else {
                l.reverse = !l.reverse;
            }
            let rev = if l.reverse { ", reversed" } else { "" };
            let text = format!("Sorted by {}{rev}", l.sort.label());
            state.status.info(text);
            state.refresh_list();
        }
        Action::ClearFilter => {
            l.filter.clear();
            state.status.info("Filter cleared");
            state.refresh_list();
        }
        Action::StoppedNextPage | Action::StoppedPrevPage => {
            let pages = l.stopped_pages();
            l.stopped_page = if action == Action::StoppedNextPage {
                (l.stopped_page + 1).min(pages - 1)
            } else {
                l.stopped_page.saturating_sub(1)
            };
            let text = format!("Stopped downloads: page {}/{pages}", l.stopped_page + 1);
            state.status.info(text);
            state.refresh_list();
        }
        _ => return None,
    }
    Some(())
}

fn with_targets(state: &mut State, make: fn(Vec<JobId>) -> ApiCall) -> Vec<Cmd> {
    let ids = state.list.targets();
    if ids.is_empty() {
        state.status.info("Nothing selected");
        return Vec::new();
    }
    state.call(make(ids))
}

fn names(state: &State, ids: &[JobId]) -> Vec<String> {
    let mut out: Vec<String> = ids
        .iter()
        .take(MAX_LISTED)
        .map(|id| {
            let name = state.jobs.get(id).map_or("?", |j| j.name.as_str());
            format!("  • {}", format::truncate(name, 52))
        })
        .collect();
    if ids.len() > MAX_LISTED {
        out.push(format!("  … and {} more", ids.len() - MAX_LISTED));
    }
    out
}

fn confirm_remove(state: &mut State, delete_files: bool) -> Vec<Cmd> {
    let ids = state.list.targets();
    if ids.is_empty() {
        state.status.info("Nothing selected");
        return Vec::new();
    }
    let what = format::plural(ids.len(), "download");
    let mut lines = names(state, &ids);
    let call = ApiCall::Remove {
        ids: ids.clone(),
        delete_files,
    };
    let dialog = if delete_files {
        lines.push(String::new());
        lines.push("Next you will be asked to confirm deleting the files.".into());
        let second = ConfirmState::typed(
            "Delete files from disk?",
            vec![
                format!("This permanently DELETES the files of {what} from disk."),
                "It cannot be undone.".into(),
            ],
            call,
        );
        ConfirmState::then(format!("Remove {what}?"), lines, second)
    } else {
        lines.push(String::new());
        lines.push("Files on disk are kept.".into());
        ConfirmState::yes_no(format!("Remove {what}?"), lines, call)
    };
    state.modal = Some(Modal::Confirm(dialog));
    Vec::new()
}

fn confirm_purge(state: &mut State) -> Vec<Cmd> {
    let stopped = |j: &&Job| j.state.is_stopped();
    let selected: Vec<JobId> = state
        .list
        .selected
        .iter()
        .filter_map(|id| state.jobs.get(id))
        .filter(stopped)
        .map(|j| j.id)
        .collect();
    let mut ids = if selected.is_empty() {
        state.jobs.values().filter(stopped).map(|j| j.id).collect()
    } else {
        selected
    };
    if ids.is_empty() {
        state.status.info("No stopped downloads to purge");
        return Vec::new();
    }
    ids.sort();
    let what = format::plural(ids.len(), "stopped download");
    let mut lines = names(state, &ids);
    lines.push(String::new());
    lines.push("They are dropped from the list; files on disk are kept.".into());
    state.modal = Some(Modal::Confirm(ConfirmState::yes_no(
        format!("Purge {what}?"),
        lines,
        ApiCall::Purge(ids),
    )));
    Vec::new()
}

/// The shutdown confirmation dialog.
pub fn shutdown_confirm() -> ConfirmState {
    ConfirmState::yes_no(
        "Shut down the daemon?",
        vec![
            "Stops the dman daemon and its engines; all transfers stop.".into(),
            "Downloads resume when the daemon starts again.".into(),
        ],
        ApiCall::Shutdown,
    )
}

fn reorder(state: &mut State, to: Reorder) -> Vec<Cmd> {
    let Some(id) = state.list.cursor else {
        state.status.info("No download under the cursor");
        return Vec::new();
    };
    let queued = state
        .jobs
        .get(&id)
        .is_some_and(|j| j.queue_position.is_some());
    if !queued {
        state
            .status
            .info("Only waiting or paused downloads can be moved in the queue");
        return Vec::new();
    }
    state.call(ApiCall::Reorder { id, to })
}

/// Next global download limit. `None` when already at the end of the scale.
pub fn next_limit(current: u64, speed: u64, up: bool) -> Option<u64> {
    if up {
        if current == 0 {
            return None;
        }
        Some(
            LIMIT_STEPS
                .iter()
                .copied()
                .find(|s| *s > current)
                .unwrap_or(0),
        )
    } else if current == 0 {
        // From unlimited, start just below the current speed.
        let below = LIMIT_STEPS.iter().copied().rfind(|s| *s < speed);
        Some(below.unwrap_or(1 << 20))
    } else {
        LIMIT_STEPS.iter().copied().rfind(|s| *s < current)
    }
}

fn change_limit(state: &mut State, up: bool) -> Vec<Cmd> {
    match next_limit(state.limits.download, state.stats.download_speed, up) {
        Some(download) => state.call(ApiCall::SetLimits(LimitsUpdate {
            download: Some(download),
            ..LimitsUpdate::default()
        })),
        None => {
            let edge = if up {
                "unlimited"
            } else {
                "at the lowest step"
            };
            state.status.info(format!("Download limit already {edge}"));
            Vec::new()
        }
    }
}

/// Mouse: click moves the cursor, wheel scrolls.
pub fn on_mouse(state: &mut State, m: MouseEvent) {
    let body = state.layout().body;
    match m.kind {
        MouseEventKind::ScrollDown => state.list.move_by(3),
        MouseEventKind::ScrollUp => state.list.move_by(-3),
        MouseEventKind::Down(MouseButton::Left) => {
            let first = body.y + 1; // below the header row
            if m.row < first || m.row >= body.y + body.height {
                state.dirty = false;
                return;
            }
            let row = state.list.offset + usize::from(m.row - first);
            state.list.click_row(row);
        }
        _ => {
            state.dirty = false;
            return;
        }
    }
    let h = state.list_height();
    state.list.ensure_visible(h);
}

/// Table columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Mark,
    Name,
    Status,
    Progress,
    Down,
    Up,
    Eta,
    Size,
    Peers,
    Ratio,
}

/// Display order with width; Name takes the rest.
const DISPLAY: &[(Col, &str, u16)] = &[
    (Col::Mark, "", 1),
    (Col::Name, "Name", 0),
    (Col::Status, "Status", 7),
    (Col::Progress, "Progress", 13),
    (Col::Down, "Down", 11),
    (Col::Up, "Up", 11),
    (Col::Eta, "ETA", 6),
    (Col::Size, "Size", 10),
    (Col::Peers, "Peers", 7),
    (Col::Ratio, "Ratio", 5),
];

/// Optional columns, most important first.
const PRIORITY: &[Col] = &[
    Col::Status,
    Col::Progress,
    Col::Down,
    Col::Eta,
    Col::Size,
    Col::Up,
    Col::Peers,
    Col::Ratio,
];

const MIN_NAME: u16 = 22;

fn width_of(c: Col) -> u16 {
    DISPLAY.iter().find(|d| d.0 == c).map_or(0, |d| d.2)
}

/// Columns that fit in `width`, in display order.
fn columns(width: u16) -> Vec<Col> {
    let mut chosen = vec![Col::Mark, Col::Name];
    // Mark + its gap.
    let mut used: u16 = width_of(Col::Mark) + 1;
    for c in PRIORITY {
        let need = width_of(*c) + 1;
        if width.saturating_sub(used + need) >= MIN_NAME {
            used += need;
            chosen.push(*c);
        }
    }
    DISPLAY
        .iter()
        .map(|d| d.0)
        .filter(|c| chosen.contains(c))
        .collect()
}

fn state_style(t: &Theme, s: JobState) -> Style {
    match s {
        JobState::Active | JobState::Seeding => t.ok,
        JobState::Paused => t.warn,
        JobState::Error => t.error,
        JobState::Waiting | JobState::Removed => t.dim,
        JobState::Complete => t.text,
    }
}

fn name_cell(j: &Job, t: &Theme, width: usize) -> Cell<'static> {
    let name = format::truncate(&j.name, width);
    let used = name.chars().count();
    let mut spans = vec![Span::raw(name)];
    if let Some(e) = &j.error
        && used + 4 < width
    {
        let err = format::truncate(&format!(" ({e})"), width - used);
        spans.push(Span::styled(err, t.error));
    }
    Cell::from(Line::from(spans))
}

/// Width left for the Name column.
fn name_width(cols: &[Col], total: u16) -> usize {
    let others: u16 = cols.iter().map(|c| width_of(*c)).sum();
    let gaps = u16::try_from(cols.len().saturating_sub(1)).unwrap_or(0);
    usize::from(total.saturating_sub(others + gaps))
}

fn job_cell(c: Col, j: &Job, t: &Theme, marked: bool, name_w: usize) -> Cell<'static> {
    match c {
        Col::Mark => Cell::from(if marked { "●" } else { " " }).style(t.marked),
        Col::Name => name_cell(j, t, name_w),
        Col::Status => Cell::from(format::state(j.state)).style(state_style(t, j.state)),
        Col::Progress => Cell::from(format::progress_bar(j, 8)),
        Col::Down => Cell::from(right(format::speed_or_dash(j.download_speed))),
        Col::Up => Cell::from(right(format::speed_or_dash(j.upload_speed))),
        Col::Eta => Cell::from(right(eta(j))),
        Col::Size => Cell::from(right(format::size_or_dash(j.total_bytes))),
        Col::Peers => Cell::from(right(format::peers(j))),
        Col::Ratio => Cell::from(right(format::ratio(j))),
    }
}

fn eta(j: &Job) -> String {
    match j.state {
        JobState::Active => format::eta(j.eta_secs()),
        _ => "-".into(),
    }
}

fn right(s: String) -> Line<'static> {
    Line::from(s).alignment(Alignment::Right)
}

fn section_label(section: Section, total: usize, page: usize, pages: usize) -> String {
    if pages > 1 {
        format!(
            "{} ({total}) · page {}/{pages} · [ ] to page",
            section.title(),
            page + 1
        )
    } else {
        format!("{} ({total})", section.title())
    }
}

/// Draw the table into `area`.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let cols = columns(area.width);
    let header = TableRow::new(cols.iter().map(|c| {
        let name = DISPLAY.iter().find(|d| d.0 == *c).map_or("", |d| d.1);
        let w = usize::from(width_of(*c));
        match c {
            Col::Mark | Col::Name | Col::Status | Col::Progress => Cell::from(name),
            _ => Cell::from(format!("{name:>w$}")),
        }
    }))
    .style(t.header);
    let widths: Vec<Constraint> = cols
        .iter()
        .map(|c| match c {
            Col::Name => Constraint::Fill(1),
            other => Constraint::Length(width_of(*other)),
        })
        .collect();
    let height = usize::from(area.height.saturating_sub(1));
    let offline = !state.connected();
    let name_w = name_width(&cols, area.width);
    let l = &state.list;
    let rows = l.rows.iter().skip(l.offset).take(height).map(|r| match *r {
        Row::Section {
            section,
            total,
            page,
            pages,
        } => {
            let mut cells = vec![Cell::from(""); cols.len()];
            cells[1] = Cell::from(section_label(section, total, page, pages));
            TableRow::new(cells).style(t.section)
        }
        Row::Job(id) => job_row(state, &cols, id, offline, name_w),
    });
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
    if l.rows.is_empty() {
        render_empty(frame, area, state);
    }
}

fn job_row(
    state: &State,
    cols: &[Col],
    id: JobId,
    offline: bool,
    name_w: usize,
) -> TableRow<'static> {
    let t = &state.theme;
    let Some(j) = state.jobs.get(&id) else {
        return TableRow::new(Vec::<Cell<'_>>::new());
    };
    let marked = state.list.selected.contains(&id);
    let row = if offline {
        let plain = Theme {
            ok: t.dim,
            warn: t.dim,
            error: t.dim,
            marked: t.dim,
            ..t.clone()
        };
        TableRow::new(cols.iter().map(|c| job_cell(*c, j, &plain, marked, name_w))).style(t.dim)
    } else {
        let row = TableRow::new(cols.iter().map(|c| job_cell(*c, j, t, marked, name_w)));
        if marked { row.style(t.marked) } else { row }
    };
    if state.list.cursor == Some(id) {
        row.style(if offline {
            t.dim.patch(t.cursor)
        } else {
            t.cursor
        })
    } else {
        row
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let text = if !state.list.filter.is_empty() {
        format!("No downloads match '{}'", state.list.filter)
    } else if state.connected() {
        let add = state
            .keymap
            .label(crate::keymap::Context::Downloads, Action::Add);
        format!("No downloads yet — press {add} to add one")
    } else {
        "No data — waiting for the daemon".into()
    };
    if area.height < 3 {
        return;
    }
    let msg = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    frame.render_widget(
        Paragraph::new(Span::styled(text, t.dim)).alignment(Alignment::Center),
        msg,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_terminals_hide_low_priority_columns() {
        let c80 = columns(80);
        assert!(c80.contains(&Col::Eta) && c80.contains(&Col::Size));
        assert!(!c80.contains(&Col::Ratio) && !c80.contains(&Col::Up));
        let c120 = columns(120);
        assert_eq!(c120.len(), DISPLAY.len());
        let c40 = columns(40);
        assert!(c40.contains(&Col::Name) && c40.len() <= 4);
    }

    #[test]
    fn limit_ladder() {
        assert_eq!(next_limit(0, 0, true), None);
        assert_eq!(next_limit(1 << 20, 0, true), Some(2 << 20));
        assert_eq!(next_limit(128 << 20, 0, true), Some(0));
        assert_eq!(next_limit(0, 3 << 20, false), Some(2 << 20));
        assert_eq!(next_limit(0, 0, false), Some(1 << 20));
        assert_eq!(next_limit(32 << 10, 0, false), None);
    }
}
