//! Add view (placeholder; the full form — links, file browser, clipboard,
//! per-add options, preview — replaces this module).
//!
//! Contract with the rest of the app: the view lives in [`View::Add`], keys reach
//! [`handle_raw_key`] before the keymap (return `None` to let the keymap handle
//! a key, e.g. Esc → `Back`), and submitting returns
//! `state.call(ApiCall::Add(Box::new(params)))` so the connection gate and status line
//! work as for every other action.
//!
//! [`View::Add`]: crate::views::View::Add

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::views::View;

/// Add view state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddState {
    /// Text collected so far (e.g. pasted links).
    pub text: String,
}

impl AddState {
    /// Start with pasted text.
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
        }
    }
}

/// Raw keys (text entry). The placeholder consumes nothing.
pub fn handle_raw_key(_state: &mut State, _key: KeyEvent) -> Option<Vec<Cmd>> {
    None
}

/// Add-view actions.
pub fn apply(_state: &mut State, _action: Action) -> Option<Vec<Cmd>> {
    None
}

/// Bracketed paste into the add view.
pub fn on_paste(state: &mut State, text: &str) {
    if let Some(View::Add(a)) = state.views.last_mut() {
        if !a.text.is_empty() && !a.text.ends_with('\n') {
            a.text.push('\n');
        }
        a.text.push_str(text);
    }
}

/// Draw the add view.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, a: &AddState) {
    let t = &state.theme;
    let mut lines = vec![
        Line::styled("The add form is not available in this build yet.", t.warn),
        Line::styled("Use `dman add <url>` from a shell meanwhile.", t.dim),
        Line::default(),
    ];
    if !a.text.is_empty() {
        lines.push(Line::styled("Pasted:", t.dim));
        lines.extend(a.text.lines().map(|l| Line::from(l.to_owned())));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border)
        .title(Span::styled(" Add downloads ", t.accent));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}
