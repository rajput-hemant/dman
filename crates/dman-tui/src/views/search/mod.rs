//! Search view: query, provider selector, live results table, filters, adding
//! from results and the search history.
//!
//! The search runs in the daemon. `search.start` answers at once with an id;
//! progress arrives as [`Event::SearchUpdate`] (cumulative merged results plus
//! one provider's status) and [`Event::SearchFinished`]. All of it lives in
//! [`SearchState`] on [`State`], *not* in the view, so leaving the view neither
//! cancels the search nor loses its results; only Esc inside the view cancels.
//!
//! Events may arrive before the `search.start` reply has told us the id; they
//! are buffered ([`SearchState::early`]) and replayed when the reply comes.
//!
//! Adding: a result's magnet is used as is; with only a detail page it is first
//! resolved with `search.resolve` (an [`AddFlow`]); with only a torrent URL that
//! URL is added as an http source. The sources then open the Add view
//! (prefilled) or, for `A`, go straight to `jobs.add` with the profile defaults.

pub mod filter;
pub mod render;

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dman_proto::search::{
    ProviderStatus, SearchHistoryEntry, SearchResolveParams, SearchResultDto, SearchSnapshot,
    SearchStartParams, SearchStarted,
};
use dman_proto::{AddParams, AddSource, JobOptions};

use self::filter::{Filters, SortKey};
pub use self::render::render;
use crate::action::Action;
use crate::api::{ApiCall, ApiReply};
use crate::app::{Cmd, State};
use crate::format::plural;
use crate::views::View;
use crate::views::add::AddState;
use crate::views::providers;
use crate::widgets::Modal;
use crate::widgets::confirm::ConfirmState;
use crate::widgets::input::LineInput;
use crate::widgets::prompt::{PromptKind, PromptState};

/// Footer actions while the results have the keyboard.
pub const FOOTER: &[Action] = &[
    Action::SearchAdd,
    Action::SearchEdit,
    Action::ToggleSelect,
    Action::SortNext,
    Action::Filter,
    Action::SearchHistory,
    Action::SearchProviders,
    Action::SearchCancel,
    Action::Help,
];

/// Most early events kept while waiting for the `search.start` reply.
const MAX_EARLY: usize = 64;

/// Which part of the view has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The query line.
    Query,
    /// The optional category line.
    Category,
    /// The provider selector.
    Providers,
    /// The results table.
    Results,
}

impl Focus {
    const ORDER: [Focus; 4] = [
        Focus::Query,
        Focus::Category,
        Focus::Providers,
        Focus::Results,
    ];

    fn step(self, forward: bool) -> Focus {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        let n = Self::ORDER.len();
        Self::ORDER[(i + if forward { 1 } else { n - 1 }) % n]
    }

    /// Whether the focus is a text line.
    pub const fn is_text(self) -> bool {
        matches!(self, Focus::Query | Focus::Category)
    }
}

/// Progress of one provider in the search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prov {
    /// Still querying.
    Running,
    /// Finished with `count` results.
    Done {
        /// Results returned.
        count: u32,
        /// Time taken.
        elapsed_ms: u64,
    },
    /// Failed; the other providers are unaffected.
    Failed(String),
    /// The search ended (cancelled) before it answered.
    Cancelled,
}

impl From<ProviderStatus> for Prov {
    fn from(s: ProviderStatus) -> Self {
        match s {
            ProviderStatus::Running => Prov::Running,
            ProviderStatus::Done { count, elapsed_ms } => Prov::Done { count, elapsed_ms },
            ProviderStatus::Failed { error } => Prov::Failed(error),
        }
    }
}

/// One search, running or finished.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Daemon id; `None` until `search.start` answered.
    pub id: Option<u64>,
    /// The query.
    pub query: String,
    /// The category, if any.
    pub category: Option<String>,
    /// Status per provider, in the order first seen.
    pub providers: Vec<(String, Prov)>,
    /// The latest merged list from the daemon.
    pub results: Vec<SearchResultDto>,
    /// Every provider finished, or the search was cancelled / failed to start.
    pub finished: bool,
    /// `search.start` failed with this.
    pub error: Option<String>,
    /// A cancel was requested.
    pub cancelling: bool,
}

impl Run {
    /// Providers that failed.
    pub fn failed(&self) -> usize {
        self.providers
            .iter()
            .filter(|(_, p)| matches!(p, Prov::Failed(_)))
            .count()
    }

    fn set_provider(&mut self, name: &str, status: Prov) {
        match self.providers.iter_mut().find(|(n, _)| n == name) {
            Some((_, p)) => *p = status,
            None => self.providers.push((name.to_owned(), status)),
        }
    }
}

/// An event that arrived before its search id was known.
#[derive(Debug, Clone, PartialEq)]
enum Early {
    Update {
        id: u64,
        provider: String,
        status: ProviderStatus,
        results: Vec<SearchResultDto>,
    },
    Finished(u64),
}

/// Where one result of an add stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowState {
    /// A source is known (magnet, or a torrent URL).
    Ready(String),
    /// `search.resolve` is in flight for this detail page.
    Resolving(String),
    /// It cannot be added.
    Failed(String),
}

