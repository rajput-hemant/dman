//! Elm-style core: [`State`], [`Msg`], [`Cmd`] and the pure [`update`] function.
//!
//! `update` never performs I/O. Anything with a side effect is returned as a
//! [`Cmd`] and executed by the runtime, which feeds results back as [`Msg`]s.
//!
//! Key routing, in order:
//! 1. an open [`Modal`] gets the key ([`crate::widgets::Modal::handle_key`]);
//! 2. the top [`View`] may consume raw keys ([`crate::views::handle_raw_key`],
//!    for text-entry views such as the add view);
//! 3. the [`Keymap`] resolves the key in the view's [`Context`] (falling back to
//!    the global context) to an [`Action`], applied by [`apply_action`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyEvent, KeyEventKind, MouseEvent};
use dman_client::ConnState;
use dman_proto::method::Outcome as JobOutcome;
use dman_proto::method::{EngineStatus, Event, Level};
use dman_proto::{GlobalStats, Job, JobId, Limits};

use crate::action::Action;
use crate::api::{ApiCall, ApiReply, Outcome, PrefsIntent, summarize};
use crate::keymap::{Context, Keymap};
use crate::layout::ScreenLayout;
use crate::list::DownloadsList;
use crate::theme::Theme;
use crate::views::add::browser::LocalListing;
use crate::views::add::preview::Loaded;
use crate::views::add::source::PathStatus;
use crate::views::{self, View};
use crate::widgets::graph::{self, GraphState};
use crate::widgets::palette::PaletteState;
use crate::widgets::prompt::PromptKind;
use crate::widgets::{FilterState, Modal, ModalOutcome};

/// Input to [`update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// A key press.
    Key(KeyEvent),
    /// A mouse event (only delivered when mouse support is on).
    Mouse(MouseEvent),
    /// Bracketed paste.
    Paste(String),
    /// Terminal resized to (columns, rows).
    Resize(u16, u16),
    /// Pushed by the daemon.
    Event(Event),
    /// The connection state changed.
    Conn(ConnState),
    /// An [`ApiCall`] finished.
    ActionResult {
        /// What was called.
        call: ApiCall,
        /// The reply, or the error text.
        result: Result<ApiReply, String>,
    },
    /// A local directory listing for the add view's file browser.
    LocalDir(Result<LocalListing, String>),
    /// Local source files were checked.
    PathsChecked(Vec<(String, PathStatus)>),
    /// Local source files were read for the preview.
    FilesRead(Loaded),
    /// A providers file was read: `(path, text)`.
    ProviderFile(Result<(String, String), String>),
    /// A providers file was written: the path.
    ProviderFileWritten(Result<String, String>),
    /// The clipboard text, or why it could not be read.
    Clipboard(Result<String, String>),
    /// Events were dropped (slow consumer); state must be re-fetched.
    Lagged,
    /// Periodic tick (every `refresh_ms`).
    Tick,
    /// SIGTERM / SIGHUP: leave cleanly.
    Terminate,
}

/// A side effect requested by [`update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// Call the daemon; the runtime answers with [`Msg::ActionResult`].
    Api(ApiCall),
    /// Re-fetch jobs, stats and limits; answered with a synthesized
    /// [`Event::Snapshot`].
    Resync,
    /// List a local directory (`None` = working directory); answered with
    /// [`Msg::LocalDir`].
    ReadDir(Option<PathBuf>),
    /// Check local source paths; answered with [`Msg::PathsChecked`].
    CheckPaths(Vec<String>),
    /// Read local source files; answered with [`Msg::FilesRead`].
    ReadFiles(Vec<String>),
    /// Read the clipboard; answered with [`Msg::Clipboard`].
    ReadClipboard,
    /// Read a providers file; answered with [`Msg::ProviderFile`].
    ReadProviderFile(String),
    /// Write a providers export (never over an existing file); answered with
    /// [`Msg::ProviderFileWritten`].
    WriteProviderFile {
        /// Target path.
        path: String,
        /// TOML text.
        content: String,
    },
    /// Remember that the first-run welcome was shown by creating this
    /// (empty) marker file (best effort; never answered).
    MarkWelcomeShown(PathBuf),
    /// Show a desktop notification (best effort; never answered).
    Notify {
        /// Summary line.
        title: String,
        /// Body text.
        body: String,
    },
    /// Leave the TUI.
    Quit,
}

