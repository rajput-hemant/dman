//! Add view: a multi-line source editor with live source detection, a per-add
//! options form, a local file browser, a daemon directory picker, clipboard
//! paste and a preview before anything is sent.
//!
//! Contract with the rest of the app: the view lives in [`View::Add`]; keys
//! reach [`handle_raw_key`] before the keymap (it returns `None` for keys that
//! should resolve to an [`Action`], e.g. Esc → `Back`, Ctrl-s → `AddSubmit`);
//! confirming the preview returns `state.call(ApiCall::Add(Box::new(params)))`
//! so the connection gate and status line work as for every other action.
//! Everything with a side effect is a [`Cmd`] (`ReadDir`, `CheckPaths`,
//! `ReadFiles`, `ReadClipboard`, `Api(FsList)`) whose answer comes back as a
//! [`crate::app::Msg`] routed to the `on_*` functions below.
//!
//! Layout of the module: [`source`] (detection), [`options`] (form and
//! validation), [`browser`] (local file browser, daemon directory picker),
//! [`preview`] (plan, request, preview modal with the magnet file list).
//!
//! [`View::Add`]: crate::views::View::Add

pub mod browser;
pub mod options;
pub mod preview;
pub mod source;
pub mod video;

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dman_proto::method::FsListParams;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use self::browser::{DirOutcome, DirPicker, FileBrowser, FileOutcome, LocalListing};
use self::options::{Field, OptionsForm};
use self::preview::{Fetch, Loaded, MagnetFiles, Planned, Preview, PreviewOutcome};
use self::source::{Kind, PathStatus, Severity, Token, VideoCtx};
use self::video::ProbeFetch;
use crate::action::Action;
use crate::api::{ApiCall, ApiReply};
use crate::app::{Cmd, State};
use crate::format::{plural, truncate};
use crate::theme::Theme;
use crate::views::View;
use crate::widgets::confirm::ConfirmState;
use crate::widgets::editor::Editor;
use crate::widgets::footer::{Hint, hint, own};
use dman_proto::{AddOutcome, AddParams, AddSource};

/// Actions shown in the footer of the form.
pub const FOOTER: &[Action] = &[
    Action::AddSubmit,
    Action::AddNextField,
    Action::AddPasteClipboard,
    Action::AddBrowseFiles,
    Action::AddToggleAdvanced,
    Action::Back,
];

/// The options shown before "Advanced options" is expanded.
pub const COMMON: [Field; 3] = [Field::Dir, Field::Paused, Field::DownLimit];

/// Which part of the form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The sources editor.
    Sources,
    /// An options field.
    Field(Field),
    /// The "Advanced options" row (Enter / Space shows or hides them).
    Advanced,
}

/// A popup over the form.
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// Local file browser.
    Files(FileBrowser),
    /// Daemon directory picker.
    Dirs(DirPicker),
    /// Preview before confirming.
    Preview(Box<Preview>),
}

/// Add view state.
#[derive(Debug, Clone, PartialEq)]
pub struct AddState {
    /// One source per line.
    pub sources: Editor,
    /// Keyboard focus.
    pub focus: Focus,
    /// Options form.
    pub form: OptionsForm,
    /// Popup, if any.
    pub overlay: Option<Overlay>,
    /// Results of local file checks, by source text.
    pub paths: HashMap<String, PathStatus>,
    /// Plan waiting for its local files to be read.
    pub pending_plan: Option<Vec<Planned>>,
    /// Source texts behind each source of the request in flight.
    pub submitting: Option<Vec<Vec<String>>>,
    /// The video engine and the per-URL "try as video" choices.
    pub video: VideoCtx,
    /// The advanced options are shown (otherwise only [`COMMON`] ones).
    pub advanced: bool,
}

impl Default for AddState {
    fn default() -> Self {
        Self {
            sources: Editor::new(false),
            focus: Focus::Sources,
            form: OptionsForm::default(),
            overlay: None,
            paths: HashMap::new(),
            pending_plan: None,
            submitting: None,
            video: VideoCtx::default(),
            advanced: false,
        }
    }
}

impl AddState {
    /// Start with pasted text.
    pub fn with_text(text: &str) -> Self {
        Self {
            sources: Editor::from_text(text, false),
            ..Self::default()
        }
    }

    /// Whether anything was typed or toggled (leaving then asks first).
    pub fn is_dirty(&self) -> bool {
        !self.sources.is_empty() || !self.form.is_pristine()
    }

    /// Every source on every line.
    pub fn all_tokens(&self) -> Vec<Token> {
        self.sources
            .lines()
            .iter()
            .flat_map(|l| source::tokens_with(l, &self.video))
            .collect()
    }

    /// Whether any source goes to the video engine.
    pub fn has_video(&self) -> bool {
        self.all_tokens().iter().any(|t| t.kind.is_video())
    }

    /// The options always shown: folder, start paused, download limit, and
    /// the video quality when a line goes to the video engine.
    pub fn common_fields(&self) -> Vec<Field> {
        let mut v = COMMON.to_vec();
        if self.has_video() {
            v.push(Field::VQuality);
        }
        v
    }

    /// The options behind "Advanced options" (video section only with a video).
    pub fn advanced_fields(&self) -> Vec<Field> {
        let all: &[Field] = if self.has_video() {
            &Field::FULL
        } else {
            &Field::ALL
        };
        let common = self.common_fields();
        all.iter()
            .copied()
            .filter(|f| !common.contains(f))
            .collect()
    }

    /// The fields shown: the common ones, then the advanced ones if expanded.
    pub fn visible_fields(&self) -> Vec<Field> {
        let mut v = self.common_fields();
        if self.advanced {
            v.extend(self.advanced_fields());
        }
        v
    }

    /// Advanced fields that hold a value (shown on the collapsed row).
    pub fn advanced_set(&self) -> Vec<Field> {
        self.advanced_fields()
            .into_iter()
            .filter(|f| {
                self.form.checked(*f) || self.form.editor(*f).is_some_and(|e| !e.is_empty())
            })
            .collect()
    }

