//! Command palette: every [`Action`], grouped into tabs ([`Group`]).
//!
//! * The "All" tab lists recently used commands, then every group under a
//!   heading; the other tabs list one group.
//! * Tab / Shift-Tab (and ← / → while the query is empty) switch tabs.
//! * Typing searches every group at once (fuzzy, matches highlighted); the
//!   tab bar then shows how many matches each group has, and Tab narrows the
//!   results to one group.
//! * Commands that cannot run right now (offline, not in this view, nothing
//!   selected) stay listed, dimmed, with the reason.
//! * Esc clears the query first, then closes; Enter runs.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::action::{Action, Group};
use crate::app::State;
use crate::keymap::Context;
use crate::views::View;
use crate::widgets::ModalOutcome;
use crate::widgets::input::LineInput;

mod render;

/// Recently run commands remembered for the "All" tab.
pub const RECENT_MAX: usize = 5;

/// A row of the palette list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A section heading (the "All" tab without a query).
    Header(&'static str),
    /// A command; `hits` are the matched character positions of its name.
    Item {
        /// The command.
        action: Action,
        /// Matched characters of the name (highlighted).
        hits: Vec<u32>,
    },
}

impl Entry {
    /// The command of an item row.
    pub fn action(&self) -> Option<Action> {
        match self {
            Entry::Item { action, .. } => Some(*action),
            Entry::Header(_) => None,
        }
    }
}

/// Palette state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteState {
    /// Query.
    pub input: LineInput,
    /// Selected tab: 0 = All, `n` = `Group::ALL[n - 1]`.
    pub tab: usize,
    /// Rows shown for the query and tab.
    pub entries: Vec<Entry>,
    /// Highlighted row (always a command when there is one).
    pub selected: usize,
    /// Matches per tab for the current query (index as [`Self::tab`]).
    pub counts: Vec<usize>,
    /// Recently run commands, newest first.
    pub recent: Vec<Action>,
    /// Commands that cannot run now, with the reason.
    pub disabled: Vec<(Action, String)>,
    /// Shown under the list (e.g. why the chosen command cannot run).
    pub notice: Option<String>,
}

impl Default for PaletteState {
    /// A palette with nothing disabled and no history (tests, previews).
    fn default() -> Self {
        Self::with(Vec::new(), Vec::new())
    }
}

impl PaletteState {
    /// The palette for the current state: recent commands and which commands
    /// cannot run now.
    pub fn new(state: &State) -> Self {
        Self::with(state.recent_actions.clone(), unavailable(state))
    }

    fn with(recent: Vec<Action>, disabled: Vec<(Action, String)>) -> Self {
        let mut s = Self {
            input: LineInput::default(),
            tab: 0,
            entries: Vec::new(),
            selected: 0,
            counts: Vec::new(),
            recent,
            disabled,
            notice: None,
        };
        s.refilter();
        s
    }

    /// Number of tabs (All plus the groups).
    pub fn tabs() -> usize {
        Group::ALL.len() + 1
    }