/// A status-line message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMsg {
    /// Severity.
    pub level: Level,
    /// Text.
    pub text: String,
}

/// Status line contents: the last message and the last error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The latest message of any level.
    pub last: Option<StatusMsg>,
    /// The latest error (kept when newer info messages arrive).
    pub last_error: Option<String>,
}

impl Status {
    /// Set an info message.
    pub fn info(&mut self, text: impl Into<String>) {
        self.set(Level::Info, text.into());
    }

    /// Set a warning (also remembered as last error).
    pub fn warn(&mut self, text: impl Into<String>) {
        self.set(Level::Warn, text.into());
    }

    /// Set an error (also remembered as last error).
    pub fn error(&mut self, text: impl Into<String>) {
        self.set(Level::Error, text.into());
    }

    /// Set a message of `level`.
    pub fn set(&mut self, level: Level, text: String) {
        if level != Level::Info {
            self.last_error = Some(text.clone());
        }
        self.last = Some(StatusMsg { level, text });
    }
}

/// Construction options for [`State`].
#[derive(Debug, Clone)]
pub struct StateOptions {
    /// Colours.
    pub theme: Theme,
    /// Key bindings.
    pub keymap: Keymap,
    /// Mouse support enabled.
    pub mouse: bool,
    /// Profile name shown in the header.
    pub profile: String,
    /// Tick interval.
    pub tick: Duration,
    /// Initial terminal size.
    pub size: (u16, u16),
    /// Show desktop notifications for finished jobs (`[notify] desktop`).
    pub desktop: bool,
    /// Show the first-run welcome when the first snapshot has no jobs.
    pub welcome: bool,
    /// Marker file created once the welcome was shown (`None` = memory only).
    pub welcome_marker: Option<PathBuf>,
}

impl Default for StateOptions {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            keymap: Keymap::default(),
            mouse: false,
            profile: "local".into(),
            tick: Duration::from_millis(250),
            size: (80, 24),
            desktop: false,
            welcome: false,
            welcome_marker: None,
        }
    }
}

/// The whole UI state.
#[derive(Debug, Clone)]
pub struct State {
    /// Live jobs by id.
    pub jobs: HashMap<JobId, Job>,
    /// Global figures.
    pub stats: GlobalStats,
    /// Global limits.
    pub limits: Limits,
    /// Last reported status of each engine.
    pub engines: Vec<EngineStatus>,
    /// Daemon connection.
    pub conn: ConnState,
    /// Countdown to the next reconnect attempt (while disconnected).
    pub retry_left: Duration,
    /// Key bindings.
    pub keymap: Keymap,
    /// Colours.
    pub theme: Theme,
    /// Mouse support enabled.
    pub mouse: bool,
    /// Profile name.
    pub profile: String,
    /// The downloads list model.
    pub list: DownloadsList,
    /// View stack; the first entry is always [`View::Downloads`].
    pub views: Vec<View>,
    /// The open modal, drawn over the top view.
    pub modal: Option<Modal>,
    /// Status line.
    pub status: Status,
    /// Calls in flight.
    pub pending: usize,
    /// Terminal size (columns, rows).
    pub size: (u16, u16),
    /// Tick interval.
    pub tick: Duration,
    /// The global speed graph of the downloads view (memory only).
    pub graph: GraphState,
    /// Search view state (the last search outlives the view).
    pub search: views::search::SearchState,
    /// Search providers as last reported by the daemon.
    pub providers: views::providers::ProvidersState,
    /// Desktop notifications for finished jobs are enabled.
    pub desktop: bool,
    /// The add view shows its advanced options (remembered while running).
    pub add_advanced: bool,
    /// The first-run welcome may still be shown (cleared after the first
    /// snapshot either way).
    pub welcome_pending: bool,
    /// Marker file recording that the welcome was shown.
    pub welcome_marker: Option<PathBuf>,
    /// Time zone used to show schedule times (the client's local zone).
    pub time_zone: jiff::tz::TimeZone,
    /// Replaces the clock when set (tests).
    pub now_override: Option<i64>,
    /// The video quality-picker preferences as last seen (palette display).
    pub video_prefs: Option<dman_proto::VideoPreferences>,
    /// Shown after the next successful add ("quality picker turned off ...").
    pub video_notice: Option<String>,
    /// Needs a redraw.
    pub dirty: bool,
}