    /// Tab order: sources, common fields, the advanced row, advanced fields.
    fn focus_order(&self) -> Vec<Focus> {
        let mut v = vec![Focus::Sources];
        v.extend(self.common_fields().into_iter().map(Focus::Field));
        v.push(Focus::Advanced);
        if self.advanced {
            v.extend(self.advanced_fields().into_iter().map(Focus::Field));
        }
        v
    }

    /// Show or hide the advanced options.
    pub fn set_advanced(&mut self, on: bool) {
        self.advanced = on;
        if let Focus::Field(f) = self.focus
            && !self.visible_fields().contains(&f)
        {
            self.focus = Focus::Advanced;
        }
    }

    /// What Enter would add: `Ok(count)` or why nothing can be added yet.
    pub fn readiness(&self) -> Result<usize, String> {
        let tokens = self.all_tokens();
        if tokens.is_empty() {
            return Err(String::new());
        }
        if let Some(t) = tokens.iter().find(|t| !t.kind.is_supported()) {
            return Err(format!(
                "Can't add `{}` ({}) — fix or remove that line",
                truncate(&t.text, 30),
                t.kind.label()
            ));
        }
        if let Some((f, msg)) = self.errors().first() {
            return Err(format!("Fix {}: {msg}", f.label()));
        }
        let parsed = self.form.parse();
        Ok(preview::plan(&tokens, parsed.mirror).len())
    }

    /// A hint about the video engine (never blocks anything), when the
    /// sources include something that looks like a video.
    pub fn engine_hint(&self) -> Option<(String, Severity)> {
        if !self.video.known {
            return None;
        }
        let tokens = self.all_tokens();
        if !self.video.available {
            return tokens.iter().any(|t| t.kind == Kind::VideoUnavailable).then(|| {
                let why = self.video.reason.as_deref().unwrap_or("not found");
                (
                    format!(
                        "yt-dlp is unavailable ({why}): video links are added as plain downloads of the page."
                    ),
                    Severity::Warn,
                )
            });
        }
        if !self.video.ffmpeg && tokens.iter().any(|t| t.kind.is_video()) {
            let why = self.video.reason.as_deref().unwrap_or("ffmpeg not found");
            return Some((
                format!(
                    "ffmpeg missing ({why}): no merging, conversion or embedding; separate streams need consent."
                ),
                Severity::Warn,
            ));
        }
        None
    }

    /// Toggle "try as video" for the http(s) URLs of the cursor line.
    fn toggle_video_line(&mut self) -> Result<(), String> {
        if !self.video.known || !self.video.available {
            return Err(match &self.video.reason {
                Some(r) => format!("yt-dlp is unavailable: {r}"),
                None => "yt-dlp is not available on the daemon host".into(),
            });
        }
        let (row, _) = self.sources.cursor();
        let line = self.sources.lines().get(row).cloned().unwrap_or_default();
        let toks = source::tokens_with(&line, &self.video);
        let urls: Vec<&Token> = toks
            .iter()
            .filter(|t| t.kind.is_video() || matches!(t.kind, Kind::Http | Kind::Https))
            .collect();
        if urls.is_empty() {
            return Err("Put the cursor on an http(s) URL to try it as video".into());
        }
        for t in urls {
            self.video.forced.insert(t.text.clone(), !t.kind.is_video());
        }
        Ok(())
    }

    /// Form and plan problems, in form order.
    pub fn errors(&self) -> Vec<(Field, String)> {
        let parsed = self.form.parse();
        let mut e = parsed.errors.clone();
        let tokens = self.all_tokens();
        if tokens.iter().any(|t| t.kind.is_video()) {
            e.extend(parsed.video_errors.clone());
        }
        if !tokens.is_empty() {
            e.extend(preview::plan_errors(
                &preview::plan(&tokens, parsed.mirror),
                &parsed,
            ));
        }
        e.sort_by_key(|(f, _)| f.index());
        e
    }

    /// Ask for checks of file paths not yet known; forget stale answers.
    pub fn check_cmds(&mut self) -> Vec<Cmd> {
        let wanted: HashSet<String> = self
            .all_tokens()
            .into_iter()
            .filter(|t| t.kind.is_file())
            .map(|t| t.text)
            .collect();
        self.paths
            .retain(|k, v| wanted.contains(k) && *v != PathStatus::Missing);
        let mut new: Vec<String> = wanted
            .into_iter()
            .filter(|t| !self.paths.contains_key(t))
            .collect();
        if new.is_empty() {
            return Vec::new();
        }
        new.sort();
        for t in &new {
            self.paths.insert(t.clone(), PathStatus::Checking);
        }
        vec![Cmd::CheckPaths(new)]
    }

    /// Append lines to the sources (each on its own line).
    pub fn append_sources(&mut self, lines: &[String]) {
        self.sources
            .handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        if self
            .sources
            .lines()
            .last()
            .is_some_and(|l| !l.trim().is_empty())
        {
            self.sources.newline();
        }
        self.sources.insert_str(&lines.join("\n"));
        self.sources.newline();
    }

