//! A small single-line prompt dialog (e.g. "max concurrent downloads").

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme::Theme;
use crate::widgets::input::LineInput;
use crate::widgets::{ModalOutcome, centered};

/// What the prompt's value is for; interpreted by `update` on submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Global max concurrent downloads.
    MaxConcurrent,
}

/// Prompt state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptState {
    /// What it is for.
    pub kind: PromptKind,
    /// Dialog title.
    pub title: String,
    /// Explanation line.
    pub label: String,
    /// The value.
    pub input: LineInput,
}

impl PromptState {
    /// A prompt pre-filled with `value`.
    pub fn new(kind: PromptKind, title: &str, label: &str, value: &str) -> Self {
        Self {
            kind,
            title: title.into(),
            label: label.into(),
            input: LineInput::new(value),
        }
    }

    /// Handle a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => ModalOutcome::Close,
            KeyCode::Enter => ModalOutcome::Submit(self.kind, self.input.text().trim().to_owned()),
            _ => {
                self.input.handle_key(key);
                ModalOutcome::Stay
            }
        }
    }

    /// Footer hints.
    pub fn hints() -> Vec<(String, String)> {
        vec![
            ("Enter".into(), "apply".into()),
            ("Esc".into(), "cancel".into()),
        ]
    }

    /// Draw the prompt.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let rect = centered(area, 50, 5);
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(format!(" {} ", self.title), theme.accent));
        let lines = vec![
            Line::from(self.label.as_str()),
            Line::default(),
            self.input.line(theme, true),
        ];
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }
}