impl State {
    /// A fresh state (connecting, no jobs).
    pub fn new(opts: StateOptions) -> Self {
        Self {
            jobs: HashMap::new(),
            stats: GlobalStats::default(),
            limits: Limits::default(),
            engines: Vec::new(),
            conn: ConnState::Connecting,
            retry_left: Duration::ZERO,
            keymap: opts.keymap,
            theme: opts.theme,
            mouse: opts.mouse,
            profile: opts.profile,
            list: DownloadsList::default(),
            views: vec![View::Downloads],
            modal: None,
            status: Status::default(),
            pending: 0,
            size: opts.size,
            tick: opts.tick,
            graph: GraphState::default(),
            search: views::search::SearchState::default(),
            providers: views::providers::ProvidersState::default(),
            desktop: opts.desktop,
            add_advanced: false,
            welcome_pending: opts.welcome,
            welcome_marker: opts.welcome_marker,
            time_zone: jiff::tz::TimeZone::system(),
            now_override: None,
            video_prefs: None,
            video_notice: None,
            dirty: true,
        }
    }

    /// Current unix time (or the test override).
    pub fn now_secs(&self) -> i64 {
        self.now_override
            .unwrap_or_else(|| jiff::Timestamp::now().as_second())
    }

    /// Whether the daemon connection is up.
    pub fn connected(&self) -> bool {
        matches!(self.conn, ConnState::Connected { .. })
    }

    /// Why actions are disabled, or `None` when connected.
    pub fn offline_reason(&self) -> Option<String> {
        match &self.conn {
            ConnState::Connected { .. } => None,
            ConnState::Connecting => Some("connecting to the daemon".into()),
            ConnState::Disconnected { .. } => Some("disconnected from the daemon".into()),
            ConnState::Failed { error } => Some(format!("cannot connect: {error}")),
        }
    }

    /// The top view.
    pub fn view(&self) -> &View {
        self.views.last().unwrap_or(&View::Downloads)
    }

    /// Key context of the top view.
    pub fn context(&self) -> Context {
        self.view().context()
    }

    /// Engines reported unavailable.
    pub fn unavailable_engines(&self) -> impl Iterator<Item = &EngineStatus> {
        self.engines.iter().filter(|e| !e.available)
    }

    /// Screen areas for the current terminal size.
    pub fn layout(&self) -> ScreenLayout {
        ScreenLayout::compute(self.size, self)
    }

    /// Rows available for list content (table body without its header row).
    pub fn list_height(&self) -> usize {
        usize::from(self.layout().body.height.saturating_sub(1))
    }

    /// Rebuild the list rows and keep the cursor visible.
    pub fn refresh_list(&mut self) {
        self.list.recompute(&self.jobs);
        self.list.ensure_visible(self.list_height());
    }

    /// Like [`State::call`] but without a status message while it runs (for
    /// calls whose answer is shown elsewhere, e.g. in a picker).
    pub fn call_quiet(&mut self, call: ApiCall) -> Vec<Cmd> {
        if let Some(reason) = self.offline_reason() {
            self.status
                .error(format!("{} is disabled: {reason}", call.verb()));
            return Vec::new();
        }
        self.pending += 1;
        vec![Cmd::Api(call)]
    }

