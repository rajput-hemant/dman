//! Modals (palette, confirmations, prompts, filter input) and shared chrome
//! (header, status line, footer).
//!
//! # Adding a modal
//! 1. Write `widgets/<name>.rs` with a state struct offering
//!    `handle_key(&mut self, KeyEvent) -> ModalOutcome`, `hints()` and
//!    `render(&self, &mut Frame, Rect, &Theme)`.
//! 2. Add a [`Modal`] variant and dispatch to it in the `match`es below.
//! 3. If it produces a value, add a [`prompt::PromptKind`]-like variant to
//!    [`ModalOutcome`] (or reuse `Api`/`Run`) and handle it in
//!    [`crate::app::on_modal_outcome`].
//!
//! Modals are drawn after the view, so live data keeps rendering underneath.

pub mod confirm;
pub mod editor;
pub mod footer;
pub mod graph;
pub mod input;
pub mod palette;
pub mod prompt;
pub mod status;
pub mod welcome;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::action::Action;
use crate::api::ApiCall;
use crate::app::State;
use confirm::ConfirmState;
use input::LineInput;
use palette::PaletteState;
use prompt::{PromptKind, PromptState};

/// What a modal wants after handling a key.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalOutcome {
    /// Keep the modal open.
    Stay,
    /// Close the modal.
    Close,
    /// Close and run an action in the top view.
    Run(Action),
    /// Close and call the daemon.
    Api(ApiCall),
    /// Close and leave the top view (confirmed discard of a form).
    LeaveView,
    /// Replace the modal with another (e.g. the second confirmation).
    Replace(Modal),
    /// Close and hand a prompt value to `update`.
    Submit(PromptKind, String),
}

/// Live editing of the downloads filter (no popup; shown in the info bar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterState {
    /// Filter before editing started; restored on Esc.
    pub previous: String,
    /// The edited filter.
    pub input: LineInput,
}

impl FilterState {
    /// Start editing `current`.
    pub fn new(current: &str) -> Self {
        Self {
            previous: current.to_owned(),
            input: LineInput::new(current),
        }
    }

    fn handle_key(&mut self, key: KeyEvent, state: &mut State) -> ModalOutcome {
        match key.code {
            KeyCode::Enter => return ModalOutcome::Close,
            KeyCode::Esc => {
                self.input = LineInput::new(self.previous.clone());
                self.apply(state);
                return ModalOutcome::Close;
            }
            KeyCode::Up | KeyCode::Down => {
                state
                    .list
                    .move_by(if key.code == KeyCode::Up { -1 } else { 1 });
                let h = state.list_height();
                state.list.ensure_visible(h);
                return ModalOutcome::Stay;
            }
            _ => {}
        }
        if self.input.handle_key(key) {
            self.apply(state);
        }
        ModalOutcome::Stay
    }

    fn apply(&self, state: &mut State) {
        if state.list.filter != self.input.text() {
            state.list.filter = self.input.text().to_owned();
            state.list.offset = 0;
            state.refresh_list();
        }
    }
}

/// A dialog or input layer drawn over the current view.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Command palette.
    Palette(PaletteState),
    /// Confirmation dialog.
    Confirm(ConfirmState),
    /// Value prompt.
    Prompt(PromptState),
    /// Filter editing.
    Filter(FilterState),
    /// First-run welcome.
    Welcome(welcome::WelcomeState),
}

impl Modal {
    /// Handle a key. `state` is the app state without this modal.
    pub fn handle_key(&mut self, key: KeyEvent, state: &mut State) -> ModalOutcome {
        match self {
            Modal::Palette(p) => p.handle_key(key),
            Modal::Confirm(c) => c.handle_key(key),
            Modal::Prompt(p) => p.handle_key(key),
            Modal::Filter(f) => f.handle_key(key, state),
            Modal::Welcome(w) => w.handle_key(key, state),
        }
    }

    /// Handle bracketed paste.
    pub fn paste(&mut self, text: &str, state: &mut State) {
        match self {
            Modal::Palette(p) => p.paste(text),
            Modal::Confirm(c) => c.paste(text),
            Modal::Prompt(p) => p.input.insert_str(text),
            Modal::Filter(f) => {
                f.input.insert_str(text);
                f.apply(state);
            }
            Modal::Welcome(_) => {}
        }
    }

    /// Footer hints as (keys, label).
    pub fn hints(&self, keymap: &crate::keymap::Keymap) -> Vec<(String, String)> {
        match self {
            Modal::Welcome(_) => welcome::WelcomeState::hints(keymap),
            Modal::Palette(_) => PaletteState::hints(),
            Modal::Confirm(c) => c.hints(),
            Modal::Prompt(_) => PromptState::hints(),
            Modal::Filter(_) => vec![
                ("Enter".into(), "keep filter".into()),
                ("Esc".into(), "undo".into()),
                ("↑↓".into(), "move".into()),
            ],
        }
    }

    /// Draw the modal over `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, state: &State) {
        match self {
            Modal::Palette(p) => p.render(frame, area, &state.theme, &state.keymap),
            Modal::Confirm(c) => c.render(frame, area, &state.theme),
            Modal::Prompt(p) => p.render(frame, area, &state.theme),
            // Drawn inline by the info bar.
            Modal::Filter(_) => {}
            Modal::Welcome(w) => w.render(frame, area, &state.theme, &state.keymap),
        }
    }
}

/// A `width` × `height` rectangle centred in `area` (clamped to it).
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}