    /// The editor that has the keyboard (none on a checkbox).
    fn focused_editor(&mut self) -> Option<&mut Editor> {
        match self.focus {
            Focus::Sources => Some(&mut self.sources),
            Focus::Field(f) => self.form.editor_mut(f),
            Focus::Advanced => None,
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let order = self.focus_order();
        let n = order.len();
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = order[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
    }

    /// Footer hints while a popup is open.
    pub fn overlay_hints(&self) -> Option<Vec<(String, String)>> {
        Some(match self.overlay.as_ref()? {
            Overlay::Files(_) => FileBrowser::hints(),
            Overlay::Dirs(_) => DirPicker::hints(),
            Overlay::Preview(p) => p.hints(),
        })
    }

    /// Remove sources that were added; whole lines go, mixed lines keep the rest.
    fn remove_sources(&mut self, done: &HashSet<String>) {
        let mut out = Vec::new();
        for line in self.sources.lines() {
            let toks = source::tokens(line);
            if toks.is_empty() {
                out.push(line.clone());
                continue;
            }
            let keep: Vec<&str> = toks
                .iter()
                .filter(|t| !done.contains(&t.text))
                .map(|t| t.text.as_str())
                .collect();
            if !keep.is_empty() {
                out.push(keep.join(" "));
            }
        }
        self.sources.set_lines(out);
    }
}

fn add_mut(state: &mut State) -> Option<&mut AddState> {
    match state.views.last_mut() {
        Some(View::Add(a)) => Some(&mut **a),
        _ => None,
    }
}

/// Open the add view with `a`, remembering whether the advanced options were
/// shown last time. Returns the path checks the sources need.
pub fn push(state: &mut State, mut a: AddState) -> Vec<Cmd> {
    a.advanced = state.add_advanced;
    let cmds = a.check_cmds();
    state.views.push(View::Add(Box::new(a)));
    cmds
}

/// Keys that start a new line in a multi-line box (plain Enter adds).
fn is_newline_key(key: &KeyEvent) -> bool {
    let m = key.modifiers;
    match key.code {
        KeyCode::Enter => m.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT),
        KeyCode::Char('j') => m.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

/// Raw keys (text entry and popups).
pub fn handle_raw_key(state: &mut State, key: KeyEvent) -> Option<Vec<Cmd>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let a = add_mut(state)?;
    if a.overlay.is_some() {
        // Quit, the palette and help keep working over a popup.
        if (ctrl && matches!(key.code, KeyCode::Char('c' | 'p'))) || key.code == KeyCode::F(1) {
            return None;
        }
        return Some(overlay_key(state, key));
    }
    if is_newline_key(&key) {
        let multi = match a.focus {
            Focus::Sources => true,
            Focus::Field(f) => f.is_multiline(),
            Focus::Advanced => false,
        };
        if !multi {
            return Some(Vec::new());
        }
        let focus = a.focus;
        if let Some(e) = a.focused_editor() {
            e.newline();
        }
        return Some(if focus == Focus::Sources {
            a.check_cmds()
        } else {
            Vec::new()
        });
    }
    match key.code {
        KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_) => return None,
        // Enter (with or without Ctrl) reviews what will be added.
        KeyCode::Enter if a.focus != Focus::Advanced => return Some(apply_submit(state)),
        _ => {}
    }
    if a.focus == Focus::Advanced {
        return match key.code {
            KeyCode::Char(' ') | KeyCode::Enter if !ctrl && !alt => {
                let on = !a.advanced;
                a.set_advanced(on);
                state.add_advanced = on;
                Some(Vec::new())
            }
            KeyCode::Right | KeyCode::Left => {
                let on = key.code == KeyCode::Right;
                a.set_advanced(on);
                state.add_advanced = on;
                Some(Vec::new())
            }
            KeyCode::Up | KeyCode::Down => {
                a.step_focus(key.code == KeyCode::Down);
                Some(Vec::new())
            }
            KeyCode::Char(_) if !ctrl && !alt => Some(Vec::new()),
            _ => None,
        };
    }
    if let Focus::Field(f) = a.focus
        && f.is_check()
    {
        return match key.code {
            KeyCode::Char(' ') if !ctrl && !alt => {
                a.form.toggle(f);
                Some(Vec::new())
            }
            KeyCode::Up => {
                a.step_focus(false);
                Some(Vec::new())
            }
            KeyCode::Down => {
                a.step_focus(true);
                Some(Vec::new())
            }
            KeyCode::Char(_) if !ctrl && !alt => Some(Vec::new()),
            _ => None,
        };
    }
    // Up/Down leave a field at its first/last line.
    if let Focus::Field(f) = a.focus
        && matches!(key.code, KeyCode::Up | KeyCode::Down)
    {
        let up = key.code == KeyCode::Up;
        if !a.form.editor(f).is_some_and(|e| e.can_move(up)) {
            a.step_focus(!up);
            return Some(Vec::new());
        }
    }
    let focus = a.focus;
    let edited = a.focused_editor()?.handle_key(key);
    if !edited {
        return None;
    }
    Some(if focus == Focus::Sources {
        a.check_cmds()
    } else {
        Vec::new()
    })
}

fn overlay_key(state: &mut State, key: KeyEvent) -> Vec<Cmd> {
    let Some(a) = add_mut(state) else {
        return Vec::new();
    };
    let Some(mut overlay) = a.overlay.take() else {
        return Vec::new();
    };
    let mut cmds = Vec::new();
    match &mut overlay {
        Overlay::Files(b) => match b.key(key) {
            FileOutcome::Stay => {}
            FileOutcome::Close => return cmds,
            FileOutcome::List(p) => cmds.push(Cmd::ReadDir(p)),
            FileOutcome::Pick(paths) => {
                a.append_sources(&paths);
                a.focus = Focus::Sources;
                return a.check_cmds();
            }
        },
        Overlay::Dirs(p) => match p.key(key) {
            DirOutcome::Stay => {}
            DirOutcome::Close => return cmds,
            DirOutcome::List(path) => {
                a.overlay = Some(overlay);
                return state.call_quiet(ApiCall::FsList(FsListParams {
                    path,
                    dirs_only: true,
                }));
            }
            DirOutcome::Choose(dir) => {
                a.form.set_text(Field::Dir, &dir);
                a.focus = Focus::Field(Field::Dir);
                return cmds;
            }
        },
        Overlay::Preview(p) => match p.key(key) {
            PreviewOutcome::Stay | PreviewOutcome::CancelFetch => {}
            PreviewOutcome::Probe(params) => {
                let cmds = state.call_quiet(ApiCall::VideoProbe(params));
                if cmds.is_empty() {
                    // Offline: the call was refused (the status line says why).
                    if let Overlay::Preview(p) = &mut overlay {
                        p.probe = ProbeFetch::Idle;
                    }
                }
                if let Some(a) = add_mut(state) {
                    a.overlay = Some(overlay);
                }
                return cmds;
            }
            PreviewOutcome::Back => return cmds,
            PreviewOutcome::Fetch(uri) => {
                let cmds = state.call_quiet(ApiCall::PreviewMagnet { uri });
                if cmds.is_empty() {
                    // Offline: the call was refused (the status line says why).
                    overlay_fetch_idle(&mut overlay);
                }
                if let Some(a) = add_mut(state) {
                    a.overlay = Some(overlay);
                }
                return cmds;
            }
            PreviewOutcome::Confirm => {
                let params = p.confirm_params();
                a.submitting = Some(p.origins.clone());
                let cmds = state.call(ApiCall::Add(Box::new(params)));
                if cmds.is_empty()
                    && let Some(a) = add_mut(state)
                {
                    // Offline: the call was refused; keep the preview open.
                    a.submitting = None;
                    a.overlay = Some(overlay);
                }
                return cmds;
            }
        },
    }
    a.overlay = Some(overlay);
    cmds
}

/// Add-view actions.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    add_mut(state)?;
    Some(match action {
        Action::AddSubmit => apply_submit(state),
        Action::AddNextField | Action::AddPrevField => {
            if let Some(a) = add_mut(state) {
                a.step_focus(action == Action::AddNextField);
            }
            Vec::new()
        }
        Action::AddBrowseFiles => {
            if let Some(a) = add_mut(state) {
                a.overlay = Some(Overlay::Files(FileBrowser::new()));
            }
            vec![Cmd::ReadDir(None)]
        }
        Action::AddPasteClipboard => vec![Cmd::ReadClipboard],
        Action::AddToggleMirror => {
            if let Some(a) = add_mut(state) {
                a.form.toggle(Field::Mirror);
                let on = a.form.checked(Field::Mirror);
                state.status.info(if on {
                    "Mirrors on: all links are treated as copies of one file"
                } else {
                    "Mirrors off: every link is its own download"
                });
            }
            Vec::new()
        }
        Action::AddToggleAdvanced => {
            if let Some(a) = add_mut(state) {
                let on = !a.advanced;
                a.set_advanced(on);
                state.add_advanced = on;
            }
            Vec::new()
        }
        Action::AddToggleVideo => {
            let r = add_mut(state).map(AddState::toggle_video_line);
            if let Some(Err(msg)) = r {
                state.status.warn(msg);
            }
            Vec::new()
        }
        Action::AddPickDir => {
            let start = add_mut(state)
                .map(|a| {
                    a.form
                        .editor(Field::Dir)
                        .map(Editor::text)
                        .unwrap_or_default()
                })
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty());
            let cmds = state.call_quiet(ApiCall::FsList(FsListParams {
                path: start,
                dirs_only: true,
            }));
            if !cmds.is_empty()
                && let Some(a) = add_mut(state)
            {
                a.overlay = Some(Overlay::Dirs(DirPicker::new()));
            }
            cmds
        }
        _ => return None,
    })
}