    /// Request a daemon call, or explain why it is disabled.
    pub fn call(&mut self, call: ApiCall) -> Vec<Cmd> {
        if let Some(reason) = self.offline_reason() {
            self.status
                .error(format!("{} is disabled: {reason}", call.verb()));
            return Vec::new();
        }
        self.status.info(format!("{}…", call.progress_label()));
        self.pending += 1;
        vec![Cmd::Api(call)]
    }
}

/// Apply `msg` to `state`; returns the side effects to perform.
pub fn update(state: &mut State, msg: Msg) -> Vec<Cmd> {
    let cmds = update_inner(state, msg);
    views::add::sync_engine(state);
    cmds
}

fn update_inner(state: &mut State, msg: Msg) -> Vec<Cmd> {
    state.dirty = true;
    match msg {
        Msg::Key(key) => on_key(state, key),
        Msg::Mouse(m) => views::on_mouse(state, m),
        Msg::Paste(text) => on_paste(state, &text),
        Msg::Resize(w, h) => {
            state.size = (w, h);
            let height = state.list_height();
            state.list.ensure_visible(height);
            Vec::new()
        }
        Msg::Event(e) => apply_event(state, e),
        Msg::Conn(c) => on_conn(state, c),
        Msg::ActionResult { call, result } => on_result(state, &call, &result),
        Msg::LocalDir(r) => {
            views::add::on_local_dir(state, r);
            Vec::new()
        }
        Msg::PathsChecked(r) => {
            views::add::on_paths_checked(state, r);
            Vec::new()
        }
        Msg::FilesRead(files) => {
            views::add::on_files_read(state, &files);
            Vec::new()
        }
        Msg::Clipboard(r) => views::add::on_clipboard(state, r),
        Msg::ProviderFile(r) => views::providers::on_file_read(state, r),
        Msg::ProviderFileWritten(r) => {
            views::providers::on_file_written(state, r);
            Vec::new()
        }
        Msg::Lagged => {
            state.status.warn("Missed some live updates; reloading");
            let mut cmds = vec![Cmd::Resync];
            cmds.extend(views::search::on_lagged(state));
            cmds.push(Cmd::Api(ApiCall::EngineStatus));
            cmds.push(Cmd::Api(ApiCall::VideoPrefsGet(PrefsIntent::Cache)));
            cmds
        }
        Msg::Tick => on_tick(state),
        Msg::Terminate => vec![Cmd::Quit],
    }
}

fn on_tick(state: &mut State) -> Vec<Cmd> {
    let mut cmds = views::detail::on_tick(state);
    cmds.extend(graph::on_tick(state));
    let animating = views::add::on_tick(state) | views::search::on_tick(state);
    if matches!(state.conn, ConnState::Disconnected { .. }) && !state.retry_left.is_zero() {
        state.retry_left = state.retry_left.saturating_sub(state.tick);
    } else if cmds.is_empty() && !animating {
        state.dirty = false;
    }
    cmds
}

fn on_key(state: &mut State, key: KeyEvent) -> Vec<Cmd> {
    if key.kind == KeyEventKind::Release {
        state.dirty = false;
        return Vec::new();
    }
    if let Some(mut modal) = state.modal.take() {
        let outcome = modal.handle_key(key, state);
        state.modal = Some(modal);
        return on_modal_outcome(state, outcome);
    }
    if let Some(cmds) = views::handle_raw_key(state, key) {
        return cmds;
    }
    match state.keymap.resolve_event(state.context(), key) {
        Some(action) => apply_action(state, action),
        None => {
            state.dirty = false;
            Vec::new()
        }
    }
}

fn on_paste(state: &mut State, text: &str) -> Vec<Cmd> {
    if let Some(mut modal) = state.modal.take() {
        modal.paste(text, state);
        state.modal = Some(modal);
        return Vec::new();
    }
    views::on_paste(state, text)
}