    /// The label of tab `i`.
    pub fn tab_label(i: usize) -> &'static str {
        match i.checked_sub(1) {
            None => "All",
            Some(g) => Group::ALL.get(g).map_or("", |g| g.label()),
        }
    }

    fn tab_short(i: usize) -> &'static str {
        match i.checked_sub(1) {
            None => "All",
            Some(g) => Group::ALL.get(g).map_or("", |g| g.short()),
        }
    }

    fn tab_group(&self) -> Option<Group> {
        self.tab
            .checked_sub(1)
            .and_then(|g| Group::ALL.get(g).copied())
    }

    /// Why `a` cannot run now, if it cannot.
    pub fn reason(&self, a: Action) -> Option<&str> {
        self.disabled
            .iter()
            .find(|(d, _)| *d == a)
            .map(|(_, r)| r.as_str())
    }

    /// The commands shown, in order (without headings).
    pub fn items(&self) -> impl Iterator<Item = Action> + '_ {
        self.entries.iter().filter_map(Entry::action)
    }

    /// The highlighted command.
    pub fn current(&self) -> Option<Action> {
        self.entries.get(self.selected).and_then(Entry::action)
    }

    /// Recompute the rows from the query and the tab.
    pub fn refilter(&mut self) {
        let query = self.input.text().trim().to_owned();
        self.entries.clear();
        self.counts = vec![0; Self::tabs()];
        if query.is_empty() {
            self.counts[0] = Action::ALL.len();
            for (i, g) in Group::ALL.iter().enumerate() {
                self.counts[i + 1] = g.actions().count();
            }
            let item = |action| Entry::Item {
                action,
                hits: Vec::new(),
            };
            match self.tab_group() {
                Some(g) => self.entries.extend(g.actions().map(item)),
                None => {
                    if !self.recent.is_empty() {
                        self.entries.push(Entry::Header("Recent"));
                        self.entries.extend(self.recent.iter().copied().map(item));
                    }
                    for g in Group::ALL {
                        self.entries.push(Entry::Header(g.label()));
                        self.entries.extend(g.actions().map(item));
                    }
                }
            }
        } else {
            let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
            let mut matcher = Matcher::new(Config::DEFAULT);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize, Action, Vec<u32>)> = Vec::new();
            for (i, a) in Action::ALL.iter().enumerate() {
                let (name, desc) = a.palette_text();
                // The name counts most; description, group and config id
                // still find a command by what it does.
                let hay = format!(
                    "{name} {} {} {}",
                    desc.as_deref().unwrap_or(""),
                    a.group().label(),
                    a.id()
                );
                let Some(score) = pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher) else {
                    continue;
                };
                let bonus = pattern
                    .score(Utf32Str::new(&name, &mut buf), &mut matcher)
                    .map_or(0, |s| s.saturating_mul(2));
                let mut hits = Vec::new();
                let _ = pattern.indices(Utf32Str::new(&name, &mut buf), &mut matcher, &mut hits);
                hits.sort_unstable();
                hits.dedup();
                scored.push((score.saturating_add(bonus), i, *a, hits));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.counts[0] = scored.len();
            for (_, _, a, _) in &scored {
                if let Some(g) = Group::ALL.iter().position(|g| *g == a.group()) {
                    self.counts[g + 1] += 1;
                }
            }
            let group = self.tab_group();
            self.entries.extend(
                scored
                    .into_iter()
                    .filter(|(_, _, a, _)| group.is_none_or(|g| a.group() == g))
                    .map(|(_, _, action, hits)| Entry::Item { action, hits }),
            );
        }
        self.selected = self
            .entries
            .iter()
            .position(|e| e.action().is_some())
            .unwrap_or(0);
    }

    /// Switch to the tab `delta` away (wrapping).
    pub fn switch_tab(&mut self, delta: isize) {
        let n = Self::tabs();
        self.tab = (self.tab + n).saturating_add_signed(delta) % n;
        self.notice = None;
        self.refilter();
    }

    /// Show tab `i`.
    pub fn set_tab(&mut self, i: usize) {
        if i < Self::tabs() {
            self.tab = i;
            self.notice = None;
            self.refilter();
        }
    }

    /// Handle a key; running a command records it as recent in `state`.
    pub fn handle_key(&mut self, key: KeyEvent, state: &mut State) -> ModalOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let typing = !self.input.text().is_empty();
        match key.code {
            KeyCode::Esc if typing => {
                self.input = LineInput::default();
                self.notice = None;
                self.refilter();
                ModalOutcome::Stay
            }
            KeyCode::Esc => ModalOutcome::Close,
            KeyCode::Enter => self.run(state),
            KeyCode::Up => self.step(-1),
            KeyCode::Char('p' | 'k') if ctrl => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::Char('n' | 'j') if ctrl => self.step(1),
            KeyCode::PageUp => self.step(-10),
            KeyCode::PageDown => self.step(10),
            KeyCode::Home if !typing => self.step(isize::MIN / 2),
            KeyCode::End if !typing => self.step(isize::MAX / 2),
            KeyCode::Tab => {
                self.switch_tab(1);
                ModalOutcome::Stay
            }
            KeyCode::BackTab => {
                self.switch_tab(-1);
                ModalOutcome::Stay
            }
            KeyCode::Right if !typing => {
                self.switch_tab(1);
                ModalOutcome::Stay
            }
            KeyCode::Left if !typing => {
                self.switch_tab(-1);
                ModalOutcome::Stay
            }
            _ => {
                let before = self.input.text().to_owned();
                if self.input.handle_key(key) && self.input.text() != before {
                    if before.trim().is_empty() {
                        // A new search looks through every group.
                        self.tab = 0;
                    }
                    self.notice = None;
                    self.refilter();
                }
                ModalOutcome::Stay
            }
        }
    }

    /// Run the highlighted command, or say why it cannot run.
    pub fn run(&mut self, state: &mut State) -> ModalOutcome {
        let Some(a) = self.current() else {
            return ModalOutcome::Stay;
        };
        if let Some(r) = self.reason(a) {
            self.notice = Some(format!("{}: {r}", a.palette_text().0));
            return ModalOutcome::Stay;
        }
        remember(state, a);
        ModalOutcome::Run(a)
    }

    /// Insert pasted text into the query.
    pub fn paste(&mut self, text: &str) {
        if self.input.text().trim().is_empty() {
            self.tab = 0;
        }
        self.input.insert_str(text);
        self.refilter();
    }

    /// Move the highlight by `delta` commands (clamped; headings skipped).
    pub fn step(&mut self, delta: isize) -> ModalOutcome {
        let items: Vec<usize> = (0..self.entries.len())
            .filter(|i| self.entries[*i].action().is_some())
            .collect();
        if let Some(pos) = items.iter().position(|i| *i == self.selected) {
            let to = pos.saturating_add_signed(delta).min(items.len() - 1);
            self.selected = items[to];
        } else if let Some(first) = items.first() {
            self.selected = *first;
        }
        self.notice = None;
        ModalOutcome::Stay
    }

    /// Footer hints.
    pub fn hints() -> Vec<(String, String)> {
        vec![
            ("Enter".into(), "run".into()),
            ("Esc".into(), "close".into()),
            ("Tab".into(), "next group".into()),
            ("↑↓".into(), "choose".into()),
        ]
    }
}