/// One result being added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowItem {
    /// Result identity ([`result_key`]).
    pub key: String,
    /// Title, for messages.
    pub title: String,
    /// Progress.
    pub state: FlowState,
}

/// Results being added while their magnets are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddFlow {
    /// Add at once with profile defaults instead of opening the Add view.
    pub direct: bool,
    /// The results.
    pub items: Vec<FlowItem>,
}

impl AddFlow {
    fn resolving(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.state, FlowState::Resolving(_)))
            .count()
    }
}

/// Everything about search that outlives the view.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// Keyboard focus.
    pub focus: Focus,
    /// The query text.
    pub query: LineInput,
    /// The category text.
    pub category: LineInput,
    /// Enabled providers the user switched off for the next search.
    pub off: BTreeSet<String>,
    /// Cursor in the provider selector.
    pub sel_cursor: usize,
    /// The last search.
    pub run: Option<Run>,
    /// `search.start` has not answered yet.
    pub starting: bool,
    early: Vec<Early>,
    /// Result filters.
    pub filters: Filters,
    /// Sort key.
    pub sort: SortKey,
    /// Reversed sort order.
    pub reverse: bool,
    /// Indexes into the run's results that are shown, in order.
    pub rows: Vec<usize>,
    /// Cursor row (index into `rows`).
    pub cursor: usize,
    /// First shown row.
    pub offset: usize,
    /// Marked results ([`result_key`]).
    pub selected: BTreeSet<String>,
    /// Results being added.
    pub adding: Option<AddFlow>,
    /// The history panel replaces the results.
    pub history_open: bool,
    /// Past searches, newest first.
    pub history: Vec<SearchHistoryEntry>,
    /// History cursor.
    pub hist_cursor: usize,
    /// The first-run prompt was shown (memory only, decision D26).
    pub first_run_shown: bool,
    /// Spinner frame.
    pub spin: usize,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            focus: Focus::Query,
            query: LineInput::default(),
            category: LineInput::default(),
            off: BTreeSet::new(),
            sel_cursor: 0,
            run: None,
            starting: false,
            early: Vec::new(),
            filters: Filters::default(),
            sort: SortKey::default(),
            reverse: false,
            rows: Vec::new(),
            cursor: 0,
            offset: 0,
            selected: BTreeSet::new(),
            adding: None,
            history_open: false,
            history: Vec::new(),
            hist_cursor: 0,
            first_run_shown: false,
            spin: 0,
        }
    }
}

/// Identity of a result across list updates.
pub fn result_key(r: &SearchResultDto) -> String {
    r.info_hash
        .clone()
        .or_else(|| r.magnet.clone())
        .or_else(|| r.detail_url.clone())
        .or_else(|| r.torrent_url.clone())
        .unwrap_or_else(|| r.title.clone())
}

impl SearchState {
    /// The search is in progress.
    pub fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.finished)
    }

    /// The result under the cursor.
    pub fn current(&self) -> Option<&SearchResultDto> {
        let run = self.run.as_ref()?;
        run.results.get(*self.rows.get(self.cursor)?)
    }

    /// Rebuild the shown rows, keeping the cursor on the same result.
    pub fn recompute(&mut self) {
        let keep = self.current().map(result_key);
        self.recompute_keeping(keep);
    }

    /// [`SearchState::recompute`] with the key of the result to stay on
    /// (taken before the results were replaced).
    fn recompute_keeping(&mut self, keep: Option<String>) {
        let Some(run) = &self.run else {
            self.rows.clear();
            self.cursor = 0;
            self.offset = 0;
            return;
        };
        self.rows = filter::visible(&run.results, &self.filters, self.sort, self.reverse);
        let found = keep.and_then(|k| {
            self.rows
                .iter()
                .position(|i| result_key(&run.results[*i]) == k)
        });
        self.cursor = found
            .unwrap_or(self.cursor)
            .min(self.rows.len().saturating_sub(1));
        self.offset = self.offset.min(self.cursor);
    }

    /// Results an add acts on: the marked ones that are shown, else the cursor row.
    fn targets(&self) -> Vec<SearchResultDto> {
        let Some(run) = &self.run else {
            return Vec::new();
        };
        let marked: Vec<SearchResultDto> = self
            .rows
            .iter()
            .map(|i| &run.results[*i])
            .filter(|r| self.selected.contains(&result_key(r)))
            .cloned()
            .collect();
        if marked.is_empty() {
            self.current().cloned().into_iter().collect()
        } else {
            marked
        }
    }

    /// Keys of the results whose magnet is being resolved.
    pub fn resolving_keys(&self) -> Vec<&str> {
        self.adding.as_ref().map_or_else(Vec::new, |f| {
            f.items
                .iter()
                .filter(|i| matches!(i.state, FlowState::Resolving(_)))
                .map(|i| i.key.as_str())
                .collect()
        })
    }
}

fn move_cursor(s: &mut SearchState, delta: isize, height: usize) {
    let last = s.rows.len().saturating_sub(1);
    s.cursor = s.cursor.saturating_add_signed(delta).min(last);
    let height = height.max(1);
    if s.cursor < s.offset {
        s.offset = s.cursor;
    } else if s.cursor >= s.offset + height {
        s.offset = s.cursor + 1 - height;
    }
}

