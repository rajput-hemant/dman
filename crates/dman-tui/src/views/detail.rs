//! Detail view of one download: header plus six tabs (files, peers, trackers,
//! pieces, speed, options).
//!
//! The view owns the last `jobs.detail` reply. It is fetched when the view
//! opens and re-fetched about once a second while it is the top view
//! ([`on_tick`]); the header is drawn from `State::jobs`, which the daemon's
//! live events keep current. Edits (file selection, options) go out as
//! `jobs.set_options` calls and the view re-reads the detail when they finish.

mod options;
mod render;

use std::time::Duration;

use dman_proto::{Job, JobDetail, JobId, JobOptions, JobState};

use crate::action::Action;
use crate::api::{ApiCall, ApiReply};
use crate::app::{Cmd, State};
use crate::views::{View, downloads, rename};
use crate::widgets::Modal;
use crate::widgets::footer::{Hint, hint};
use crate::widgets::prompt::{PromptKind, PromptState};

pub use options::{OptField, OptRow, option_rows};
pub use render::render;

/// Footer actions of this view.
pub const FOOTER: &[Action] = &[
    Action::Back,
    Action::DetailNextTab,
    Action::DetailToggleFile,
    Action::DetailEditOption,
    Action::Pause,
    Action::Help,
    Action::Resume,
    Action::Quit,
];

/// Footer hints for the shown job and tab, most useful first.
pub fn footer_hints(state: &State, d: &DetailState) -> Vec<Hint> {
    let h = |a: Action, label: &str| hint(state, a, label);
    let job = state.jobs.get(&d.id);
    let editable = job.is_some_and(|j| j.state.is_renamable_in_place());
    let mut v = vec![h(Action::Back, "back")];
    match job.map(|j| j.state) {
        Some(JobState::Paused) => v.push(h(Action::Resume, "resume")),
        Some(JobState::Error) => v.push(h(Action::Resume, "retry")),
        Some(JobState::Seeding) => v.push(h(Action::Pause, "stop seeding")),
        Some(JobState::Active | JobState::Waiting) => v.push(h(Action::Pause, "pause")),
        _ => {}
    }
    match d.tab {
        Tab::Files if editable => {
            v.push(h(Action::DetailToggleFile, "toggle file"));
            v.push(h(Action::DetailToggleAllFiles, "all files"));
        }
        Tab::Options => v.push(h(Action::DetailEditOption, "edit")),
        _ => {}
    }
    v.push(h(Action::DetailNextTab, "next tab"));
    v.push(h(Action::Remove, "remove"));
    v.push(h(Action::RemoveDelete, "delete files"));
    v.push(h(Action::Rename, "rename"));
    v.into_iter().flatten().collect()
}

/// Whether `job` is a magnet whose torrent metadata has not arrived yet.
pub fn awaiting_metadata(job: &Job, detail: Option<&JobDetail>) -> bool {
    use dman_proto::SourceKind;
    if job.kind != SourceKind::Magnet || job.state.is_stopped() {
        return false;
    }
    let placeholder = detail.is_none_or(|d| {
        d.files.is_empty() || d.files.iter().all(|f| f.path.contains("[METADATA]"))
    });
    job.total_bytes == 0 && placeholder
}

/// One line saying what is happening and what can be done about it.
pub fn summary(state: &State, job: &Job, detail: Option<&JobDetail>) -> String {
    let key = |a: Action| {
        state
            .keymap
            .keys_with_global(crate::keymap::Context::Detail, a)
            .first()
            .map_or_else(String::new, |k| crate::keymap::key_label(*k))
    };
    if awaiting_metadata(job, detail) {
        return if job.state == JobState::Paused {
            format!(
                "Paused before the torrent's details arrived — press {} to resume",
                key(Action::Resume)
            )
        } else {
            "Fetching torrent metadata… (asking peers for the file list)".to_owned()
        };
    }
    match job.state {
        JobState::Active => {
            let mut parts = vec!["Downloading".to_owned()];
            if job.seeders.is_some() || job.info_hash.is_some() {
                parts.push(crate::format::plural(job.connections as usize, "peer"));
            } else if job.connections > 0 {
                parts.push(crate::format::plural(
                    job.connections as usize,
                    "connection",
                ));
            }
            if job.download_speed > 0 {
                parts.push(crate::format::speed(job.download_speed));
            }
            match job.eta_secs() {
                Some(_) => parts.push(format!("ETA {}", crate::format::eta(job.eta_secs()))),
                None if job.download_speed == 0 => parts.push("waiting for data".to_owned()),
                None => {}
            }
            parts.join(" · ")
        }
        JobState::Waiting => match job.queue_position {
            Some(0) => "Queued — starts next, when a download slot is free".to_owned(),
            Some(n) => format!("Queued — {n} ahead of it; starts when a slot is free"),
            None => "Waiting to start".to_owned(),
        },
        JobState::Paused => format!("Paused — press {} to resume", key(Action::Resume)),
        JobState::Seeding => format!(
            "Seeding (sharing with others) — press {} to stop",
            key(Action::Pause)
        ),
        JobState::Complete => "Finished — the files are in the folder below".to_owned(),
        JobState::Error => format!(
            "Failed — press {} to retry (partial files are kept)",
            key(Action::Resume)
        ),
        JobState::Removed => "Removed from the queue — files on disk are kept".to_owned(),
    }
}