/// Act on what a modal decided.
pub fn on_modal_outcome(state: &mut State, outcome: ModalOutcome) -> Vec<Cmd> {
    match outcome {
        ModalOutcome::Stay => Vec::new(),
        ModalOutcome::Close => {
            state.modal = None;
            Vec::new()
        }
        ModalOutcome::Run(action) => {
            state.modal = None;
            apply_action(state, action)
        }
        ModalOutcome::Api(call) => {
            state.modal = None;
            state.call(call)
        }
        ModalOutcome::LeaveView => {
            state.modal = None;
            if state.views.len() > 1 {
                state.views.pop();
            }
            Vec::new()
        }
        ModalOutcome::Replace(m) => {
            state.modal = Some(m);
            Vec::new()
        }
        ModalOutcome::Submit(kind, value) => {
            state.modal = None;
            on_submit(state, kind, &value)
        }
    }
}

fn on_submit(state: &mut State, kind: PromptKind, value: &str) -> Vec<Cmd> {
    match kind {
        PromptKind::JobOption(field) => views::detail::submit_option(state, field, value),
        PromptKind::Rename { id, file_index } => {
            views::rename::submit(state, id, file_index, value)
        }
        PromptKind::SearchSeeders
        | PromptKind::SearchSize
        | PromptKind::SearchSource
        | PromptKind::SearchTitle => views::search::submit(state, kind, value),
        PromptKind::ProvidersImport | PromptKind::ProvidersExport => {
            views::providers::submit(state, kind, value)
        }
        PromptKind::VideoQuality => views::add::quality::submit_default(state, false, value),
        PromptKind::VideoAudio => views::add::quality::submit_default(state, true, value),
        PromptKind::MaxConcurrent => match value.parse::<u32>() {
            Ok(n) if (1..=1000).contains(&n) => {
                state.call(ApiCall::SetLimits(dman_proto::method::LimitsUpdate {
                    max_concurrent: Some(n),
                    ..Default::default()
                }))
            }
            _ => {
                state.status.error(format!(
                    "Max concurrent must be a number from 1 to 1000, got '{value}'"
                ));
                Vec::new()
            }
        },
    }
}

/// Apply an action in the context of the top view.
pub fn apply_action(state: &mut State, action: Action) -> Vec<Cmd> {
    if action.needs_connection()
        && let Some(reason) = state.offline_reason()
    {
        state
            .status
            .error(format!("{} is disabled: {reason}", action.title()));
        return Vec::new();
    }
    // Add-view actions run from the palette elsewhere open the view first.
    let mut cmds = Vec::new();
    if action.home_context() == Context::Add && !matches!(state.view(), View::Add(_)) {
        cmds = views::add::push(state, views::add::AddState::default());
    }
    cmds.extend(apply_action_inner(state, action));
    cmds
}

fn apply_action_inner(state: &mut State, action: Action) -> Vec<Cmd> {
    match action {
        Action::Quit => vec![Cmd::Quit],
        Action::Palette => {
            state.modal = Some(Modal::Palette(PaletteState::default()));
            Vec::new()
        }
        Action::Help => {
            if matches!(state.view(), View::Help(_)) {
                state.views.pop();
            } else {
                state
                    .views
                    .push(View::Help(views::help::HelpState::default()));
            }
            Vec::new()
        }
        Action::Back => {
            if let View::Add(a) = state.view()
                && a.overlay.is_none()
                && a.is_dirty()
            {
                state.modal = Some(Modal::Confirm(views::add::discard_confirm()));
                return Vec::new();
            }
            if state.views.len() > 1 {
                state.views.pop();
            }
            Vec::new()
        }
        Action::Add => views::add::push(state, views::add::AddState::default()),
        Action::Shutdown => {
            state.modal = Some(Modal::Confirm(views::downloads::shutdown_confirm()));
            Vec::new()
        }
        Action::VideoQualityToggle => views::add::quality::toggle_ask(state),
        Action::VideoDefaultQuality => views::add::quality::edit_default(state, false),
        Action::VideoDefaultAudio => views::add::quality::edit_default(state, true),
        Action::Search => views::search::open(state),
        Action::SearchProviders => views::providers::open(state),
        Action::Filter if !matches!(state.view(), View::Search) => {
            state.views.truncate(1);
            state.modal = Some(Modal::Filter(FilterState::new(&state.list.filter)));
            Vec::new()
        }
        _ => {
            let handled = views::apply_action(state, action);
            handled.unwrap_or_else(|| {
                state
                    .status
                    .info(format!("{} is not available in this view", action.title()));
                Vec::new()
            })
        }
    }
}