fn apply_submit(state: &mut State) -> Vec<Cmd> {
    let result = match add_mut(state) {
        Some(a) => prepare(a),
        None => return Vec::new(),
    };
    match result {
        Ok(cmds) => cmds,
        Err(msg) => {
            state.status.error(msg);
            Vec::new()
        }
    }
}

/// Validate the form; either start reading local files / open the preview, or
/// say what is wrong.
fn prepare(a: &mut AddState) -> Result<Vec<Cmd>, String> {
    if a.submitting.is_some() {
        return Err("Still adding; wait for the daemon's answer".into());
    }
    let errors = a.errors();
    if let Some((f, msg)) = errors.first() {
        if !a.visible_fields().contains(f) {
            a.advanced = true;
        }
        a.focus = Focus::Field(*f);
        return Err(format!("{}: {msg} — fix it, then press Enter", f.label()));
    }
    let tokens = a.all_tokens();
    if tokens.is_empty() {
        a.focus = Focus::Sources;
        return Err(
            "Nothing to add yet: paste a link (Ctrl-v), a magnet or a .torrent path first".into(),
        );
    }
    if let Some(t) = tokens.iter().find(|t| !t.kind.is_supported()) {
        a.focus = Focus::Sources;
        return Err(format!(
            "Cannot add `{}`: {} — fix or remove that line",
            truncate(&t.text, 40),
            t.kind.label()
        ));
    }
    let parsed = a.form.parse();
    let plan = preview::plan(&tokens, parsed.mirror);
    let files: Vec<String> = plan
        .iter()
        .filter_map(|p| match p {
            Planned::File(t) => Some(t.text.clone()),
            Planned::Uris(_) | Planned::Video(_) => None,
        })
        .collect();
    if files.is_empty() {
        open_preview(a, &plan, &Loaded::new());
        Ok(Vec::new())
    } else {
        a.pending_plan = Some(plan);
        Ok(vec![Cmd::ReadFiles(files)])
    }
}

fn open_preview(a: &mut AddState, plan: &[Planned], files: &Loaded) {
    let parsed = a.form.parse();
    let items = preview::build_items(plan, files, &parsed);
    let params = preview::build_params(plan, files, &parsed).unwrap_or_else(|_| {
        // A file failed to load: the item carries the error and blocks confirming.
        dman_proto::AddParams {
            sources: Vec::new(),
            options: parsed.options.clone(),
            paused: parsed.paused,
        }
    });
    a.overlay = Some(Overlay::Preview(Box::new(Preview::new(
        items,
        params,
        plan.iter().map(Planned::tokens).collect(),
    ))));
}

fn overlay_fetch_idle(overlay: &mut Overlay) {
    if let Overlay::Preview(p) = overlay {
        p.fetch = Fetch::Idle;
    }
}