/// Record `a` as the most recently run command.
pub fn remember(state: &mut State, a: Action) {
    if matches!(a, Action::Palette) {
        return;
    }
    state.recent_actions.retain(|r| *r != a);
    state.recent_actions.insert(0, a);
    state.recent_actions.truncate(RECENT_MAX);
}

/// Commands that cannot run in the current state, with the reason. The
/// view-specific ones are found by asking the view itself on a copy of the
/// state ([`crate::views::apply_action`] answers `None` for actions it does
/// not handle), so this never disagrees with what a key press would do.
pub fn unavailable(state: &State) -> Vec<(Action, String)> {
    let offline = state.offline_reason();
    let downloads = matches!(state.view(), View::Downloads);
    let no_target = state.list.cursor.is_none() && state.list.selected.is_empty();
    let mut out = Vec::new();
    for &a in Action::ALL {
        if let (true, Some(r)) = (a.needs_connection(), &offline) {
            out.push((a, format!("offline ({r})")));
            continue;
        }
        let reason = match a {
            // Handled by the app in every view.
            Action::Quit
            | Action::Palette
            | Action::Help
            | Action::Add
            | Action::Shutdown
            | Action::VideoQualityToggle
            | Action::VideoDefaultQuality
            | Action::VideoDefaultAudio
            | Action::Search
            | Action::ToggleMouse
            | Action::Undo
            | Action::SearchProviders => None,
            Action::Back if state.views.len() <= 1 => Some("already in the downloads list".into()),
            Action::Back => None,
            Action::Filter if !matches!(state.view(), View::Search) => None,
            // Add-view commands open the add view first.
            _ if a.home_context() == Context::Add => None,
            Action::Pause
            | Action::Resume
            | Action::Remove
            | Action::RemoveDelete
            | Action::Rename
            | Action::CleanupUnselected
            | Action::OpenDetail
            | Action::MoveUp
            | Action::MoveDown
            | Action::MoveTop
            | Action::MoveBottom
                if downloads && no_target =>
            {
                Some("no download selected".into())
            }
            _ => {
                let mut copy = state.clone();
                crate::views::apply_action(&mut copy, a)
                    .is_none()
                    .then(|| where_it_works(a))
            }
        };
        if let Some(r) = reason {
            out.push((a, r));
        }
    }
    out
}

