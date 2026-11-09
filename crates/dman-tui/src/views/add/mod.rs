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

/// Actions shown in the footer of the form.
pub const FOOTER: &[Action] = &[
    Action::AddSubmit,
    Action::AddNextField,
    Action::AddBrowseFiles,
    Action::AddPasteClipboard,
    Action::AddToggleMirror,
    Action::AddToggleVideo,
    Action::AddPickDir,
    Action::Back,
];

/// Which part of the form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The sources editor.
    Sources,
    /// An options field.
    Field(Field),
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

    /// The fields shown: the video section only with a video source.
    pub fn visible_fields(&self) -> Vec<Field> {
        if self.has_video() {
            Field::FULL.to_vec()
        } else {
            Field::ALL.to_vec()
        }
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
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let fields = self.visible_fields();
        let last = fields.len() - 1;
        self.focus = match self.focus {
            Focus::Sources if forward => Focus::Field(fields[0]),
            Focus::Sources => Focus::Field(fields[last]),
            Focus::Field(f) => match fields.iter().position(|x| *x == f) {
                Some(i) if forward && i == last => Focus::Sources,
                Some(0) if !forward => Focus::Sources,
                Some(i) => Focus::Field(fields[if forward { i + 1 } else { i - 1 }]),
                None => Focus::Sources,
            },
        };
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

/// Raw keys (text entry and popups).
pub fn handle_raw_key(state: &mut State, key: KeyEvent) -> Option<Vec<Cmd>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let a = add_mut(state)?;
    if a.overlay.is_some() {
        // Quit and the palette keep working over a popup.
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'p')) {
            return None;
        }
        return Some(overlay_key(state, key));
    }
    match key.code {
        KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_) => return None,
        KeyCode::Enter if ctrl || alt => return None,
        _ => {}
    }
    if let Focus::Field(f) = a.focus
        && f.is_check()
    {
        return match key.code {
            KeyCode::Char(' ') | KeyCode::Enter if !ctrl && !alt => {
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
    if key.code == KeyCode::Enter
        && let Focus::Field(f) = a.focus
        && !f.is_multiline()
    {
        return Some(apply_submit(state));
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
        a.focus = Focus::Field(*f);
        return Err(format!("{}: {msg}", f.label()));
    }
    let tokens = a.all_tokens();
    if tokens.is_empty() {
        a.focus = Focus::Sources;
        return Err("Nothing to add: enter a URL, magnet link or .torrent path".into());
    }
    if let Some(t) = tokens.iter().find(|t| !t.kind.is_supported()) {
        a.focus = Focus::Sources;
        return Err(format!(
            "Cannot add `{}`: {}",
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
        "Discard this form? y/N",
        vec!["What you entered in the add form is lost.".into()],
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

/// The daemon answered `jobs.add`: leave the view on full success, otherwise
/// drop the sources that were added and keep the rest for another try.
pub fn on_add_result(state: &mut State, result: &Result<ApiReply, String>) {
    let Some(a) = add_mut(state) else { return };
    let origins = a.submitting.take();
    let Ok(ApiReply::Added(outcomes)) = result else {
        return;
    };
    let ok = |o: &dman_proto::AddOutcome| matches!(o, dman_proto::AddOutcome::Added { .. });
    if outcomes.iter().all(ok) {
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
            "Paste or type URLs, magnet links or .torrent/.metalink paths, one per line.",
            theme.dim,
        ));
        lines.push(Line::styled(
            "Ctrl-o browses local files, Ctrl-v reads the clipboard, Ctrl-s previews.",
            theme.dim,
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

const LABEL_W: usize = 13;

fn render_options(frame: &mut Frame<'_>, area: Rect, theme: &Theme, a: &AddState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            if matches!(a.focus, Focus::Field(_)) && a.overlay.is_none() {
                theme.accent
            } else {
                theme.border
            },
        )
        .title(Span::styled(
            " Options (empty = profile default) ",
            theme.accent,
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let value_w = width.saturating_sub(2 + LABEL_W + 1).max(1);
    let errors = a.errors();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut focus_line = 0;
    for f in a.visible_fields() {
        if f == Field::VIDEO[0] {
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
                Span::styled(format!(" {}", f.hint()), theme.dim),
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
    let h = usize::from(inner.height);
    let offset = (focus_line + 1).saturating_sub(h);
    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), inner);
}

/// Draw the add view.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, a: &AddState) {
    let t = &state.theme;
    let src_h = (area.height * 2 / 5).clamp(5, 14);
    let hint = a.engine_hint();
    let [src, hint_area, opt] = Layout::vertical([
        Constraint::Length(src_h),
        Constraint::Length(u16::from(hint.is_some())),
        Constraint::Fill(1),
    ])
    .areas(area);
    render_sources(frame, src, t, a);
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
