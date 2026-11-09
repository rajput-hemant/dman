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
use dman_proto::method::{EngineStatus, Event, Level};
use dman_proto::{GlobalStats, Job, JobId, Limits};

use crate::action::Action;
use crate::api::{ApiCall, ApiReply, Outcome, summarize};
use crate::keymap::{Context, Keymap};
use crate::layout::ScreenLayout;
use crate::list::DownloadsList;
use crate::theme::Theme;
use crate::views::add::browser::LocalListing;
use crate::views::add::preview::Loaded;
use crate::views::add::source::PathStatus;
use crate::views::{self, View};
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
            dirty: true,
        }
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
        Msg::Event(e) => {
            apply_event(state, e);
            Vec::new()
        }
        Msg::Conn(c) => {
            on_conn(state, c);
            Vec::new()
        }
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
        Msg::Lagged => {
            state.status.warn("Missed some live updates; reloading");
            vec![Cmd::Resync]
        }
        Msg::Tick => on_tick(state),
        Msg::Terminate => vec![Cmd::Quit],
    }
}

fn on_tick(state: &mut State) -> Vec<Cmd> {
    let cmds = views::detail::on_tick(state);
    if matches!(state.conn, ConnState::Disconnected { .. }) && !state.retry_left.is_zero() {
        state.retry_left = state.retry_left.saturating_sub(state.tick);
    } else if cmds.is_empty() {
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
    if action.home_context() == Context::Add && !matches!(state.view(), View::Add(_)) {
        state.views.push(View::Add(Box::default()));
    }
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
            if state.views.len() > 1 {
                state.views.pop();
            }
            Vec::new()
        }
        Action::Add => {
            state.views.push(View::Add(Box::default()));
            Vec::new()
        }
        Action::Shutdown => {
            state.modal = Some(Modal::Confirm(views::downloads::shutdown_confirm()));
            Vec::new()
        }
        Action::Filter => {
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

fn apply_event(state: &mut State, event: Event) {
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
        Event::Stats { stats } => state.stats = stats,
        Event::Limits { limits } => state.limits = limits,
        Event::Message { level, text } => state.status.set(level, text),
        Event::Engine { status } => {
            state.engines.retain(|e| e.engine != status.engine);
            state.engines.push(status);
        }
    }
}

fn on_conn(state: &mut State, conn: ConnState) {
    match &conn {
        ConnState::Connected { daemon_version } => {
            state
                .status
                .info(format!("Connected to daemon {daemon_version}"));
        }
        ConnState::Disconnected { retry_in, .. } => {
            state.retry_left = *retry_in;
            state.pending = 0;
            views::detail::on_disconnect(state);
        }
        ConnState::Failed { error } => {
            state.status.error(format!("Cannot connect: {error}"));
            state.pending = 0;
        }
        ConnState::Connecting => {}
    }
    state.conn = conn;
}

fn on_result(state: &mut State, call: &ApiCall, result: &Result<ApiReply, String>) -> Vec<Cmd> {
    if let ApiCall::Detail(id) = call {
        views::detail::on_detail_result(state, *id, result);
        return Vec::new();
    }
    state.pending = state.pending.saturating_sub(1);
    if matches!(call, ApiCall::FsList(_)) {
        views::add::on_fs_listing(state, result);
        return Vec::new();
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
    if matches!(call, ApiCall::Add(_)) {
        views::add::on_add_result(state, result);
    }
    if *call == ApiCall::Shutdown && result.is_ok() {
        return vec![Cmd::Quit];
    }
    if let ApiCall::SetOptions { id, .. } = call {
        // Success or refusal, re-read so the view shows what is really stored.
        return views::detail::refetch(state, *id);
    }
    Vec::new()
}
