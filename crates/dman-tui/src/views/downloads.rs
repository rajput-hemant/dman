//! The downloads view: list actions, confirmations and mouse handling; the
//! drawing lives in [`render`].

mod render;

pub use render::{chip_counts, render};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use dman_proto::method::{LimitsUpdate, Reorder};
use dman_proto::{Job, JobId, JobState};
use ratatui::layout::Rect;

use crate::action::Action;
use crate::api::ApiCall;
use crate::app::{Cmd, State};
use crate::format;
use crate::layout::Hit;
use crate::list::{Chip, Row};
use crate::views::View;
use crate::widgets::Modal;
use crate::widgets::confirm::ConfirmState;
use crate::widgets::footer::{Hint, hint};
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
    Action::Search,
];

/// Footer hints for what is under the cursor (or marked), most useful first.
pub fn footer_hints(state: &State) -> Vec<Hint> {
    let h = |a: Action, label: &str| hint(state, a, label);
    let marked = state.list.selected.len();
    let job = state.list.cursor.and_then(|id| state.jobs.get(&id));
    let mut v: Vec<Option<Hint>> = if marked > 0 {
        vec![
            h(Action::Pause, &format!("pause {marked}")),
            h(Action::Resume, &format!("resume {marked}")),
            h(Action::Remove, &format!("remove {marked}")),
            h(Action::RemoveDelete, "delete files"),
            h(Action::SelectNone, "unmark"),
            h(Action::ToggleSelect, "mark"),
        ]
    } else {
        match job.map(|j| j.state) {
            None if state.list.filter.is_empty() => vec![
                h(Action::Add, "add a link or torrent"),
                h(Action::Search, "search torrents"),
            ],
            None => vec![
                h(Action::SelectNone, "clear filter"),
                h(Action::Filter, "edit filter"),
                h(Action::Add, "add"),
            ],
            Some(JobState::Active) => vec![
                h(Action::OpenDetail, "details"),
                h(Action::Pause, "pause"),
                h(Action::Remove, "remove"),
            ],
            Some(JobState::Seeding) => vec![
                h(Action::OpenDetail, "details"),
                h(Action::Pause, "stop seeding"),
                h(Action::Remove, "remove"),
            ],
            Some(JobState::Paused) => vec![
                h(Action::Resume, "resume"),
                h(Action::OpenDetail, "details"),
                h(Action::Remove, "remove"),
            ],
            Some(JobState::Waiting) => vec![
                h(Action::OpenDetail, "details"),
                h(Action::Pause, "pause"),
                h(Action::Remove, "remove"),
            ],
            Some(JobState::Error) => vec![
                h(Action::Resume, "retry"),
                h(Action::OpenDetail, "details"),
                h(Action::Remove, "remove"),
            ],
            Some(JobState::Complete | JobState::Removed) => vec![
                h(Action::OpenDetail, "details"),
                h(Action::Remove, "remove"),
            ],
        }
    };
    if marked == 0 && job.is_some() {
        v.push(h(Action::RemoveDelete, "delete files"));
        v.extend([
            h(Action::Add, "add"),
            h(Action::Search, "search"),
            h(Action::Filter, "filter"),
            h(Action::ToggleSelect, "mark"),
        ]);
    }
    v.push(h(Action::Quit, "quit"));
    v.into_iter().flatten().collect()
}

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
        state.fit_list();
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
        Action::Rename => rename_cursor(state),
        Action::CleanupUnselected => cleanup_cursor(state),
        Action::ReloadConfig => state.call(ApiCall::ReloadConfig),
        Action::ShowAll => show_chip(state, Chip::All),
        Action::ShowActive => show_chip(state, Chip::Active),
        Action::ShowWaiting => show_chip(state, Chip::Waiting),
        Action::ShowDone => show_chip(state, Chip::Done),
        Action::ShowFailed => show_chip(state, Chip::Failed),
        Action::ToggleGraph => crate::widgets::graph::toggle(state),
        Action::ToggleRemoved => {
            state.list.show_removed = !state.list.show_removed;
            state.refresh_list();
            state.status.info(if state.list.show_removed {
                "Showing removed downloads (history)"
            } else {
                "Removed downloads hidden"
            });
            Vec::new()
        }
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
        Action::OpenDetail => match state.list.cursor {
            Some(id) => {
                state
                    .views
                    .push(View::Detail(Box::new(super::detail::DetailState::new(id))));
                super::detail::fetch(state)
            }
            None => {
                state.status.info("No download under the cursor");
                Vec::new()
            }
        },
        _ => return None,
    };
    Some(cmds)
}

/// Show only the downloads of `chip` (a second press of a chip shows all).
pub fn show_chip(state: &mut State, chip: Chip) -> Vec<Cmd> {
    state.list.chip = if state.list.chip == chip && chip != Chip::All {
        Chip::All
    } else {
        chip
    };
    state.list.offset = 0;
    state.refresh_list();
    Vec::new()
}

/// List navigation, selection, sort and paging. `None` when not one of those.
fn navigate(state: &mut State, action: Action) -> Option<()> {
    let page = isize::try_from(state.list_page()).unwrap_or(1);
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
        // Esc / u: unmark; with nothing marked it clears the filter instead.
        Action::SelectNone if l.selected.is_empty() && !l.filter.is_empty() => {
            l.filter.clear();
            l.offset = 0;
            state.status.info("Filter cleared");
            state.refresh_list();
        }
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
            if action == Action::SortNext {
                crate::widgets::whichkey::open(state, crate::widgets::whichkey::Prefix::Sort);
            }
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
    confirm_remove_ids(state, ids, delete_files)
}