fn apply_event(state: &mut State, event: Event) -> Vec<Cmd> {
    match event {
        Event::Snapshot {
            jobs,
            stats,
            limits,
        } => {
            state.jobs = jobs.into_iter().map(|j| (j.id, j)).collect();
            state.stats = stats;
            state.limits = limits;
            state.refresh_list();
            views::detail::close_if_gone(state);
            return crate::widgets::welcome::on_snapshot(state);
        }
        Event::JobsUpdated { jobs } => {
            for j in jobs {
                state.jobs.insert(j.id, j);
            }
            state.refresh_list();
        }
        Event::JobsRemoved { ids } => {
            for id in ids {
                state.jobs.remove(&id);
            }
            state.refresh_list();
            views::detail::close_if_gone(state);
        }
        Event::Stats { stats } => {
            if state.graph.visible {
                state
                    .graph
                    .push_live(stats.download_speed, stats.upload_speed);
            }
            state.stats = stats;
        }
        Event::Limits { limits } => state.limits = limits,
        Event::Message { level, text } => state.status.set(level, text),
        Event::Engine { status } => {
            state.engines.retain(|e| e.engine != status.engine);
            state.engines.push(status);
        }
        Event::CleanupPending {
            id,
            name,
            count,
            bytes,
        } => {
            // List the files first; the answer opens the confirmation unless
            // another dialog is open by then.
            if state.connected() && state.modal.is_none() {
                return state.call_quiet(ApiCall::Cleanup { id, dry_run: true });
            }
            state.status.info(format!(
                "{name}: {count} unselected partial files ({}) can be deleted (palette: cleanup)",
                crate::format::size(bytes)
            ));
        }
        Event::SearchUpdate {
            search_id,
            provider,
            status,
            results,
        } => views::search::on_update(state, search_id, &provider, status, results),
        Event::SearchFinished { search_id } => views::search::on_finished(state, search_id),
        Event::JobFinished {
            name,
            path,
            outcome,
            ..
        } => {
            let (title, body, level) = match &outcome {
                JobOutcome::Completed => ("Download complete", name.clone(), Level::Info),
                JobOutcome::SeedCompleted => ("Seeding finished", name.clone(), Level::Info),
                JobOutcome::Failed { error } => {
                    ("Download failed", format!("{name}: {error}"), Level::Error)
                }
            };
            state.status.set(level, format!("{title}: {body}"));
            if state.desktop && state.connected() {
                let body = if level == Level::Info && path != name {
                    format!("{name}\n{path}")
                } else {
                    body
                };
                return vec![Cmd::Notify {
                    title: title.to_owned(),
                    body,
                }];
            }
        }
    }
    Vec::new()
}

fn on_conn(state: &mut State, conn: ConnState) -> Vec<Cmd> {
    let mut cmds = Vec::new();
    match &conn {
        ConnState::Connected { daemon_version } => {
            state
                .status
                .info(format!("Connected to daemon {daemon_version}"));
            // The daemon only pushes engine changes; ask for the current
            // status after every (re)connect (not counted as pending).
            cmds.push(Cmd::Api(ApiCall::EngineStatus));
            cmds.push(Cmd::Api(ApiCall::VideoPrefsGet(PrefsIntent::Cache)));
        }
        ConnState::Disconnected { retry_in, .. } => {
            state.retry_left = *retry_in;
            state.pending = 0;
            views::detail::on_disconnect(state);
            graph::on_disconnect(state);
            views::add::on_disconnect(state);
            views::search::on_disconnect(state);
        }
        ConnState::Failed { error } => {
            state.status.error(format!("Cannot connect: {error}"));
            state.pending = 0;
        }
        ConnState::Connecting => {}
    }
    state.conn = conn;
    cmds
}

