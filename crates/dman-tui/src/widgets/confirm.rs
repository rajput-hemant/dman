//! Confirmation dialogs for destructive actions.
//!
//! A dialog either asks for `y`/`n`, or (for the second confirmation of
//! deleting files) for a typed word. Confirming either runs an [`ApiCall`] or
//! opens the next dialog in a chain.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::action::Action;
use crate::api::ApiCall;
use crate::theme::Theme;
use crate::widgets::input::LineInput;
use crate::widgets::{ModalOutcome, centered};

/// The word to type before files are deleted.
pub const DELETE_WORD: &str = "DELETE";

/// How the user confirms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmMode {
    /// Press `y` to confirm, `n` or Esc to cancel.
    YesNo,
    /// Type `word` and press Enter; Esc cancels.
    Typed {
        /// Required word.
        word: &'static str,
        /// What was typed so far.
        input: LineInput,
    },
}

/// What happens on confirmation.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmNext {
    /// Run this call.
    Call(ApiCall),
    /// Ask again with this dialog.
    Then(Box<ConfirmState>),
    /// Leave the top view, dropping what was typed in it.
    LeaveView,
    /// Run an action in the top view.
    Run(Action),
}

/// A confirmation dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmState {
    /// Dialog title.
    pub title: String,
    /// Body lines.
    pub lines: Vec<String>,
    /// y/n or typed.
    pub mode: ConfirmMode,
    /// What confirming does.
    pub next: ConfirmNext,
}

impl ConfirmState {
    /// A y/n dialog that runs `call`.
    pub fn yes_no(title: impl Into<String>, lines: Vec<String>, call: ApiCall) -> Self {
        Self {
            title: title.into(),
            lines,
            mode: ConfirmMode::YesNo,
            next: ConfirmNext::Call(call),
        }
    }

    /// A y/n dialog that opens `next` when confirmed.
    pub fn then(title: impl Into<String>, lines: Vec<String>, next: ConfirmState) -> Self {
        Self {
            title: title.into(),
            lines,
            mode: ConfirmMode::YesNo,
            next: ConfirmNext::Then(Box::new(next)),
        }
    }

    /// A y/N dialog that leaves the top view when confirmed.
    pub fn discard(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            title: title.into(),
            lines,
            mode: ConfirmMode::YesNo,
            next: ConfirmNext::LeaveView,
        }
    }

    /// A y/N dialog that runs `action` when confirmed.
    pub fn run(title: impl Into<String>, lines: Vec<String>, action: Action) -> Self {
        Self {
            title: title.into(),
            lines,
            mode: ConfirmMode::YesNo,
            next: ConfirmNext::Run(action),
        }
    }

    /// A dialog that requires typing [`DELETE_WORD`].
    pub fn typed(title: impl Into<String>, lines: Vec<String>, call: ApiCall) -> Self {
        Self {
            title: title.into(),
            lines,
            mode: ConfirmMode::Typed {
                word: DELETE_WORD,
                input: LineInput::default(),
            },
            next: ConfirmNext::Call(call),
        }
    }

    /// Handle a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match &mut self.mode {
            ConfirmMode::YesNo => match key.code {
                KeyCode::Char('y' | 'Y') => self.confirmed(),
                KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => ModalOutcome::Close,
                _ => ModalOutcome::Stay,
            },
            ConfirmMode::Typed { word, input } => match key.code {
                KeyCode::Esc => ModalOutcome::Close,
                KeyCode::Enter if input.text() == *word => self.confirmed(),
                KeyCode::Enter => ModalOutcome::Stay,
                _ => {
                    input.handle_key(key);
                    ModalOutcome::Stay
                }
            },
        }
    }

    /// Handle pasted text (only typed dialogs accept it).
    pub fn paste(&mut self, text: &str) {
        if let ConfirmMode::Typed { input, .. } = &mut self.mode {
            input.insert_str(text);
        }
    }

    fn confirmed(&self) -> ModalOutcome {
        match &self.next {
            ConfirmNext::Call(c) => ModalOutcome::Api(c.clone()),
            ConfirmNext::LeaveView => ModalOutcome::LeaveView,
            ConfirmNext::Run(a) => ModalOutcome::Run(*a),
            ConfirmNext::Then(next) => {
                ModalOutcome::Replace(crate::widgets::Modal::Confirm(next.as_ref().clone()))
            }
        }
    }

    /// Footer hints.
    pub fn hints(&self) -> Vec<(String, String)> {
        match self.mode {
            ConfirmMode::YesNo => vec![
                ("y".into(), "confirm".into()),
                ("n/Esc".into(), "cancel".into()),
            ],
            ConfirmMode::Typed { word, .. } => vec![
                (format!("type {word} + Enter"), "confirm".into()),
                ("Esc".into(), "cancel".into()),
            ],
        }
    }

    /// Draw the dialog centred in `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut lines: Vec<Line<'_>> = self.lines.iter().map(|l| Line::from(l.as_str())).collect();
        lines.push(Line::default());
        match &self.mode {
            ConfirmMode::YesNo => lines.push(Line::from(vec![
                Span::styled("y", theme.key),
                Span::raw(" yes   "),
                Span::styled("n", theme.key),
                Span::raw(" no"),
            ])),
            ConfirmMode::Typed { word, input } => {
                lines.push(Line::from(vec![
                    Span::raw("Type "),
                    Span::styled(*word, theme.error),
                    Span::raw(" and press Enter:"),
                ]));
                lines.push(input.line(theme, true));
            }
        }
        let width = area.width.min(60);
        let inner = usize::from(width.saturating_sub(2)).max(1);
        let rows: usize = lines.iter().map(|l| l.width().div_ceil(inner).max(1)).sum();
        let height = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2);
        let rect = centered(area, width, height);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.error)
            .title(Span::styled(format!(" {} ", self.title), theme.error));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use dman_proto::JobId;

    use super::*;

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn yes_no() {
        let call = ApiCall::Purge(vec![JobId(1)]);
        let mut c = ConfirmState::yes_no("t", vec![], call.clone());
        assert_eq!(c.handle_key(k('x')), ModalOutcome::Stay);
        assert_eq!(c.handle_key(k('y')), ModalOutcome::Api(call));
        assert_eq!(c.handle_key(k('n')), ModalOutcome::Close);
    }

    #[test]
    fn typed_requires_exact_word() {
        let call = ApiCall::Shutdown;
        let mut c = ConfirmState::typed("t", vec![], call.clone());
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        // `y` is just text here.
        assert_eq!(c.handle_key(k('y')), ModalOutcome::Stay);
        assert_eq!(c.handle_key(enter), ModalOutcome::Stay);
        c.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        c.paste("delete");
        assert_eq!(c.handle_key(enter), ModalOutcome::Stay);
        c.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        c.paste("DELETE");
        assert_eq!(c.handle_key(enter), ModalOutcome::Api(call));
    }
}