/// How often the detail is re-read while the view is open.
pub const REFRESH_EVERY: Duration = Duration::from_secs(1);

/// The tabs of the detail view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    /// Files with progress and selection.
    #[default]
    Files,
    /// Connected peers.
    Peers,
    /// Trackers.
    Trackers,
    /// Piece map.
    Pieces,
    /// Speed history chart.
    Speed,
    /// Editable options.
    Options,
}

impl Tab {
    /// All tabs, in display order (key `1` … `6`).
    pub const ALL: [Tab; 6] = [
        Tab::Files,
        Tab::Peers,
        Tab::Trackers,
        Tab::Pieces,
        Tab::Speed,
        Tab::Options,
    ];

    /// Tab title.
    pub const fn label(self) -> &'static str {
        match self {
            Tab::Files => "Files",
            Tab::Peers => "Peers",
            Tab::Trackers => "Trackers",
            Tab::Pieces => "Pieces",
            Tab::Speed => "Speed",
            Tab::Options => "Options",
        }
    }

    /// Tabs that only make sense for torrents.
    pub const fn torrent_only(self) -> bool {
        matches!(self, Tab::Peers | Tab::Trackers | Tab::Pieces)
    }

    /// The tabs shown for a job (torrent-only tabs are hidden for HTTP, FTP
    /// and video jobs).
    pub fn visible(torrent: bool) -> Vec<Tab> {
        Tab::ALL
            .iter()
            .copied()
            .filter(|t| torrent || !t.torrent_only())
            .collect()
    }

    /// The next of `tabs` after this one, wrapping.
    pub fn step(self, tabs: &[Tab], forward: bool) -> Tab {
        let n = tabs.len().max(1);
        match tabs.iter().position(|t| *t == self) {
            Some(i) => {
                tabs[if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                }]
            }
            None => Tab::Files,
        }
    }

    /// Position in [`Tab::ALL`].
    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// The following tab, wrapping.
    pub fn next(self) -> Tab {
        Tab::ALL[(self.index() + 1) % Tab::ALL.len()]
    }

    /// The preceding tab, wrapping.
    pub fn prev(self) -> Tab {
        Tab::ALL[(self.index() + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }
}

/// Detail view state.
#[derive(Debug, Clone, PartialEq)]
pub struct DetailState {
    /// The job shown.
    pub id: JobId,
    /// Active tab.
    pub tab: Tab,
    /// Row cursor of the active tab (files, peers, trackers, options).
    pub cursor: usize,
    /// Last `jobs.detail` reply.
    pub detail: Option<JobDetail>,
    /// Why the last refresh failed.
    pub error: Option<String>,
    /// Time since the last refresh was requested.
    pub since_refresh: Duration,
    /// A `jobs.detail` call is in flight.
    pub inflight: bool,
}

impl DetailState {
    /// A view of job `id` that has not loaded anything yet.
    pub fn new(id: JobId) -> Self {
        Self {
            id,
            tab: Tab::default(),
            cursor: 0,
            detail: None,
            error: None,
            since_refresh: Duration::ZERO,
            inflight: false,
        }
    }

    /// Number of cursor rows on the active tab.
    pub fn rows(&self) -> usize {
        let Some(d) = &self.detail else { return 0 };
        match self.tab {
            Tab::Files => d.files.len(),
            Tab::Peers => d.peers.len(),
            Tab::Trackers => d.trackers.len(),
            Tab::Options => option_rows(&d.options).len(),
            Tab::Pieces | Tab::Speed => 0,
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.cursor = 0;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let last = self.rows().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }
}

fn top(state: &mut State) -> Option<&mut DetailState> {
    match state.views.last_mut() {
        Some(View::Detail(d)) => Some(d),
        _ => None,
    }
}

/// Request `jobs.detail` for the open view (nothing while disconnected).
pub fn fetch(state: &mut State) -> Vec<Cmd> {
    if !state.connected() {
        return Vec::new();
    }
    match top(state) {
        Some(d) => {
            d.inflight = true;
            d.since_refresh = Duration::ZERO;
            vec![Cmd::Api(ApiCall::Detail(d.id))]
        }
        None => Vec::new(),
    }
}

/// Re-read the detail of `id` if its view is on top (after an edit).
pub fn refetch(state: &mut State, id: JobId) -> Vec<Cmd> {
    if top(state).is_some_and(|d| d.id == id) {
        fetch(state)
    } else {
        Vec::new()
    }
}

/// Tick: refresh once a second while the detail is the top view.
pub fn on_tick(state: &mut State) -> Vec<Cmd> {
    let tick = state.tick;
    let Some(d) = top(state) else {
        return Vec::new();
    };
    d.since_refresh += tick;
    if d.since_refresh >= REFRESH_EVERY && !d.inflight {
        fetch(state)
    } else {
        Vec::new()
    }
}

/// The connection dropped: calls in flight will never answer.
pub fn on_disconnect(state: &mut State) {
    for v in &mut state.views {
        if let View::Detail(d) = v {
            d.inflight = false;
        }
    }
}

/// A `jobs.detail` call finished.
pub fn on_detail_result(state: &mut State, id: JobId, result: &Result<ApiReply, String>) {
    for v in &mut state.views {
        let View::Detail(d) = v else { continue };
        if d.id != id {
            continue;
        }
        d.inflight = false;
        match result {
            Ok(ApiReply::Detail(detail)) => {
                let mut detail = (**detail).clone();
                detail
                    .peers
                    .sort_by_key(|p| std::cmp::Reverse(p.download_speed));
                d.detail = Some(detail);
                d.error = None;
                d.move_cursor(0);
            }
            Ok(_) => {}
            Err(e) => d.error = Some(e.clone()),
        }
    }
}

/// Return to the list when the job of an open detail view is gone.
pub fn close_if_gone(state: &mut State) {
    let gone = state.views.iter().enumerate().find_map(|(i, v)| match v {
        View::Detail(d) if !state.jobs.contains_key(&d.id) => Some((i, d.id)),
        _ => None,
    });
    if let Some((i, id)) = gone {
        state.views.truncate(i);
        if matches!(state.modal, Some(Modal::Prompt(_))) {
            state.modal = None;
        }
        state
            .status
            .warn(format!("Download #{id} was removed; back to the list"));
    }
}

/// Whether the job has torrent data (peers, trackers, pieces). Unknown jobs
/// count as torrents so nothing is hidden by mistake.
pub fn is_torrent(state: &State, id: JobId) -> bool {
    use dman_proto::SourceKind;
    state.jobs.get(&id).is_none_or(|j| {
        !matches!(
            j.kind,
            SourceKind::Http | SourceKind::Ftp | SourceKind::Sftp | SourceKind::Video
        )
    })
}

fn target_tab(action: Action) -> Tab {
    match action {
        Action::DetailPeers => Tab::Peers,
        Action::DetailTrackers => Tab::Trackers,
        _ => Tab::Pieces,
    }
}

/// Apply a detail-view action; `None` when not handled here.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    let (id, tab) = match state.view() {
        View::Detail(d) => (d.id, d.tab),
        _ => return None,
    };
    let page = isize::try_from(state.list_height().max(1)).unwrap_or(1);
    let tabs = Tab::visible(is_torrent(state, id));
    let d = top(state)?;
    match action {
        Action::Up => d.move_cursor(-1),
        Action::Down => d.move_cursor(1),
        Action::PageUp => d.move_cursor(-page),
        Action::PageDown => d.move_cursor(page),
        Action::Top => d.cursor = 0,
        Action::Bottom => d.cursor = d.rows().saturating_sub(1),
        Action::DetailNextTab => d.set_tab(tab.step(&tabs, true)),
        Action::DetailPrevTab => d.set_tab(tab.step(&tabs, false)),
        Action::DetailFiles => d.set_tab(Tab::Files),
        Action::DetailPeers | Action::DetailTrackers | Action::DetailPieces
            if !tabs.contains(&target_tab(action)) =>
        {
            state
                .status
                .info("Peers, trackers and pieces are only shown for torrents");
        }
        Action::DetailPeers => d.set_tab(Tab::Peers),
        Action::DetailTrackers => d.set_tab(Tab::Trackers),
        Action::DetailPieces => d.set_tab(Tab::Pieces),
        Action::DetailSpeed => d.set_tab(Tab::Speed),
        Action::DetailOptions => d.set_tab(Tab::Options),
        Action::Pause => return Some(state.call(ApiCall::Pause(vec![id]))),
        Action::Resume => return Some(state.call(ApiCall::Resume(vec![id]))),
        Action::Remove => return Some(downloads::confirm_remove_ids(state, vec![id], false)),
        Action::RemoveDelete => return Some(downloads::confirm_remove_ids(state, vec![id], true)),
        Action::DetailToggleFile => return Some(toggle_files(state, false)),
        Action::DetailToggleAllFiles => return Some(toggle_files(state, true)),
        Action::DetailEditOption => return Some(edit_option(state)),
        Action::Rename => return Some(rename(state)),
        _ => return None,
    }
    Some(Vec::new())
}