/// The confirmation shown when Esc would throw away a filled-in form.
pub fn discard_confirm() -> ConfirmState {
    ConfirmState::discard(
        "Leave without adding?",
        vec!["The links and options you entered are not added and will be lost.".into()],
    )
}

/// Every add view on the stack (a help screen may sit above it).
fn add_views(state: &mut State) -> impl Iterator<Item = &mut AddState> {
    state.views.iter_mut().filter_map(|v| match v {
        View::Add(a) => Some(&mut **a),
        _ => None,
    })
}

/// Tick: advance the spinner of a running magnet fetch. Returns whether the
/// screen needs a redraw because of it.
pub fn on_tick(state: &mut State) -> bool {
    let tick = state.tick;
    let mut animated = false;
    for a in add_views(state) {
        if let Some(Overlay::Preview(p)) = &mut a.overlay {
            animated |= p.on_tick(tick);
        }
    }
    animated
}

/// The connection dropped: a running fetch will never be answered.
pub fn on_disconnect(state: &mut State) {
    for a in add_views(state) {
        if let Some(Overlay::Preview(p)) = &mut a.overlay {
            if p.fetching() {
                p.fetch = Fetch::Failed("lost the connection to the daemon".into());
            }
            if p.probing() {
                p.probe = ProbeFetch::Failed("lost the connection to the daemon".into());
            }
        }
    }
}

/// The daemon answered `jobs.preview_magnet`. A fetch that was cancelled (or
/// whose preview was closed) is no longer running, so its answer is dropped.
pub fn on_magnet_preview(state: &mut State, uri: &str, result: &Result<ApiReply, String>) {
    for a in add_views(state) {
        let Some(Overlay::Preview(p)) = &mut a.overlay else {
            continue;
        };
        if !matches!(&p.fetch, Fetch::Running { uri: u, .. } if u == uri) {
            continue;
        }
        p.fetch = match result {
            Ok(ApiReply::MagnetPreview(m)) if m.files.is_empty() => {
                Fetch::Failed("the torrent lists no files".into())
            }
            Ok(ApiReply::MagnetPreview(m)) => Fetch::Done(Box::new(MagnetFiles::from_preview(
                m,
                p.params.options.select_files.as_deref(),
            ))),
            Ok(_) => Fetch::Failed("unexpected reply".into()),
            Err(e) => Fetch::Failed(e.clone()),
        };
        p.scroll = 0;
    }
}

/// The daemon answered `video.probe`. A probe that was cancelled (or whose
/// preview was closed) is no longer running, so its answer is dropped.
pub fn on_video_probe(
    state: &mut State,
    params: &dman_proto::VideoProbeParams,
    result: &Result<ApiReply, String>,
) {
    for a in add_views(state) {
        let Some(Overlay::Preview(p)) = &mut a.overlay else {
            continue;
        };
        if !matches!(&p.probe, ProbeFetch::Running { url, .. } if *url == params.url) {
            continue;
        }
        p.probe = match result {
            Ok(ApiReply::VideoProbe(v)) => {
                ProbeFetch::Done(Box::new(video::ProbeUi::new((**v).clone())))
            }
            Ok(_) => ProbeFetch::Failed("unexpected reply".into()),
            Err(e) => ProbeFetch::Failed(e.clone()),
        };
        p.scroll = 0;
    }
}

/// Re-read the video engine from the engine statuses (after every update).
pub fn sync_engine(state: &mut State) {
    let engines = state.engines.clone();
    for a in add_views(state) {
        let ctx = a.video.from_engines(&engines);
        if !ctx.same_engine(&a.video) {
            a.video = VideoCtx {
                forced: std::mem::take(&mut a.video.forced),
                ..ctx
            };
        }
        if let Focus::Field(f) = a.focus
            && !a.visible_fields().contains(&f)
        {
            a.focus = Focus::Sources;
        }
    }
}

/// Bracketed paste into the add view.
pub fn on_paste(state: &mut State, text: &str) -> Vec<Cmd> {
    let Some(a) = add_mut(state) else {
        return Vec::new();
    };
    if a.overlay.is_some() {
        state.dirty = false;
        return Vec::new();
    }
    let focus = a.focus;
    match a.focused_editor() {
        Some(e) => e.insert_str(text),
        None => return Vec::new(),
    }
    if focus == Focus::Sources {
        a.check_cmds()
    } else {
        Vec::new()
    }
}

/// A local directory listing arrived.
pub fn on_local_dir(state: &mut State, result: Result<LocalListing, String>) {
    if let Some(a) = add_mut(state)
        && let Some(Overlay::Files(b)) = &mut a.overlay
    {
        b.on_listing(result);
    }
}

/// Local file checks arrived.
pub fn on_paths_checked(state: &mut State, results: Vec<(String, PathStatus)>) {
    if let Some(a) = add_mut(state) {
        for (k, v) in results {
            if a.paths.contains_key(&k) {
                a.paths.insert(k, v);
            }
        }
    }
}

/// Local files were read for the preview.
pub fn on_files_read(state: &mut State, files: &Loaded) {
    if let Some(a) = add_mut(state)
        && let Some(plan) = a.pending_plan.take()
    {
        open_preview(a, &plan, files);
    }
}

/// The clipboard text (or why it could not be read) arrived.
pub fn on_clipboard(state: &mut State, result: Result<String, String>) -> Vec<Cmd> {
    match result {
        Ok(text) if text.trim().is_empty() => {
            state.status.warn("The clipboard is empty");
            Vec::new()
        }
        Ok(text) => on_paste(state, &text),
        Err(e) => {
            state.status.warn(format!(
                "Clipboard unavailable ({e}). Paste with your terminal instead \
                 (Ctrl-Shift-V, Cmd-V or middle click)."
            ));
            Vec::new()
        }
    }
}

