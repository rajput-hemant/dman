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

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyEvent, KeyEventKind, MouseEvent};
use dman_client::ConnState;
use dman_proto::method::Outcome as JobOutcome;
use dman_proto::method::{EngineStatus, Event, Level};
use dman_proto::{GlobalStats, Job, JobId, JobState, Limits};
use ratatui::layout::Rect;

use crate::action::Action;
use crate::anim::Anim;
use crate::api::{ApiCall, ApiReply, Outcome, PrefsIntent, summarize};
use crate::keymap::{Context, Keymap};
use crate::layout::{Hit, ScreenLayout};
use crate::list::DownloadsList;
use crate::theme::Theme;
use crate::views::add::browser::LocalListing;
use crate::views::add::preview::Loaded;
use crate::views::add::source::PathStatus;
use crate::views::{self, View};
use crate::widgets::graph::{self, GraphState};
use crate::widgets::palette::PaletteState;
use crate::widgets::prompt::PromptKind;
use crate::widgets::toast::{ToastAction, Toasts, UNDO_TTL};
use crate::widgets::{FilterState, Modal, ModalOutcome};

mod actions;
mod events;

pub use actions::{apply_action, on_modal_outcome, toggle_mouse};
use actions::{on_key, on_paste};
use events::{apply_event, on_conn, on_result};

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
    /// Animation frame; the time since the previous one. Only sent while
    /// [`State::frame_interval`] asks for frames.
    Frame(Duration),
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
    /// Turn mouse capture on or off in the terminal.
    SetMouseCapture(bool),
    /// Save `[ui] mouse` in config.toml (best effort; never answered).
    SaveMouse(bool),
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

/// Messages: the last message, the last error and the toasts showing them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The latest message of any level.
    pub last: Option<StatusMsg>,
    /// The latest error (kept when newer info messages arrive).
    pub last_error: Option<String>,
    /// Toasts on screen.
    pub toasts: Toasts,
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

    /// Set a message of `level` (shown as a toast).
    pub fn set(&mut self, level: Level, text: String) {
        self.toasts.push(level, text.clone());
        self.remember(level, text);
    }

    /// Remember a message without a toast (progress of a call in flight,
    /// which the footer spinner already shows).
    pub fn progress(&mut self, text: impl Into<String>) {
        self.remember(Level::Info, text.into());
    }

    fn remember(&mut self, level: Level, text: String) {
        if level != Level::Info {
            self.last_error = Some(text.clone());
        }
        self.last = Some(StatusMsg { level, text });
    }
}

/// How many lines a list row takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Density {
    /// Two lines on terminals with at least 30 rows, else one.
    #[default]
    Auto,
    /// Always one line.
    Compact,
    /// Always two lines.
    Comfortable,
}