/// Why file selection cannot change now.
fn selection_blocked(state: &State, id: JobId) -> Option<&'static str> {
    match state.jobs.get(&id).map(|j| j.state) {
        Some(JobState::Waiting | JobState::Paused) => None,
        Some(_) => {
            Some("Files can only be selected while waiting or paused: pause the download first")
        }
        None => Some("The download is gone"),
    }
}

fn toggle_files(state: &mut State, all: bool) -> Vec<Cmd> {
    let Some(d) = top(state) else {
        return Vec::new();
    };
    let id = d.id;
    if d.tab != Tab::Files {
        state.status.info("Select files on the Files tab (key 1)");
        return Vec::new();
    }
    if let Some(reason) = selection_blocked(state, id) {
        state.status.warn(reason);
        return Vec::new();
    }
    let Some(d) = top(state) else {
        return Vec::new();
    };
    let cursor = d.cursor;
    let Some(detail) = d.detail.as_mut() else {
        state.status.info("Files are not loaded yet");
        return Vec::new();
    };
    if all {
        if detail.files.iter().all(|f| f.selected) {
            state
                .status
                .info("All files are selected already (Space deselects one)");
            return Vec::new();
        }
        for f in &mut detail.files {
            f.selected = true;
        }
    } else if let Some(f) = detail.files.get_mut(cursor) {
        f.selected = !f.selected;
    } else {
        return Vec::new();
    }
    let chosen: Vec<u32> = detail
        .files
        .iter()
        .filter(|f| f.selected)
        .map(|f| f.index)
        .collect();
    if chosen.is_empty() {
        // Undo the local flip; aria2 needs at least one file.
        if let Some(f) = detail.files.get_mut(cursor) {
            f.selected = true;
        }
        state.status.warn("At least one file must stay selected");
        return Vec::new();
    }
    state.call(ApiCall::SetOptions {
        id,
        options: Box::new(JobOptions {
            select_files: Some(chosen),
            ..JobOptions::default()
        }),
    })
}