/// Rows of the results table that fit (shared with the renderer).
fn table_height(state: &State) -> usize {
    render::table_rows(state.layout().body, state)
}

// ------------------------------------------------------------------ opening

/// Open the search view (once) and refresh the provider list.
pub fn open(state: &mut State) -> Vec<Cmd> {
    if !matches!(state.view(), View::Search) {
        state.views.push(View::Search);
    }
    let s = &mut state.search;
    s.focus = if s.run.is_some() {
        Focus::Results
    } else {
        Focus::Query
    };
    s.history_open = false;
    providers::fetch(state)
}

/// Whether the search view is open somewhere in the stack.
fn in_search(state: &State) -> bool {
    matches!(state.view(), View::Search)
}

/// The provider list arrived: maybe show the first-run prompt (D26).
pub fn on_providers_loaded(state: &mut State) {
    let none_enabled = !state.providers.list.iter().any(|p| p.enabled);
    if state.providers.loaded
        && none_enabled
        && in_search(state)
        && !state.search.first_run_shown
        && state.modal.is_none()
    {
        state.search.first_run_shown = true;
        state.modal = Some(Modal::Confirm(first_run_prompt()));
    }
    let enabled = state.providers.list.iter().filter(|p| p.enabled).count();
    state.search.sel_cursor = state.search.sel_cursor.min(enabled.saturating_sub(1));
}

/// The modal explaining that every provider is off by default.
pub fn first_run_prompt() -> ConfirmState {
    ConfirmState::run(
        "Choose search providers first?",
        vec![
            "No search site is switched on yet (they are all off until".into(),
            "you choose), so a search would find nothing.".into(),
            String::new(),
            "Yes opens the provider list: Space switches one on, Esc".into(),
            "comes back here to search.".into(),
        ],
        Action::SearchProviders,
    )
    .label("Choose providers")
}

// --------------------------------------------------------------------- keys

/// Raw keys: text entry and the provider selector.
pub fn handle_raw_key(state: &mut State, key: KeyEvent) -> Option<Vec<Cmd>> {
    if state.search.history_open {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let focus = state.search.focus;
    match (focus, key.code) {
        (Focus::Results, _) => None,
        (_, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_)) => None,
        (_, KeyCode::Enter) if !ctrl && !alt => Some(start(state)),
        (_, KeyCode::Up | KeyCode::Down) => {
            state.search.focus = focus.step(key.code == KeyCode::Down);
            Some(Vec::new())
        }
        (Focus::Providers, KeyCode::Left | KeyCode::Right) => {
            let n = providers::enabled(state).count();
            let s = &mut state.search;
            if key.code == KeyCode::Left {
                s.sel_cursor = s.sel_cursor.saturating_sub(1);
            } else {
                s.sel_cursor = (s.sel_cursor + 1).min(n.saturating_sub(1));
            }
            Some(Vec::new())
        }
        (Focus::Providers, KeyCode::Char(' ')) => {
            toggle_provider(state);
            Some(Vec::new())
        }
        (Focus::Providers, _) => None,
        (Focus::Query, _) => state.search.query.handle_key(key).then(Vec::new),
        (Focus::Category, _) => state.search.category.handle_key(key).then(Vec::new),
    }
}

fn toggle_provider(state: &mut State) {
    let Some(name) = providers::enabled(state)
        .nth(state.search.sel_cursor)
        .map(|p| p.name.clone())
    else {
        state
            .status
            .info("No provider is enabled; press p for the settings");
        return;
    };
    let off = &mut state.search.off;
    if !off.remove(&name) {
        off.insert(name);
    }
}

/// Bracketed paste.
pub fn on_paste(state: &mut State, text: &str) -> Vec<Cmd> {
    match state.search.focus {
        Focus::Query => state.search.query.insert_str(text),
        Focus::Category => state.search.category.insert_str(text),
        Focus::Providers | Focus::Results => state.dirty = false,
    }
    Vec::new()
}

/// Mouse wheel scrolls the results.
pub fn on_mouse(
    state: &mut State,
    m: crossterm::event::MouseEvent,
    hit: Option<(ratatui::layout::Rect, crate::layout::Hit)>,
) {
    use crate::layout::Hit;
    use crossterm::event::{MouseButton, MouseEventKind};
    let h = table_height(state);
    match (m.kind, hit) {
        (MouseEventKind::ScrollDown, _) => move_cursor(&mut state.search, 3, h),
        (MouseEventKind::ScrollUp, _) => move_cursor(&mut state.search, -3, h),
        (MouseEventKind::Down(MouseButton::Left), Some((_, Hit::SearchRow(i)))) => {
            let s = &mut state.search;
            s.focus = Focus::Results;
            let delta = isize::try_from(i).unwrap_or(0) - isize::try_from(s.cursor).unwrap_or(0);
            move_cursor(s, delta, h);
        }
        (MouseEventKind::Down(MouseButton::Left), Some((r, Hit::SearchScroll))) => {
            let s = &mut state.search;
            let n = s.rows.len();
            if n > 0 {
                let frac = f64::from(m.row.saturating_sub(r.y)) / f64::from(r.height.max(1));
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let i = ((frac * n as f64) as usize).min(n - 1);
                let delta =
                    isize::try_from(i).unwrap_or(0) - isize::try_from(s.cursor).unwrap_or(0);
                move_cursor(s, delta, h);
            }
        }
        _ => state.dirty = false,
    }
}