/// Look-and-feel preferences from `[ui]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPrefs {
    /// Spinners, eased bars, flashes.
    pub animations: bool,
    /// Preview pane next to lists on wide terminals.
    pub preview: bool,
    /// Row height.
    pub density: Density,
    /// Save mouse toggles to config.toml.
    pub persist_mouse: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            animations: false,
            preview: true,
            density: Density::Auto,
            persist_mouse: false,
        }
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
    /// Look-and-feel preferences.
    pub ui: UiPrefs,
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
            ui: UiPrefs::default(),
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
    /// Look-and-feel preferences.
    pub ui: UiPrefs,
    /// Animation clock, eased bars and row flashes.
    pub anim: Anim,
    /// Recent download (or, while seeding, upload) speeds per job, oldest
    /// first, sampled with every global stats event (per-row sparklines).
    pub job_speeds: HashMap<JobId, Vec<u64>>,
    /// Commands recently run from the palette, newest first (memory only).
    pub recent_actions: Vec<Action>,
    /// The "which key" popup after a prefix key (e.g. sort).
    pub whichkey: Option<crate::widgets::whichkey::WhichKey>,
    /// Click targets of the last frame (filled while rendering).
    pub hits: RefCell<Vec<(Rect, Hit)>>,
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
            // Tests get a fixed clock (relative times in snapshots).
            now_override: cfg!(test).then_some(TEST_NOW),
            video_prefs: None,
            video_notice: None,
            ui: opts.ui,
            anim: Anim::default(),
            job_speeds: HashMap::new(),
            recent_actions: Vec::new(),
            whichkey: None,
            hits: RefCell::new(Vec::new()),
            dirty: true,
        }
    }

    /// Record a click target while drawing.
    pub fn hit(&self, area: Rect, hit: Hit) {
        if area.width > 0 && area.height > 0 {
            self.hits.borrow_mut().push((area, hit));
        }
    }

    /// The topmost click target at a screen position, with its area.
    pub fn hit_at(&self, col: u16, row: u16) -> Option<(Rect, Hit)> {
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
            .cloned()
    }

    /// Whether list rows take two lines.
    pub fn tall_rows(&self) -> bool {
        match self.ui.density {
            Density::Auto => self.size.1 >= 30,
            Density::Compact => false,
            Density::Comfortable => true,
        }
    }

    /// Keep the list cursor visible (with scroll-off) in the current layout.
    pub fn fit_list(&mut self) {
        let lines = self.list_height();
        let tall = self.tall_rows();
        self.list.ensure_visible_lines(lines, tall, SCROLLOFF);
    }

    /// The animation time when animations are on (`None` = draw still).
    pub fn anim_ms(&self) -> Option<u64> {
        self.ui.animations.then_some(self.anim.ms)
    }

    /// The progress bar value to draw for `job` (eased when animating).
    pub fn shown_progress(&self, job: &Job) -> f64 {
        if self.ui.animations {
            self.anim.progress(job)
        } else if job.total_bytes == 0 {
            0.0
        } else {
            job.progress().clamp(0.0, 1.0)
        }
    }

    /// Something in flight that a spinner shows.
    pub fn spinning(&self) -> bool {
        self.pending > 0
            || matches!(self.conn, ConnState::Connecting)
            || (matches!(self.view(), View::Downloads)
                && self
                    .jobs
                    .values()
                    .any(|j| j.state == JobState::Active && j.total_bytes == 0))
    }

    /// How soon the next animation frame is wanted (`None` = no animation).
    pub fn frame_interval(&self) -> Option<Duration> {
        if !self.ui.animations {
            return None;
        }
        if self.anim.busy(&self.jobs) {
            Some(Duration::from_millis(33))
        } else if self.spinning() {
            Some(Duration::from_millis(100))
        } else {
            None
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

    /// Lines available for list content (inside the list's border).
    pub fn list_height(&self) -> usize {
        usize::from(self.layout().body.height.saturating_sub(2))
    }

    /// Jobs that fit on one page of the list.
    pub fn list_page(&self) -> usize {
        let lines = self.list_height();
        if self.tall_rows() { lines / 2 } else { lines }.max(1)
    }

    /// Rebuild the list rows and keep the cursor visible.
    pub fn refresh_list(&mut self) {
        self.list.recompute(&self.jobs);
        self.fit_list();
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
        self.status.progress(format!("{}…", call.progress_label()));
        self.pending += 1;
        vec![Cmd::Api(call)]
    }
}

/// Apply `msg` to `state`; returns the side effects to perform.
pub fn update(state: &mut State, msg: Msg) -> Vec<Cmd> {
    let mark = state.status.toasts.mark();
    // What the palette can run depends on the connection and the jobs.
    let refresh_palette = matches!(
        msg,
        Msg::Conn(_) | Msg::Event(Event::Snapshot { .. } | Event::JobsRemoved { .. })
    );
    let cmds = update_inner(state, msg);
    if refresh_palette && matches!(state.modal, Some(Modal::Palette(_))) {
        let disabled = crate::widgets::palette::unavailable(state);
        if let Some(Modal::Palette(p)) = &mut state.modal {
            p.disabled = disabled;
        }
    }
    // One message produces at most one toast (the most important one).
    state.status.toasts.collapse_since(mark);
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
            state.fit_list();
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
        Msg::Frame(dt) => {
            state.anim.advance(dt, &state.jobs);
            Vec::new()
        }
        Msg::Terminate => vec![Cmd::Quit],
    }
}

fn on_tick(state: &mut State) -> Vec<Cmd> {
    let mut cmds = views::detail::on_tick(state);
    cmds.extend(graph::on_tick(state));
    let expired = state.status.toasts.tick(state.tick);
    let whichkey = crate::widgets::whichkey::on_tick(state);
    let animating = views::add::on_tick(state) | views::search::on_tick(state) | expired | whichkey;
    if matches!(state.conn, ConnState::Disconnected { .. }) && !state.retry_left.is_zero() {
        state.retry_left = state.retry_left.saturating_sub(state.tick);
    } else if cmds.is_empty() && !animating {
        state.dirty = false;
    }
    cmds
}

/// The clock of unit tests: a day after the fixtures' `added_at`.
pub const TEST_NOW: i64 = 1_700_086_400;

/// Lines of context kept above and below the list cursor.
pub const SCROLLOFF: usize = 3;

/// Samples kept per job for the row sparklines.
pub const JOB_SPEED_SAMPLES: usize = 60;
