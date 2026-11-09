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

pub mod bar;
pub mod confirm;
pub mod editor;
pub mod footer;
pub mod graph;
pub mod input;
pub mod palette;
pub mod prompt;
pub mod status;
pub mod toast;
pub mod welcome;
pub mod whichkey;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::action::Action;
use crate::api::ApiCall;
use crate::app::State;
use crate::theme::{ColorDepth, Theme};
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
            Modal::Confirm(_) => {}
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
            Modal::Palette(p) => {
                p.render(frame, area, state);
            }
            Modal::Confirm(c) => c.render(frame, area, state),
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

/// Dim everything outside `keep` (the backdrop of a modal).
pub fn dim_backdrop(frame: &mut Frame<'_>, keep: Rect, theme: &Theme) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    // The footer stays readable: it shows the dialog's keys.
    for y in area.top()..area.bottom().saturating_sub(1) {
        for x in area.left()..area.right() {
            let inside = x >= keep.x && x < keep.right() && y >= keep.y && y < keep.bottom();
            if !inside {
                buf[(x, y)].set_style(theme.backdrop);
            }
        }
    }
}

/// A one-cell drop shadow right of and below `rect`.
pub fn shadow(frame: &mut Frame<'_>, rect: Rect, theme: &Theme) {
    if theme.depth == ColorDepth::Mono || theme.shadow == Style::new() {
        return;
    }
    let area = frame.area();
    let buf = frame.buffer_mut();
    let right = rect.right();
    let bottom = rect.bottom();
    for y in rect.y + 1..=bottom {
        if right < area.right() && y < area.bottom() {
            buf[(right, y)].set_style(theme.shadow);
        }
    }
    for x in rect.x + 1..=right {
        if x < area.right() && bottom < area.bottom() {
            buf[(x, bottom)].set_style(theme.shadow);
        }
    }
}

/// Draw the frame of a modal at `rect`: dimmed backdrop, shadow, raised
/// surface and a border in `border` with `title`. Returns the inner area.
pub fn modal_frame(
    frame: &mut Frame<'_>,
    rect: Rect,
    theme: &Theme,
    title: Line<'static>,
    border: Style,
) -> Rect {
    dim_backdrop(frame, rect, theme);
    shadow(frame, rect, theme);
    frame.render_widget(Clear, rect);
    let block = theme
        .popup()
        .border_style(border.patch(theme.surface_bg()))
        .title(title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

/// A vertical scrollbar on the right edge of `area` (drawn over a border)
/// when `total` rows do not fit in `shown`; returns where it was drawn.
pub fn scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    total: usize,
    offset: usize,
    shown: usize,
) -> Option<Rect> {
    if total <= shown || area.height < 3 || area.width == 0 {
        return None;
    }
    let ascii = theme.icons.set == crate::theme::IconSet::Ascii;
    let mut sb_state = ScrollbarState::new(total.saturating_sub(shown))
        .position(offset)
        .viewport_content_length(shown);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some(if ascii { "|" } else { "│" }))
        .thumb_symbol(if ascii { "#" } else { "┃" })
        .track_style(theme.border)
        .thumb_style(theme.border_focus);
    frame.render_stateful_widget(bar, area, &mut sb_state);
    Some(Rect {
        x: area.right().saturating_sub(1),
        width: 1,
        ..area
    })
}