/// Apply a search-view action.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    let page = isize::try_from(table_height(state).max(1)).unwrap_or(1);
    if state.search.history_open
        && let Some(cmds) = apply_history(state, action, page)
    {
        return Some(cmds);
    }
    let h = table_height(state);
    let cmds = match action {
        Action::Up => {
            move_cursor(&mut state.search, -1, h);
            Vec::new()
        }
        Action::Down => {
            move_cursor(&mut state.search, 1, h);
            Vec::new()
        }
        Action::PageUp => {
            move_cursor(&mut state.search, -page, h);
            Vec::new()
        }
        Action::PageDown => {
            move_cursor(&mut state.search, page, h);
            Vec::new()
        }
        Action::Top => {
            move_cursor(&mut state.search, isize::MIN / 2, h);
            Vec::new()
        }
        Action::Bottom => {
            move_cursor(&mut state.search, isize::MAX / 2, h);
            Vec::new()
        }
        Action::ToggleSelect => {
            if let Some(k) = state.search.current().map(result_key)
                && !state.search.selected.remove(&k)
            {
                state.search.selected.insert(k);
            }
            move_cursor(&mut state.search, 1, h);
            Vec::new()
        }
        Action::SelectAll => {
            let s = &mut state.search;
            if let Some(run) = &s.run {
                let keys: Vec<String> = s
                    .rows
                    .iter()
                    .map(|i| result_key(&run.results[*i]))
                    .collect();
                s.selected.extend(keys);
            }
            Vec::new()
        }
        Action::SelectNone => {
            state.search.selected.clear();
            Vec::new()
        }
        Action::SortNext | Action::SortReverse => {
            let s = &mut state.search;
            if action == Action::SortNext {
                s.sort = s.sort.next();
            } else {
                s.reverse = !s.reverse;
            }
            let rev = if s.reverse { ", reversed" } else { "" };
            let text = format!("Sorted by {}{rev}", s.sort.label());
            s.recompute();
            state.status.info(text);
            Vec::new()
        }
        Action::Filter => prompt(
            state,
            PromptKind::SearchTitle,
            "Filter results by title",
            "Fuzzy title filter (empty clears):",
            &state.search.filters.title.clone(),
        ),
        Action::ClearFilter => {
            state.search.filters = Filters::default();
            state.search.recompute();
            state.status.info("Result filters cleared");
            Vec::new()
        }
        Action::SearchFilterSeeders => {
            let cur = state.search.filters.min_seeders;
            prompt(
                state,
                PromptKind::SearchSeeders,
                "Minimum seeders",
                "Hide results with fewer seeders (0 clears):",
                &cur.to_string(),
            )
        }
        Action::SearchFilterSize => {
            let cur = state.search.filters.size_text();
            prompt(
                state,
                PromptKind::SearchSize,
                "Size range",
                "e.g. 100MiB-4GiB, 1GiB- or -700MiB (empty clears):",
                &cur,
            )
        }
        Action::SearchFilterSource => {
            let cur = state.search.filters.source.clone();
            prompt(
                state,
                PromptKind::SearchSource,
                "Source provider",
                "Only results found by this provider (empty clears):",
                &cur,
            )
        }
        Action::SearchRun => start(state),
        Action::SearchEdit => {
            state.search.focus = Focus::Query;
            state.search.history_open = false;
            Vec::new()
        }
        Action::SearchNextField | Action::SearchPrevField => {
            let s = &mut state.search;
            s.focus = s.focus.step(action == Action::SearchNextField);
            Vec::new()
        }
        Action::SearchCancel => cancel_or_leave(state),
        Action::SearchAdd => begin_add(state, false),
        Action::SearchAddDirect => begin_add(state, true),
        Action::SearchHistory => {
            state.search.history_open = true;
            state.search.hist_cursor = 0;
            state.call_quiet(ApiCall::SearchHistory)
        }
        Action::SearchHistoryClear => confirm_clear_history(state),
        _ => return None,
    };
    Some(cmds)
}

fn prompt(state: &mut State, kind: PromptKind, title: &str, label: &str, value: &str) -> Vec<Cmd> {
    let value = if value == "0" { "" } else { value };
    state.modal = Some(Modal::Prompt(PromptState::new(kind, title, label, value)));
    Vec::new()
}