/// The file name part of a path inside the torrent.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Rename the file under the cursor (Files tab) or the whole download.
fn rename(state: &mut State) -> Vec<Cmd> {
    let Some(d) = top(state) else {
        return Vec::new();
    };
    let (id, tab, cursor) = (d.id, d.tab, d.cursor);
    if !rename::allowed(state, id) {
        return Vec::new();
    }
    let Some(job) = state.jobs.get(&id) else {
        return Vec::new();
    };
    let (job_name, complete) = (job.name.clone(), job.state == JobState::Complete);
    let files = top(state)
        .and_then(|d| d.detail.as_ref())
        .map(|d| d.files.clone())
        .unwrap_or_default();
    if tab == Tab::Files
        && let Some(f) = files.get(cursor)
    {
        return rename::open(state, id, Some(f.index), basename(&f.path));
    }
    if files.len() > 1 && !complete {
        state
            .status
            .info("This download has several files: rename one on the Files tab (key 1)");
        return Vec::new();
    }
    rename::open(state, id, None, &job_name)
}

fn edit_option(state: &mut State) -> Vec<Cmd> {
    let Some(d) = top(state) else {
        return Vec::new();
    };
    let id = d.id;
    if d.tab != Tab::Options {
        state.status.info("Edit options on the Options tab (key 6)");
        return Vec::new();
    }
    let Some(detail) = &d.detail else {
        state.status.info("Options are not loaded yet");
        return Vec::new();
    };
    let rows = option_rows(&detail.options);
    let Some(row) = rows.get(d.cursor) else {
        return Vec::new();
    };
    let Some(field) = row.field else {
        let text = format!("{} cannot be edited here: {}", row.label, row.note);
        state.status.info(text);
        return Vec::new();
    };
    if field == OptField::Sequential {
        let now = detail.options.sequential.unwrap_or(false);
        return state.call(ApiCall::SetOptions {
            id,
            options: Box::new(JobOptions {
                sequential: Some(!now),
                ..JobOptions::default()
            }),
        });
    }
    let prompt = PromptState::new(
        PromptKind::JobOption(field),
        &format!("Edit {}", row.label),
        field.help(),
        &row.edit_value,
    );
    state.modal = Some(Modal::Prompt(prompt));
    Vec::new()
}

/// A prompt for option `field` was submitted with `value`.
pub fn submit_option(state: &mut State, field: OptField, value: &str) -> Vec<Cmd> {
    let Some(id) = top(state).map(|d| d.id) else {
        return Vec::new();
    };
    match field.build(value) {
        Ok(options) => state.call(ApiCall::SetOptions {
            id,
            options: Box::new(options),
        }),
        Err(e) => {
            state.status.error(format!("{}: {e}", field.label()));
            Vec::new()
        }
    }
}