/// Open the remove confirmation for `ids` (also used by the detail view).
pub fn confirm_remove_ids(state: &mut State, ids: Vec<JobId>, delete_files: bool) -> Vec<Cmd> {
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
        lines.insert(
            0,
            format!("Removes {what} from the list AND deletes the files from disk:"),
        );
        lines.push(String::new());
        lines.push("This permanently deletes the files. It cannot be undone.".into());
        ConfirmState::yes_no(format!("Remove and DELETE files of {what}?"), lines, call)
            .label("Delete files")
            .destructive()
    } else {
        lines.push(String::new());
        let d = state
            .keymap
            .label(crate::keymap::Context::Downloads, Action::RemoveDelete);
        lines.push(format!(
            "Files on disk are kept ({d} removes and deletes them)."
        ));
        ConfirmState::yes_no(format!("Remove {what} from the list?"), lines, call).label("Remove")
    };
    state.modal = Some(Modal::Confirm(dialog));
    Vec::new()
}

/// Message after a successful undo of a remove.
pub fn restored_message(state: &State, ids: &[JobId]) -> String {
    match ids {
        [id] => match state.jobs.get(id) {
            Some(j) => format!("Restored '{}'", format::middle(&j.name, 50)),
            None => format!("Restored download #{id}"),
        },
        _ => format!("Restored {}", format::plural(ids.len(), "download")),
    }
}

/// Status line after a successful remove: what happened to the files.
pub fn removed_message(state: &State, ids: &[JobId], delete_files: bool) -> String {
    let what = match ids {
        [id] => match state.jobs.get(id) {
            Some(j) => format!("'{}'", format::truncate(&j.name, 50)),
            None => format!("download #{id}"),
        },
        _ => format::plural(ids.len(), "download"),
    };
    let files = match (delete_files, ids.len()) {
        (false, _) => " (files kept)",
        (true, 1) => " and deleted its files",
        (true, _) => " and deleted their files",
    };
    format!("Removed {what}{files}")
}

fn rename_cursor(state: &mut State) -> Vec<Cmd> {
    let Some(id) = state.list.cursor else {
        state.status.info("No download under the cursor");
        return Vec::new();
    };
    let name = state.jobs.get(&id).map(|j| j.name.clone());
    match name {
        Some(name) => super::rename::open(state, id, None, &name),
        None => Vec::new(),
    }
}

/// Asks the daemon which unselected partial files the cursor torrent has; the
/// answer opens the confirmation ([`cleanup_confirm`]).
fn cleanup_cursor(state: &mut State) -> Vec<Cmd> {
    let Some(id) = state.list.cursor else {
        state.status.info("No download under the cursor");
        return Vec::new();
    };
    state.call(ApiCall::Cleanup { id, dry_run: true })
}

/// Confirmation before deleting the partial files of deselected torrent files.
/// `files` are relative paths with sizes; only these are deleted.
pub fn cleanup_confirm(id: JobId, name: &str, files: &[(String, u64)]) -> ConfirmState {
    let total: u64 = files.iter().map(|f| f.1).sum();
    let mut lines = vec![format!("{name} (#{id})"), String::new()];
    for (path, bytes) in files.iter().take(MAX_LISTED) {
        lines.push(format!("{}  {}", format::size(*bytes), path));
    }
    if files.len() > MAX_LISTED {
        lines.push(format!("… and {} more", files.len() - MAX_LISTED));
    }
    lines.push(String::new());
    lines.push(
        "Only files of deselected torrent files are deleted; selected files are kept.".into(),
    );
    ConfirmState::yes_no(
        format!(
            "Delete {} unselected partial files ({})?",
            files.len(),
            format::size(total)
        ),
        lines,
        ApiCall::Cleanup { id, dry_run: false },
    )
    .label("Delete")
    .destructive()
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
    state.modal = Some(Modal::Confirm(
        ConfirmState::yes_no(format!("Purge {what}?"), lines, ApiCall::Purge(ids)).label("Purge"),
    ));
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
    .label("Shut down")
    .destructive()
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

/// Mouse: click a row to move the cursor (again on the cursor row opens the
/// detail), click the scrollbar to jump, wheel scrolls.
pub fn on_mouse(state: &mut State, m: MouseEvent, hit: Option<(Rect, Hit)>) -> Vec<Cmd> {
    match m.kind {
        MouseEventKind::ScrollDown => state.list.move_by(3),
        MouseEventKind::ScrollUp => state.list.move_by(-3),
        MouseEventKind::Down(MouseButton::Left) => match hit {
            Some((_, Hit::ListRow(row))) => {
                let again = matches!(state.list.rows.get(row), Some(Row::Job(id)) if state.list.cursor == Some(*id));
                if m.modifiers.contains(KeyModifiers::CONTROL) {
                    if state.list.click_row(row) {
                        state.list.toggle_cursor();
                    }
                } else if m.modifiers.contains(KeyModifiers::SHIFT) {
                    if state.list.click_row(row) {
                        state.list.select_range();
                    }
                } else if again {
                    return apply(state, Action::OpenDetail).unwrap_or_default();
                } else {
                    state.list.click_row(row);
                }
            }
            Some((r, Hit::ListScroll)) => {
                let n = state.list.order.len();
                if n > 0 {
                    let frac = f64::from(m.row.saturating_sub(r.y)) / f64::from(r.height.max(1));
                    // Rounded index within 0..n.
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::cast_precision_loss
                    )]
                    let i = ((frac * n as f64) as usize).min(n - 1);
                    state.list.cursor = Some(state.list.order[i]);
                }
            }
            _ => {
                state.dirty = false;
                return Vec::new();
            }
        },
        _ => {
            state.dirty = false;
            return Vec::new();
        }
    }
    state.fit_list();
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

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