/// A filter prompt was submitted.
pub fn submit(state: &mut State, kind: PromptKind, value: &str) -> Vec<Cmd> {
    let f = &mut state.search.filters;
    let text = match kind {
        PromptKind::SearchTitle => {
            f.title = value.to_owned();
            if value.is_empty() {
                "Title filter cleared".to_owned()
            } else {
                format!("Title filter: {value}")
            }
        }
        PromptKind::SearchSource => {
            f.source = value.to_owned();
            if value.is_empty() {
                "Source filter cleared".to_owned()
            } else {
                format!("Only results from {value}")
            }
        }
        PromptKind::SearchSeeders => match if value.is_empty() {
            Ok(0)
        } else {
            value.parse::<u32>()
        } {
            Ok(n) => {
                f.min_seeders = n;
                format!("Minimum seeders: {n}")
            }
            Err(_) => {
                state
                    .status
                    .error(format!("Minimum seeders must be a number, got '{value}'"));
                return Vec::new();
            }
        },
        PromptKind::SearchSize => match filter::parse_size_range(value) {
            Ok((min, max)) => {
                f.size_min = min;
                f.size_max = max;
                match f.size_label() {
                    Some(l) => format!("Size filter: {l}"),
                    None => "Size filter cleared".to_owned(),
                }
            }
            Err(e) => {
                state.status.error(e);
                return Vec::new();
            }
        },
        _ => return Vec::new(),
    };
    state.search.recompute();
    state.status.info(text);
    Vec::new()
}

// ----------------------------------------------------------- start / cancel

/// Start a search with the current form.
pub fn start(state: &mut State) -> Vec<Cmd> {
    let query = state.search.query.text().trim().to_owned();
    if query.is_empty() {
        state.search.focus = Focus::Query;
        state
            .status
            .warn("Type what to search for first, then press Enter");
        return Vec::new();
    }
    if state.search.starting {
        state.status.info("Still starting the previous search");
        return Vec::new();
    }
    let enabled: Vec<String> = providers::enabled(state).map(|p| p.name.clone()).collect();
    let chosen: Vec<String> = enabled
        .iter()
        .filter(|n| !state.search.off.contains(*n))
        .cloned()
        .collect();
    if state.providers.loaded && enabled.is_empty() {
        let key = key_hint(state, Action::SearchProviders);
        state.status.warn(format!(
            "Search: no providers enabled — press {key} to choose providers"
        ));
        return Vec::new();
    }
    if !enabled.is_empty() && chosen.is_empty() {
        state.search.focus = Focus::Providers;
        state
            .status
            .warn("Search: every provider is switched off below — press Space on one to use it");
        return Vec::new();
    }
    let category = Some(state.search.category.text().trim().to_owned()).filter(|c| !c.is_empty());
    let params = SearchStartParams {
        query: query.clone(),
        providers: (!state.search.off.is_empty() && !enabled.is_empty()).then(|| chosen.clone()),
        category: category.clone(),
    };
    let mut cmds = Vec::new();
    let old = state
        .search
        .run
        .as_ref()
        .and_then(|r| r.id.filter(|_| !r.finished));
    let call = state.call(ApiCall::SearchStart(params));
    if call.is_empty() {
        return Vec::new();
    }
    if let Some(id) = old {
        cmds.extend(state.call_quiet(ApiCall::SearchCancel(id)));
    }
    cmds.extend(call);
    let s = &mut state.search;
    s.run = Some(Run {
        id: None,
        query,
        category,
        providers: chosen.into_iter().map(|n| (n, Prov::Running)).collect(),
        results: Vec::new(),
        finished: false,
        error: None,
        cancelling: false,
    });
    s.starting = true;
    s.early.clear();
    s.selected.clear();
    s.history_open = false;
    s.focus = Focus::Results;
    s.cursor = 0;
    s.offset = 0;
    s.recompute();
    cmds
}

fn cancel_or_leave(state: &mut State) -> Vec<Cmd> {
    if state.search.history_open {
        state.search.history_open = false;
        return Vec::new();
    }
    let running = state.search.run.as_ref().filter(|r| !r.finished);
    match running {
        Some(run) => match run.id {
            Some(id) if !run.cancelling => {
                let cmds = state.call(ApiCall::SearchCancel(id));
                if !cmds.is_empty()
                    && let Some(r) = &mut state.search.run
                {
                    r.cancelling = true;
                }
                cmds
            }
            Some(_) => {
                state.status.info("Already cancelling the search");
                Vec::new()
            }
            None => {
                state
                    .status
                    .info("The search is still starting; press Esc again in a moment");
                Vec::new()
            }
        },
        None => {
            state.views.pop();
            Vec::new()
        }
    }
}

// ------------------------------------------------------------------ events

/// `Event::SearchUpdate`.
pub fn on_update(
    state: &mut State,
    id: u64,
    provider: &str,
    status: ProviderStatus,
    results: Vec<SearchResultDto>,
) {
    let s = &mut state.search;
    let Some(run) = &mut s.run else { return };
    match run.id {
        Some(rid) if rid == id => {
            let keep = s
                .rows
                .get(s.cursor)
                .and_then(|i| run.results.get(*i))
                .map(result_key);
            run.set_provider(provider, status.into());
            run.results = results;
            s.recompute_keeping(keep);
        }
        None if s.starting => {
            if s.early.len() < MAX_EARLY {
                s.early.push(Early::Update {
                    id,
                    provider: provider.to_owned(),
                    status,
                    results,
                });
            }
        }
        _ => state.dirty = false,
    }
}

/// `Event::SearchFinished`.
pub fn on_finished(state: &mut State, id: u64) {
    let s = &mut state.search;
    let Some(run) = &mut s.run else { return };
    match run.id {
        Some(rid) if rid == id => {}
        None if s.starting => {
            s.early.push(Early::Finished(id));
            return;
        }
        _ => {
            state.dirty = false;
            return;
        }
    }
    finish(run);
    let text = summary(run);
    state.status.info(text);
}