/// Where a view-specific command can be used.
fn where_it_works(a: Action) -> String {
    match a.home_context() {
        Context::Detail => "open a download's details first (Enter)".into(),
        Context::Search => "only in the Search view".into(),
        Context::Providers => "only in the Search providers view".into(),
        Context::Add => "only in the Add view".into(),
        Context::Help => "only on the help screen".into(),
        Context::Downloads | Context::Global => "only in the downloads list".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn state() -> State {
        State::new(crate::app::StateOptions::default())
    }

    #[test]
    fn all_tab_lists_every_action_once_under_its_group() {
        let p = PaletteState::default();
        let items: Vec<Action> = p.items().collect();
        assert_eq!(items.len(), Action::ALL.len());
        for a in Action::ALL {
            assert!(items.contains(a), "{a:?} missing from palette");
        }
        let headers = p
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Header(_)))
            .count();
        assert_eq!(headers, Group::ALL.len());
        assert_eq!(p.current(), Group::ALL[0].actions().next());
    }

    #[test]
    fn each_group_tab_lists_exactly_its_actions() {
        let mut p = PaletteState::default();
        let mut seen = 0;
        for (i, g) in Group::ALL.iter().enumerate() {
            p.set_tab(i + 1);
            let items: Vec<Action> = p.items().collect();
            assert!(items.iter().all(|a| a.group() == *g));
            assert_eq!(items.len(), g.actions().count());
            seen += items.len();
        }
        assert_eq!(seen, Action::ALL.len());
    }

    #[test]
    fn search_spans_every_group_and_tab_narrows_it() {
        let mut s = state();
        let mut p = PaletteState::default();
        p.set_tab(3);
        p.paste("pause all");
        assert_eq!(p.tab, 0, "a new search looks everywhere");
        assert_eq!(p.current(), Some(Action::PauseAll));
        assert_eq!(
            p.handle_key(k(KeyCode::Enter), &mut s),
            ModalOutcome::Run(Action::PauseAll)
        );
        assert_eq!(s.recent_actions, vec![Action::PauseAll]);
        // Tab narrows the results to a group; the counts say where matches are.
        p.handle_key(k(KeyCode::Tab), &mut s);
        assert_eq!(p.tab, 1);
        assert!(p.items().all(|a| a.group() == Group::Downloads));
        assert_eq!(p.counts[1], p.items().count());
        // ← / → move the cursor in the query instead of switching tabs.
        p.handle_key(k(KeyCode::Left), &mut s);
        assert_eq!(p.tab, 1);
    }

    #[test]
    fn esc_clears_the_query_then_closes() {
        let mut s = state();
        let mut p = PaletteState::default();
        p.paste("zzzz");
        assert_eq!(p.items().count(), 0);
        assert_eq!(p.handle_key(k(KeyCode::Esc), &mut s), ModalOutcome::Stay);
        assert_eq!(p.input.text(), "");
        assert_eq!(p.items().count(), Action::ALL.len());
        assert_eq!(p.handle_key(k(KeyCode::Esc), &mut s), ModalOutcome::Close);
    }

    #[test]
    fn arrows_switch_tabs_when_the_query_is_empty_and_wrap() {
        let mut s = state();
        let mut p = PaletteState::default();
        p.handle_key(k(KeyCode::Left), &mut s);
        assert_eq!(p.tab, PaletteState::tabs() - 1);
        p.handle_key(k(KeyCode::Right), &mut s);
        assert_eq!(p.tab, 0);
        p.handle_key(k(KeyCode::BackTab), &mut s);
        assert_eq!(p.tab, PaletteState::tabs() - 1);
    }

    #[test]
    fn navigation_skips_headings_and_clamps() {
        let mut s = state();
        let mut p = PaletteState::default();
        let first = p.selected;
        p.handle_key(k(KeyCode::Up), &mut s);
        assert_eq!(p.selected, first);
        let group_len = Group::ALL[0].actions().count();
        for _ in 0..group_len {
            p.handle_key(k(KeyCode::Down), &mut s);
        }
        // Past the first group's heading of the second group.
        assert_eq!(p.current(), Group::ALL[1].actions().next());
        p.handle_key(k(KeyCode::End), &mut s);
        assert_eq!(
            p.current(),
            Group::ALL.last().and_then(|g| g.actions().last())
        );
    }

    #[test]
    fn recent_commands_come_first_in_all() {
        let mut s = state();
        remember(&mut s, Action::Search);
        remember(&mut s, Action::PauseAll);
        remember(&mut s, Action::Search);
        assert_eq!(s.recent_actions, vec![Action::Search, Action::PauseAll]);
        let p = PaletteState::new(&s);
        assert_eq!(p.entries[0], Entry::Header("Recent"));
        assert_eq!(p.current(), Some(Action::Search));
    }

    #[test]
    fn unavailable_commands_stay_listed_with_a_reason() {
        let mut s = state();
        // Not connected yet: daemon commands are off, local ones are not.
        let p = PaletteState::new(&s);
        assert!(
            p.reason(Action::PauseAll)
                .is_some_and(|r| r.contains("offline"))
        );
        assert!(p.reason(Action::Help).is_none());
        assert!(
            p.reason(Action::DetailFiles)
                .is_some_and(|r| r.contains("details"))
        );
        assert_eq!(p.items().count(), Action::ALL.len());
        // Enter explains instead of running.
        let mut p = p;
        p.paste("pause all");
        assert_eq!(p.handle_key(k(KeyCode::Enter), &mut s), ModalOutcome::Stay);
        assert!(p.notice.as_deref().is_some_and(|n| n.contains("offline")));
        assert!(s.recent_actions.is_empty());
        // Connecting while it is open enables them.
        s.modal = Some(crate::widgets::Modal::Palette(p));
        crate::app::update(
            &mut s,
            crate::app::Msg::Conn(dman_client::ConnState::Connected {
                daemon_version: "1".into(),
            }),
        );
        let Some(crate::widgets::Modal::Palette(p)) = &s.modal else {
            panic!("palette closed");
        };
        assert!(p.reason(Action::PauseAll).is_none());
    }
}