fn on_result(state: &mut State, call: &ApiCall, result: &Result<ApiReply, String>) -> Vec<Cmd> {
    if let ApiCall::Detail(id) = call {
        views::detail::on_detail_result(state, *id, result);
        return Vec::new();
    }
    if matches!(call, ApiCall::StatsHistory) {
        graph::on_result(state, result);
        return Vec::new();
    }
    if matches!(call, ApiCall::VideoPrefsGet(PrefsIntent::Cache)) {
        views::add::quality::on_prefs(state, PrefsIntent::Cache, result);
        return Vec::new();
    }
    if matches!(call, ApiCall::EngineStatus) {
        // Background refresh: never counted, silent on failure.
        if let Ok(ApiReply::Engines(list)) = result {
            for status in list {
                state.engines.retain(|e| e.engine != status.engine);
                state.engines.push(status.clone());
            }
        }
        return Vec::new();
    }
    state.pending = state.pending.saturating_sub(1);
    if let ApiCall::PreviewMagnet { uri } = call {
        views::add::on_magnet_preview(state, uri, result);
        return Vec::new();
    }
    if let ApiCall::VideoProbe(params) = call {
        return views::add::on_video_probe(state, params, result);
    }
    if let ApiCall::VideoPrefsGet(intent) = call {
        return views::add::quality::on_prefs(state, *intent, result);
    }
    if let (ApiCall::VideoPrefsSet(_), Ok(ApiReply::VideoPreferences(p))) = (call, result) {
        state.video_prefs = Some((**p).clone());
    }
    if matches!(call, ApiCall::FsList(_)) {
        views::add::on_fs_listing(state, result);
        return Vec::new();
    }
    if let Some(cmds) = views::search::on_result(state, call, result)
        .or_else(|| views::providers::on_result(state, call, result))
    {
        return cmds;
    }
    if let ApiCall::Cleanup { id, dry_run: true } = call {
        return match result {
            Ok(ApiReply::Cleanup(r)) if r.files.is_empty() => {
                state.status.info("No unselected partial files to delete");
                Vec::new()
            }
            Ok(ApiReply::Cleanup(_)) if state.modal.is_some() => {
                state
                    .status
                    .info("Unselected partial files can be deleted (palette: cleanup)");
                Vec::new()
            }
            Ok(ApiReply::Cleanup(r)) => {
                let name = state
                    .jobs
                    .get(id)
                    .map(|j| j.name.clone())
                    .unwrap_or_default();
                let files: Vec<(String, u64)> =
                    r.files.iter().map(|f| (f.path.clone(), f.bytes)).collect();
                state.modal = Some(Modal::Confirm(views::downloads::cleanup_confirm(
                    *id, &name, &files,
                )));
                Vec::new()
            }
            Err(e) => {
                state.status.error(format!("Cleanup failed: {e}"));
                Vec::new()
            }
            Ok(_) => Vec::new(),
        };
    }
    if let Ok(ApiReply::Limits(l)) = result {
        state.limits = l.clone();
    }
    let (outcome, text) = summarize(call, result);
    match outcome {
        Outcome::Ok => state.status.info(text),
        Outcome::Partial => state.status.warn(text),
        Outcome::Failed => state.status.error(text),
    }
    if let (ApiCall::Remove { ids, delete_files }, Ok(ApiReply::Batch(b))) = (call, result)
        && b.failed.is_empty()
    {
        let text = views::downloads::removed_message(state, ids, *delete_files);
        state.status.info(text);
    }
    if let ApiCall::Add(params) = call {
        views::add::on_add_result(state, params, result);
    }
    if *call == ApiCall::Shutdown && result.is_ok() {
        return vec![Cmd::Quit];
    }
    if let ApiCall::SetOptions { id, .. } | ApiCall::Rename { id, .. } = call {
        // Success or refusal, re-read so the view shows what is really stored.
        return views::detail::refetch(state, *id);
    }
    Vec::new()
}