fn finish(run: &mut Run) {
    run.finished = true;
    run.cancelling = false;
    for (_, p) in &mut run.providers {
        if *p == Prov::Running {
            *p = Prov::Cancelled;
        }
    }
}

/// `Search "q": 12 results from 3 providers, 1 failed`.
pub fn summary(run: &Run) -> String {
    let ok = run
        .providers
        .iter()
        .filter(|(_, p)| matches!(p, Prov::Done { .. }))
        .count();
    let mut text = format!(
        "Search '{}': {} from {}",
        run.query,
        plural(run.results.len(), "result"),
        plural(ok, "provider")
    );
    let failed = run.failed();
    if failed > 0 {
        text.push_str(&format!(", {failed} failed"));
    }
    text
}

/// The daemon missed us some events: re-read the running search.
pub fn on_lagged(state: &mut State) -> Vec<Cmd> {
    match state
        .search
        .run
        .as_ref()
        .filter(|r| !r.finished)
        .and_then(|r| r.id)
    {
        Some(id) => state.call_quiet(ApiCall::SearchResults(id)),
        None => Vec::new(),
    }
}

/// The connection dropped: what was in flight will never be answered.
pub fn on_disconnect(state: &mut State) {
    let s = &mut state.search;
    if s.starting {
        s.starting = false;
        if let Some(run) = &mut s.run {
            run.error = Some("lost the connection to the daemon".into());
            finish(run);
        }
    }
    if s.adding.take().is_some() {
        state
            .status
            .warn("Add cancelled: lost the connection to the daemon");
    }
    providers::on_disconnect(state);
}

/// Spinner tick; `true` while something on screen animates.
pub fn on_tick(state: &mut State) -> bool {
    let visible = matches!(state.view(), View::Search | View::Providers);
    let busy = state.search.running() || state.search.adding.is_some() || state.providers.busy();
    if visible && busy {
        state.search.spin = state.search.spin.wrapping_add(1);
    }
    visible && busy
}

// ----------------------------------------------------------------- replies

/// Handle the reply of a search call; `None` when it is not one of ours.
pub fn on_result(
    state: &mut State,
    call: &ApiCall,
    result: &Result<ApiReply, String>,
) -> Option<Vec<Cmd>> {
    match (call, result) {
        (ApiCall::SearchStart(_), Ok(ApiReply::SearchStarted(st))) => {
            on_started(state, st);
            Some(Vec::new())
        }
        (ApiCall::SearchStart(_), Err(e)) => {
            state.search.starting = false;
            if let Some(run) = &mut state.search.run {
                run.error = Some(e.clone());
                finish(run);
            }
            state.status.error(format!("Search failed: {e}"));
            Some(Vec::new())
        }
        (ApiCall::SearchCancel(_), Ok(_)) => {
            state.status.info("Cancelling the search…");
            Some(Vec::new())
        }
        (ApiCall::SearchCancel(_), Err(e)) => {
            if let Some(run) = &mut state.search.run {
                run.cancelling = false;
            }
            state.status.error(format!("Cancel failed: {e}"));
            Some(Vec::new())
        }
        (ApiCall::SearchResults(id), Ok(ApiReply::SearchSnapshot(snap))) => {
            on_snapshot(state, *id, snap);
            Some(Vec::new())
        }
        (ApiCall::SearchResults(_), Err(e)) => {
            state
                .status
                .warn(format!("Reload of the search failed: {e}"));
            Some(Vec::new())
        }
        (ApiCall::SearchResolve(p), r) => Some(on_resolved(state, p, r)),
        (ApiCall::SearchHistory, Ok(ApiReply::SearchHistory(h))) => {
            state.search.history = h.clone();
            state.search.hist_cursor = 0;
            Some(Vec::new())
        }
        (ApiCall::SearchHistory, Err(e)) => {
            state.search.history_open = false;
            state.status.error(format!("Search history failed: {e}"));
            Some(Vec::new())
        }
        (ApiCall::SearchHistoryClear, Ok(_)) => {
            state.search.history.clear();
            state.search.hist_cursor = 0;
            state.status.info("Search history cleared");
            Some(Vec::new())
        }
        (ApiCall::SearchHistoryClear, Err(e)) => {
            state.status.error(format!("Clear history failed: {e}"));
            Some(Vec::new())
        }
        _ => None,
    }
}

fn on_started(state: &mut State, started: &SearchStarted) {
    let s = &mut state.search;
    s.starting = false;
    let Some(run) = &mut s.run else { return };
    run.id = Some(started.search_id);
    if started.providers.is_empty() {
        finish(run);
        state
            .status
            .warn("No provider is enabled; open the providers settings (p or F2)");
        s.early.clear();
        return;
    }
    run.providers = started
        .providers
        .iter()
        .map(|n| (n.clone(), Prov::Running))
        .collect();
    // The provider chips and the status line show the progress.
    state.status.progress(format!(
        "Searching {}…",
        plural(started.providers.len(), "provider")
    ));
    let early = std::mem::take(&mut s.early);
    let id = started.search_id;
    let mut finished = false;
    if let Some(run) = &mut s.run {
        for e in early {
            match e {
                Early::Update {
                    id: eid,
                    provider,
                    status,
                    results,
                } if eid == id => {
                    run.set_provider(&provider, status.into());
                    run.results = results;
                }
                Early::Finished(eid) if eid == id => finished = true,
                _ => {}
            }
        }
        if finished {
            finish(run);
        }
    }
    s.recompute();
    if finished && let Some(run) = &s.run {
        let text = summary(run);
        state.status.info(text);
    }
}

