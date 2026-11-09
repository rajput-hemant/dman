//! Confirmation dialogs for destructive actions.
//!
//! A dialog asks for `y`/`n` once (also for deleting files; the user decided
//! against a second typed confirmation). Confirming runs an [`ApiCall`] or an
//! action.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::action::Action;
use crate::api::ApiCall;
use crate::theme::Theme;
use crate::widgets::{ModalOutcome, centered};

/// What happens on confirmation.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmNext {
    /// Run this call.
    Call(ApiCall),
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
    /// What confirming does.
    pub next: ConfirmNext,
}

impl ConfirmState {
    /// A y/n dialog that runs `call`.
    pub fn yes_no(title: impl Into<String>, lines: Vec<String>, call: ApiCall) -> Self {
        Self {
            title: title.into(),
            lines,
            next: ConfirmNext::Call(call),
        }
    }

    /// A y/N dialog that leaves the top view when confirmed.
    pub fn discard(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            title: title.into(),
            lines,
            next: ConfirmNext::LeaveView,
        }
    }

    /// A y/N dialog that runs `action` when confirmed.
    pub fn run(title: impl Into<String>, lines: Vec<String>, action: Action) -> Self {
        Self {
            title: title.into(),
            lines,
            next: ConfirmNext::Run(action),
        }
    }

    /// Handle a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.confirmed(),
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    fn confirmed(&self) -> ModalOutcome {
        match &self.next {
            ConfirmNext::Call(c) => ModalOutcome::Api(c.clone()),
            ConfirmNext::LeaveView => ModalOutcome::LeaveView,
            ConfirmNext::Run(a) => ModalOutcome::Run(*a),
        }
    }

    /// Footer hints.
    pub fn hints(&self) -> Vec<(String, String)> {
        vec![
            ("y/Enter".into(), "yes".into()),
            ("n/Esc".into(), "no, cancel".into()),
        ]
    }

    /// Draw the dialog centred in `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut lines: Vec<Line<'_>> = self.lines.iter().map(|l| Line::from(l.as_str())).collect();
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("y", theme.key),
            Span::raw(" / "),
            Span::styled("Enter", theme.key),
            Span::raw(" yes     "),
            Span::styled("n", theme.key),
            Span::raw(" / "),
            Span::styled("Esc", theme.key),
            Span::raw(" no"),
        ]));
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
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            c.handle_key(enter),
            ModalOutcome::Api(ApiCall::Purge(vec![JobId(1)]))
        );
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(c.handle_key(esc), ModalOutcome::Close);
    }
}