/// The daemon answered `fs.list` for the directory picker.
pub fn on_fs_listing(state: &mut State, result: &Result<ApiReply, String>) {
    let r = match result {
        Ok(ApiReply::Listing(l)) => Ok(l.clone()),
        Ok(_) => Err("unexpected reply".to_owned()),
        Err(e) => Err(e.clone()),
    };
    if let Some(a) = add_mut(state)
        && let Some(Overlay::Dirs(p)) = &mut a.overlay
    {
        p.on_listing(r);
    }
}

/// The daemon answered `jobs.add` (from the add view or straight from the
/// search results). On full success the add view closes, the first new job
/// gets the cursor of the downloads list and the status line says what
/// happens next; otherwise the sources that were added are dropped from the
/// form and the rest stay for another try.
pub fn on_add_result(state: &mut State, params: &AddParams, result: &Result<ApiReply, String>) {
    let Ok(ApiReply::Added(outcomes)) = result else {
        if let Some(a) = add_mut(state) {
            a.submitting = None;
        }
        return;
    };
    let ok = |o: &AddOutcome| matches!(o, AddOutcome::Added { .. });
    if let Some(id) = outcomes.iter().find_map(|o| match o {
        AddOutcome::Added { id, .. } => Some(*id),
        AddOutcome::Failed { .. } => None,
    }) {
        state.list.focus_when_listed(id);
        state.refresh_list();
    }
    let all_ok = outcomes.iter().all(ok);
    if all_ok && let Some(mut text) = added_message(state, params, outcomes) {
        if matches!(state.view(), View::Search) {
            text.push_str(" (Esc shows your downloads)");
        }
        state.status.info(text);
    }
    let Some(a) = add_mut(state) else { return };
    let origins = a.submitting.take();
    if all_ok {
        state.views.pop();
        return;
    }
    if let Some(origins) = origins {
        let done: HashSet<String> = origins
            .iter()
            .zip(outcomes)
            .filter(|(_, o)| ok(o))
            .flat_map(|(t, _)| t.iter().cloned())
            .collect();
        a.remove_sources(&done);
    }
}

/// A name for what `source` will download, before the daemon knows better.
fn source_name(source: &AddSource) -> String {
    match source {
        AddSource::Uris { uris } => uris
            .first()
            .map(|u| preview::uri_name(u))
            .unwrap_or_default(),
        AddSource::Torrent { name, .. } | AddSource::Metalink { name, .. } => name
            .trim_end_matches(".torrent")
            .trim_end_matches(".metalink")
            .trim_end_matches(".meta4")
            .to_owned(),
        AddSource::Video { url, .. } => preview::uri_name(url),
    }
}

fn is_magnet(source: &AddSource) -> bool {
    matches!(source, AddSource::Uris { uris }
        if uris.first().is_some_and(|u| u.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))))
}

/// "Added 'Name' — downloading" (or paused / fetching metadata) after a
/// fully successful add.
pub fn added_message(state: &State, params: &AddParams, outcomes: &[AddOutcome]) -> Option<String> {
    let resume = state
        .keymap
        .keys(crate::keymap::Context::Downloads, Action::Resume)
        .first()
        .map_or_else(|| "resume".to_owned(), |k| crate::keymap::key_label(*k));
    let next = if params.paused {
        format!("paused, press {resume} to start")
    } else if params.sources.iter().all(is_magnet) {
        "fetching torrent metadata…".to_owned()
    } else {
        "downloading".to_owned()
    };
    let what = match (params.sources.as_slice(), outcomes) {
        ([source], [AddOutcome::Added { id, extra_ids }]) if extra_ids.is_empty() => {
            let known = state
                .jobs
                .get(id)
                .map(|j| j.name.clone())
                .filter(|n| !n.is_empty() && !n.starts_with("[METADATA]"));
            let name = known.unwrap_or_else(|| source_name(source));
            format!("'{}'", truncate(&name, 50))
        }
        _ => {
            let n: usize = outcomes.iter().map(|o| o.ids().len()).sum();
            plural(n, "download")
        }
    };
    Some(format!("Added {what} — {next}"))
}

// ------------------------------------------------------------------ drawing

fn severity_style(theme: &Theme, s: Severity) -> ratatui::style::Style {
    match s {
        Severity::Ok => theme.ok,
        Severity::Warn => theme.warn,
        Severity::Error => theme.error,
    }
}

fn line_label(a: &AddState, line: &str) -> Option<(String, Severity)> {
    let t = source::tokens_with(line, &a.video);
    match t.len() {
        0 => None,
        1 => {
            let (mut text, mut sev) = source::describe(&t[0], &a.paths);
            if matches!(t[0].kind, Kind::Http | Kind::Https) && a.video.available {
                text.push_str(" · Ctrl-g: try as video");
            }
            if t[0].kind == Kind::VideoUnavailable {
                sev = Severity::Warn;
            }
            Some((text, sev))
        }
        n => {
            let worst = t.iter().map(|t| source::describe(t, &a.paths).1).fold(
                Severity::Ok,
                |w, s| match (w, s) {
                    (Severity::Error, _) | (_, Severity::Error) => Severity::Error,
                    (Severity::Warn, _) | (_, Severity::Warn) => Severity::Warn,
                    _ => Severity::Ok,
                },
            );
            Some((format!("{n} sources"), worst))
        }
    }
}

fn sources_title(a: &AddState) -> String {
    let t = a.all_tokens();
    if t.is_empty() {
        return " Sources ".into();
    }
    let bad = t.iter().filter(|t| !t.kind.is_supported()).count();
    let files = t.iter().filter(|t| t.kind.is_file()).count();
    let mut s = format!(" Sources · {}", plural(t.len(), "item"));
    if files > 0 {
        s.push_str(&format!(" ({} local)", files));
    }
    if bad > 0 {
        s.push_str(&format!(" · {bad} unsupported"));
    }
    s.push(' ');
    s
}