fn on_snapshot(state: &mut State, id: u64, snap: &SearchSnapshot) {
    let s = &mut state.search;
    let Some(run) = &mut s.run else { return };
    if run.id != Some(id) || snap.search_id != id {
        return;
    }
    run.providers = snap
        .providers
        .iter()
        .map(|(n, p)| (n.clone(), p.clone().into()))
        .collect();
    let keep = s
        .rows
        .get(s.cursor)
        .and_then(|i| run.results.get(*i))
        .map(result_key);
    run.results = snap.results.clone();
    if snap.finished {
        finish(run);
    }
    s.recompute_keeping(keep);
}

// ----------------------------------------------------------------- history

fn apply_history(state: &mut State, action: Action, page: isize) -> Option<Vec<Cmd>> {
    let s = &mut state.search;
    let last = s.history.len().saturating_sub(1);
    match action {
        Action::Up => s.hist_cursor = s.hist_cursor.saturating_sub(1),
        Action::Down => s.hist_cursor = (s.hist_cursor + 1).min(last),
        Action::PageUp => s.hist_cursor = s.hist_cursor.saturating_add_signed(-page),
        Action::PageDown => s.hist_cursor = (s.hist_cursor.saturating_add_signed(page)).min(last),
        Action::Top => s.hist_cursor = 0,
        Action::Bottom => s.hist_cursor = last,
        Action::SearchHistory => s.history_open = false,
        Action::SearchAddDirect | Action::SearchRun => return Some(rerun(state)),
        // Everything else (sort, filter, add, …) is about the results below.
        Action::SearchAdd
        | Action::ToggleSelect
        | Action::SelectAll
        | Action::SortNext
        | Action::SortReverse => {
            state
                .status
                .info("Close the history (h or Esc) to work on the results");
        }
        _ => return None,
    }
    Some(Vec::new())
}

fn rerun(state: &mut State) -> Vec<Cmd> {
    let Some(e) = state.search.history.get(state.search.hist_cursor).cloned() else {
        state.status.info("No search history yet");
        return Vec::new();
    };
    let enabled: Vec<String> = providers::enabled(state).map(|p| p.name.clone()).collect();
    let s = &mut state.search;
    s.query = LineInput::new(e.query);
    s.category = LineInput::new(e.category.unwrap_or_default());
    s.off = if e.providers.is_empty() {
        BTreeSet::new()
    } else {
        enabled
            .into_iter()
            .filter(|n| !e.providers.iter().any(|p| p.eq_ignore_ascii_case(n)))
            .collect()
    };
    s.history_open = false;
    start(state)
}

fn confirm_clear_history(state: &mut State) -> Vec<Cmd> {
    state.modal = Some(Modal::Confirm(
        ConfirmState::yes_no(
            "Clear the search history?",
            vec!["Removes every remembered query. Downloads are not affected.".into()],
            ApiCall::SearchHistoryClear,
        )
        .label("Clear"),
    ));
    Vec::new()
}

// ------------------------------------------------------------------ adding

fn begin_add(state: &mut State, direct: bool) -> Vec<Cmd> {
    if state.search.history_open {
        return rerun(state);
    }
    if state.search.adding.is_some() {
        state
            .status
            .info("Still resolving the previous selection; wait a moment");
        return Vec::new();
    }
    let targets = state.search.targets();
    if targets.is_empty() {
        let hidden = state
            .search
            .run
            .as_ref()
            .is_some_and(|r| !r.results.is_empty());
        state.status.info(if hidden {
            "Nothing to download: the filters hide every result (Ctrl-l clears them)"
        } else {
            "No result to download yet — run a search first (type, then Enter)"
        });
        return Vec::new();
    }
    let mut cmds = Vec::new();
    let mut items = Vec::new();
    for r in &targets {
        let fstate = if let Some(m) = &r.magnet {
            FlowState::Ready(m.clone())
        } else if let Some(url) = &r.detail_url {
            let params = SearchResolveParams {
                detail_url: Some(url.clone()),
                provider: r.sources.first().cloned(),
                ..SearchResolveParams::default()
            };
            let c = state.call_quiet(ApiCall::SearchResolve(params));
            if c.is_empty() {
                FlowState::Failed("not connected to the daemon".into())
            } else {
                cmds.extend(c);
                FlowState::Resolving(url.clone())
            }
        } else if let Some(url) = &r.torrent_url {
            FlowState::Ready(url.clone())
        } else {
            FlowState::Failed("the result has no magnet link or page".into())
        };
        items.push(FlowItem {
            key: result_key(r),
            title: r.title.clone(),
            state: fstate,
        });
    }
    let flow = AddFlow { direct, items };
    let resolving = flow.resolving();
    state.search.adding = Some(flow);
    if resolving > 0 {
        state
            .status
            .info(format!("Resolving {}…", plural(resolving, "magnet link")));
        cmds
    } else {
        finish_add(state)
    }
}

fn on_resolved(
    state: &mut State,
    params: &SearchResolveParams,
    result: &Result<ApiReply, String>,
) -> Vec<Cmd> {
    let Some(flow) = &mut state.search.adding else {
        return Vec::new();
    };
    let Some(url) = &params.detail_url else {
        return Vec::new();
    };
    let item = flow
        .items
        .iter_mut()
        .find(|i| matches!(&i.state, FlowState::Resolving(u) if u == url));
    if let Some(item) = item {
        item.state = match result {
            Ok(ApiReply::Resolved(m)) => FlowState::Ready(m.magnet.clone()),
            Ok(_) => FlowState::Failed("unexpected reply".into()),
            Err(e) => FlowState::Failed(e.clone()),
        };
    }
    finish_add(state)
}

/// When nothing is left to resolve: open the Add view or add directly.
fn finish_add(state: &mut State) -> Vec<Cmd> {
    if state
        .search
        .adding
        .as_ref()
        .is_none_or(|f| f.resolving() > 0)
    {
        return Vec::new();
    }
    let Some(flow) = state.search.adding.take() else {
        return Vec::new();
    };
    let mut ready = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for i in &flow.items {
        match &i.state {
            FlowState::Ready(u) => ready.push(u.clone()),
            FlowState::Failed(e) => {
                failed.push(format!("{}: {e}", crate::format::truncate(&i.title, 30)))
            }
            FlowState::Resolving(_) => {}
        }
    }
    let total = flow.items.len();
    if ready.is_empty() {
        let first = failed.first().cloned().unwrap_or_default();
        state.status.error(format!("Cannot add: {first}"));
        return Vec::new();
    }
    state.search.selected.clear();
    let cmds = if flow.direct {
        let params = AddParams {
            sources: ready
                .iter()
                .map(|u| AddSource::Uris {
                    uris: vec![u.clone()],
                })
                .collect(),
            options: JobOptions::default(),
            paused: false,
        };
        state.call(ApiCall::Add(Box::new(params)))
    } else {
        let cmds = crate::views::add::push(state, AddState::with_text(&ready.join("\n")));
        state.status.info(format!(
            "{} ready to add: change options if you like, then press Enter",
            plural(ready.len(), "link")
        ));
        cmds
    };
    if let Some(first) = failed.first() {
        state.status.warn(format!(
            "{} of {total} could not be resolved ({first})",
            failed.len()
        ));
    }
    cmds
}

/// The key to show for `action`: its first binding, or (while a text field
/// has the keyboard, where plain letters are text) its last, the F-key.
pub fn key_hint(state: &State, action: Action) -> String {
    use crate::keymap::{Context, key_label};
    let keys = state.keymap.keys(Context::Search, action);
    let k = if state.search.focus.is_text() {
        keys.last()
    } else {
        keys.first()
    };
    k.map_or_else(String::new, |k| key_label(*k))
}

/// Footer hints that depend on the focus (text entry, selector, history,
/// results).
pub fn footer_hints(state: &State) -> Vec<(String, String)> {
    use crate::widgets::footer::{hint, own};
    let s = &state.search;
    let h = |a: Action, label: &str| hint(state, a, label);
    let v: Vec<Option<(String, String)>> = if s.history_open {
        vec![
            Some(own("Enter", "search again")),
            Some(own("↑↓", "choose")),
            h(Action::SearchHistoryClear, "clear history"),
            h(Action::SearchCancel, "close"),
        ]
    } else {
        match s.focus {
            Focus::Query | Focus::Category => vec![
                Some(own("Enter", "search")),
                h(Action::SearchNextField, "next field"),
                h(Action::SearchProviders, "providers"),
                h(Action::SearchCancel, "back"),
            ],
            Focus::Providers => vec![
                Some(own("Space", "switch on/off")),
                Some(own("←→", "move")),
                Some(own("Enter", "search")),
                h(Action::SearchNextField, "next field"),
                h(Action::SearchCancel, "back"),
            ],
            Focus::Results if s.rows.is_empty() => vec![
                h(Action::SearchEdit, "new search"),
                h(Action::SearchProviders, "providers"),
                h(Action::SearchHistory, "history"),
                h(
                    Action::SearchCancel,
                    if s.running() { "cancel" } else { "back" },
                ),
            ],
            Focus::Results => {
                let marked = s.selected.len();
                let download = if marked > 0 {
                    format!("download {marked} marked")
                } else {
                    "download".to_owned()
                };
                vec![
                    h(Action::SearchAddDirect, &download),
                    h(Action::SearchAdd, "change options first"),
                    h(Action::ToggleSelect, "mark"),
                    h(Action::SearchEdit, "new search"),
                    h(Action::Filter, "filter"),
                    h(Action::SortNext, "sort"),
                    h(
                        Action::SearchCancel,
                        if s.running() { "cancel" } else { "back" },
                    ),
                ]
            }
        }
    };
    v.into_iter().flatten().collect()
}