fn pad(mut line: Line<'static>, width: usize) -> Line<'static> {
    let w = line.width();
    if w < width {
        line.spans.push(Span::raw(" ".repeat(width - w)));
    }
    line
}

fn render_sources(frame: &mut Frame<'_>, area: Rect, theme: &Theme, a: &AddState) {
    let focused = a.focus == Focus::Sources && a.overlay.is_none();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused { theme.accent } else { theme.border })
        .title(Span::styled(sources_title(a), theme.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = usize::from(inner.height);
    let width = usize::from(inner.width);
    let (crow, _) = a.sources.cursor();
    let top = (crow + 1).saturating_sub(height);
    let mut lines = Vec::new();
    for row in top..a.sources.lines().len().min(top + height) {
        let label = line_label(a, &a.sources.lines()[row]);
        let label_w = label
            .as_ref()
            .map_or(0, |(l, _)| l.chars().count().min(width / 2));
        let text_w = width.saturating_sub(label_w + 1).max(1);
        let mut line = pad(a.sources.render_line(row, text_w, focused, theme), text_w);
        if let Some((text, sev)) = label {
            line.spans.push(Span::raw(" "));
            line.spans.push(Span::styled(
                truncate(&text, label_w),
                severity_style(theme, sev),
            ));
        }
        lines.push(line);
    }
    if a.sources.is_empty() {
        lines.push(Line::styled(
            "Paste or type links: http(s), ftp, magnet, or a .torrent/.metalink path.",
            theme.dim,
        ));
        lines.push(Line::styled(
            "One per line (Ctrl-j starts a new line). Ctrl-v pastes the clipboard,",
            theme.dim,
        ));
        lines.push(Line::styled(
            "Ctrl-o picks local files. Then press Enter.",
            theme.dim,
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

const LABEL_W: usize = 22;

/// Footer hints of the add view (or of its open popup).
pub fn footer_hints(state: &State, a: &AddState) -> Vec<Hint> {
    if let Some(h) = a.overlay_hints() {
        return h;
    }
    let h = |act: Action, label: &str| hint(state, act, label);
    let mut v = vec![h(Action::AddSubmit, "review & add")];
    match a.focus {
        Focus::Sources => v.push(Some(own("Ctrl-j", "new line"))),
        Focus::Field(f) if f.is_check() => v.push(Some(own("Space", "tick / untick"))),
        Focus::Field(f) if f.is_multiline() => v.push(Some(own("Ctrl-j", "new line"))),
        Focus::Field(Field::Dir) => v.push(h(Action::AddPickDir, "browse folders")),
        Focus::Advanced => v.push(Some(own("Space", if a.advanced { "hide" } else { "show" }))),
        Focus::Field(_) => {}
    }
    v.push(h(
        Action::AddNextField,
        if a.focus == Focus::Sources {
            "options"
        } else {
            "next"
        },
    ));
    v.push(h(Action::Back, "back"));
    v.push(h(Action::AddPasteClipboard, "paste"));
    v.push(h(Action::AddBrowseFiles, "files"));
    v.push(h(
        Action::AddToggleAdvanced,
        if a.advanced {
            "fewer options"
        } else {
            "more options"
        },
    ));
    v.into_iter().flatten().collect()
}

/// The call to action under the sources: what Enter does right now.
fn action_line(state: &State, a: &AddState, width: usize) -> Line<'static> {
    let t = &state.theme;
    if a.submitting.is_some() {
        return Line::styled(" Adding… waiting for the daemon", t.dim);
    }
    match a.readiness() {
        Ok(n) => {
            let paused = a.form.checked(Field::Paused);
            let what = format!(
                " ⏎ Enter  Add {}{} ",
                plural(n, "download"),
                if paused { " (paused)" } else { "" }
            );
            let rest = " you get a summary, then Enter again starts it";
            let room = width.saturating_sub(what.chars().count() + 1);
            Line::from(vec![
                Span::raw(" "),
                Span::styled(what, t.action),
                Span::styled(truncate(rest, room), t.dim),
            ])
        }
        Err(e) if e.is_empty() => Line::from(vec![
            Span::styled(" ⏎ Enter", t.key),
            Span::styled(
                truncate(
                    "  nothing to add yet — paste a link (Ctrl-v) or type one",
                    width.saturating_sub(9),
                ),
                t.dim,
            ),
        ]),
        Err(e) => Line::styled(truncate(&format!(" ✗ {e}"), width), t.error),
    }
}

/// The "Advanced options" row.
fn advanced_line(a: &AddState, theme: &Theme, width: usize) -> Line<'static> {
    let focused = a.focus == Focus::Advanced && a.overlay.is_none();
    let marker = if focused { "▸ " } else { "  " };
    let style = if focused { theme.accent } else { theme.text };
    let (arrow, what) = if a.advanced {
        ("▾", "Advanced options".to_owned())
    } else {
        ("▸", "Advanced options".to_owned())
    };
    let set = a.advanced_set();
    let extra = if a.advanced {
        " (F2 hides them)".to_owned()
    } else if set.is_empty() {
        let names: Vec<&str> = a
            .advanced_fields()
            .iter()
            .take(4)
            .map(|f| f.label())
            .collect();
        format!(
            " — {} more, F2 shows them: {}, …",
            a.advanced_fields().len(),
            names.join(", ").to_lowercase()
        )
    } else {
        let names: Vec<&str> = set.iter().map(|f| f.label()).collect();
        format!(" — {} set: {} (F2 shows them)", set.len(), names.join(", "))
    };
    let used = 2 + 2 + what.chars().count();
    Line::from(vec![
        Span::styled(marker, theme.accent),
        Span::styled(format!("{arrow} {what}"), style),
        Span::styled(truncate(&extra, width.saturating_sub(used)), theme.dim),
    ])
}

/// Lines of the options box, and the line of the focused field.
fn option_lines(a: &AddState, theme: &Theme, width: usize) -> (Vec<Line<'static>>, usize) {
    let value_w = width.saturating_sub(2 + LABEL_W + 1).max(1);
    let errors = a.errors();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut focus_line = 0;
    let common = a.common_fields();
    let mut fields: Vec<Option<Field>> = common.iter().copied().map(Some).collect();
    fields.push(None);
    if a.advanced {
        fields.extend(a.advanced_fields().into_iter().map(Some));
    }
    for entry in fields {
        let Some(f) = entry else {
            if a.focus == Focus::Advanced {
                focus_line = lines.len();
            }
            lines.push(advanced_line(a, theme, width));
            continue;
        };
        if f == Field::VIDEO[1] {
            lines.push(Line::styled(
                "── Video (yt-dlp) ──".to_owned(),
                theme.accent,
            ));
        }
        let focused = a.focus == Focus::Field(f) && a.overlay.is_none();
        let marker = if focused { "▸ " } else { "  " };
        let label = Span::styled(
            format!("{:<LABEL_W$} ", f.label()),
            if focused { theme.accent } else { theme.text },
        );
        let head = Span::styled(marker, theme.accent);
        if focused {
            focus_line = lines.len();
        }
        if f.is_check() {
            let mark = if a.form.checked(f) { "[x]" } else { "[ ]" };
            lines.push(Line::from(vec![
                head,
                label,
                Span::raw(mark),
                Span::styled(
                    truncate(&format!(" {}", f.hint()), value_w.saturating_sub(3)),
                    theme.dim,
                ),
            ]));
        } else if let Some(e) = a.form.editor(f) {
            if f.is_multiline() && focused {
                let (crow, _) = e.cursor();
                let shown = e.lines().len().min(4);
                let top = (crow + 1).saturating_sub(shown);
                for (i, row) in (top..top + shown).enumerate() {
                    if row == crow {
                        focus_line = lines.len();
                    }
                    let first = if i == 0 {
                        vec![head.clone(), label.clone()]
                    } else {
                        vec![Span::raw(" ".repeat(2 + LABEL_W + 1))]
                    };
                    let mut spans = first;
                    spans.extend(e.render_line(row, value_w, true, theme).spans);
                    lines.push(Line::from(spans));
                }
                if e.is_empty() {
                    lines.push(Line::styled(
                        format!("{}{}", " ".repeat(2 + LABEL_W + 1), f.hint()),
                        theme.dim,
                    ));
                }
            } else {
                let mut spans = vec![head, label];
                if e.is_empty() && !focused {
                    spans.push(Span::styled(truncate(f.hint(), value_w), theme.dim));
                } else if f.is_multiline() {
                    let joined = e
                        .lines()
                        .iter()
                        .filter(|l| !l.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ");
                    spans.push(Span::raw(truncate(&joined, value_w)));
                } else {
                    spans.extend(e.render_line(0, value_w, focused, theme).spans);
                    if e.is_empty() {
                        spans.push(Span::styled(
                            truncate(&format!(" {}", f.hint()), value_w.saturating_sub(1)),
                            theme.dim,
                        ));
                    }
                }
                lines.push(Line::from(spans));
            }
        }
        for (_, msg) in errors.iter().filter(|(ef, _)| *ef == f) {
            lines.push(Line::styled(
                format!(
                    "{}✗ {}",
                    " ".repeat(2 + LABEL_W + 1),
                    truncate(msg, value_w.saturating_sub(2))
                ),
                theme.error,
            ));
        }
    }
    (lines, focus_line)
}

fn render_options(frame: &mut Frame<'_>, area: Rect, theme: &Theme, a: &AddState) {
    let focused = matches!(a.focus, Focus::Field(_) | Focus::Advanced) && a.overlay.is_none();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused { theme.accent } else { theme.border })
        .title(Span::styled(
            " Options · empty fields use your defaults ",
            theme.accent,
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (lines, focus_line) = option_lines(a, theme, usize::from(inner.width));
    let h = usize::from(inner.height);
    let offset = (focus_line + 1).saturating_sub(h);
    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), inner);
}

/// Draw the add view.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, a: &AddState) {
    let t = &state.theme;
    let hint = a.engine_hint();
    let hint_h = u16::from(hint.is_some());
    let opt_lines = option_lines(a, t, usize::from(area.width.saturating_sub(2)))
        .0
        .len();
    let opt_h = u16::try_from(opt_lines + 2).unwrap_or(u16::MAX);
    // Rows the sources box needs (the empty box shows three lines of help).
    let src_need = if a.sources.is_empty() {
        4
    } else {
        a.sources.lines().len()
    };
    let src_need = u16::try_from(src_need + 2).unwrap_or(u16::MAX);
    let [src, action, hint_area, opt] = if a.advanced {
        let src_h = src_need.clamp(5, (area.height * 2 / 5).max(5));
        Layout::vertical([
            Constraint::Length(src_h),
            Constraint::Length(1),
            Constraint::Length(hint_h),
            Constraint::Fill(1),
        ])
        .areas(area)
    } else {
        // Collapsed: the options take what they need, the sources the rest.
        let room = area.height.saturating_sub(1 + hint_h);
        let opt_h = opt_h.min(room.saturating_sub(5));
        let src_h = room.saturating_sub(opt_h).min(16);
        let [src, action, hint_area, opt, _] = Layout::vertical([
            Constraint::Length(src_h),
            Constraint::Length(1),
            Constraint::Length(hint_h),
            Constraint::Length(opt_h),
            Constraint::Fill(1),
        ])
        .areas(area);
        [src, action, hint_area, opt]
    };
    render_sources(frame, src, t, a);
    frame.render_widget(
        Paragraph::new(action_line(state, a, usize::from(area.width))),
        action,
    );
    if let Some((text, sev)) = hint {
        frame.render_widget(
            Paragraph::new(Line::styled(
                truncate(&format!(" {text}"), usize::from(area.width)),
                severity_style(t, sev),
            )),
            hint_area,
        );
    }
    render_options(frame, opt, t, a);
    match &a.overlay {
        Some(Overlay::Files(b)) => b.render(frame, area, t),
        Some(Overlay::Dirs(p)) => p.render(frame, area, t),
        Some(Overlay::Preview(p)) => p.render(frame, area, t),
        None => {}
    }
}
