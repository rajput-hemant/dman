//! Confirmation dialogs.
//!
//! A dialog asks once (also for deleting files: decision U1). It has two
//! buttons; `y` confirms and `n` / `Esc` cancel from anywhere, `←`/`→`/`Tab`
//! move the focus and `Enter` presses the focused button. Destructive dialogs
//! (deleting files, shutting the daemon down, …) have a danger border and a red
//! confirm button. The focus starts on the confirm button so `y`/`Enter`
//! answer the one question the user already asked for (decision U1); `n`/`Esc`
//! or moving to Cancel back out.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::action::Action;
use crate::api::ApiCall;
use crate::app::State;
use crate::layout::Hit;
use crate::widgets::{ModalOutcome, centered, modal_frame};

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
    /// Destructive: danger colours, Cancel focused first.
    pub danger: bool,
    /// Label of the confirm button.
    pub confirm_label: String,
    /// Focused button: `false` = Cancel, `true` = confirm.
    pub focus_confirm: bool,
}

impl ConfirmState {
    fn new(title: impl Into<String>, lines: Vec<String>, next: ConfirmNext) -> Self {
        Self {
            title: title.into(),
            lines,
            next,
            danger: false,
            confirm_label: "Yes".into(),
            focus_confirm: true,
        }
    }

    /// A y/n dialog that runs `call`.
    pub fn yes_no(title: impl Into<String>, lines: Vec<String>, call: ApiCall) -> Self {
        Self::new(title, lines, ConfirmNext::Call(call))
    }

    /// A y/N dialog that leaves the top view when confirmed.
    pub fn discard(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self::new(title, lines, ConfirmNext::LeaveView)
    }

    /// A y/N dialog that runs `action` when confirmed.
    pub fn run(title: impl Into<String>, lines: Vec<String>, action: Action) -> Self {
        Self::new(title, lines, ConfirmNext::Run(action))
    }

    /// Name the confirm button (e.g. "Remove").
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.confirm_label = label.into();
        self
    }

    /// Mark the dialog destructive: danger border and confirm button.
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Handle a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('y' | 'Y') => self.confirmed(),
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => ModalOutcome::Close,
            KeyCode::Enter => self.press(self.focus_confirm),
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Char('h' | 'l') => {
                self.focus_confirm = !self.focus_confirm;
                ModalOutcome::Stay
            }
            _ => ModalOutcome::Stay,
        }
    }

    /// Press a button (`true` = confirm).
    pub fn press(&self, confirm: bool) -> ModalOutcome {
        if confirm {
            self.confirmed()
        } else {
            ModalOutcome::Close
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
        let enter = if self.focus_confirm {
            self.confirm_label.to_lowercase()
        } else {
            "cancel".to_owned()
        };
        vec![
            ("y".into(), self.confirm_label.to_lowercase()),
            ("n/Esc".into(), "cancel".into()),
            ("Enter".into(), enter),
            ("←→".into(), "switch button".into()),
        ]
    }

    /// Draw the dialog centred in `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, state: &State) {
        let theme = &state.theme;
        let surface = theme.surface;
        let mut lines: Vec<Line<'_>> = self
            .lines
            .iter()
            .map(|l| Line::styled(l.as_str(), surface))
            .collect();
        lines.push(Line::default());
        let cancel = " Cancel ".to_owned();
        let confirm = format!(" {} ", self.confirm_label);
        let button = |label: &str, focused: bool, danger: bool| {
            if focused {
                Span::styled(
                    format!("[{label}]"),
                    if danger {
                        theme.danger_button
                    } else {
                        theme.mode
                    },
                )
            } else {
                Span::styled(format!("[{label}]"), theme.dim.patch(theme.surface_bg()))
            }
        };
        let width = area.width.min(64);
        let inner_w = usize::from(width.saturating_sub(4)).max(1);
        let buttons_w = cancel.chars().count() + confirm.chars().count() + 6;
        let hint = "y / n";
        let pad = inner_w.saturating_sub(buttons_w + hint.len());
        lines.push(Line::from(vec![
            Span::styled(hint, theme.faint.patch(theme.surface_bg())),
            Span::styled(" ".repeat(pad), surface),
            button(&cancel, !self.focus_confirm, false),
            Span::styled("  ", surface),
            button(&confirm, self.focus_confirm, self.danger),
        ]));
        let rows: usize = lines
            .iter()
            .map(|l| l.width().div_ceil(inner_w).max(1))
            .sum();
        let height = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2);
        let rect = centered(area, width, height);
        let (border, title_style) = if self.danger {
            (theme.border_danger, theme.error)
        } else {
            (theme.border_focus, theme.accent)
        };
        let inner = modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(
                format!(" {} ", self.title),
                title_style.patch(theme.surface_bg()),
            )),
            border,
        );
        let inner = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        frame.render_widget(
            Paragraph::new(lines)
                .style(surface)
                .wrap(Wrap { trim: false }),
            inner,
        );
        // Buttons sit on the last inner row, right-aligned.
        let y = inner.bottom().saturating_sub(1);
        let cw = u16::try_from(cancel.chars().count() + 2).unwrap_or(0);
        let fw = u16::try_from(confirm.chars().count() + 2).unwrap_or(0);
        let right = inner.right();
        let confirm_x = right.saturating_sub(fw);
        let cancel_x = confirm_x.saturating_sub(2 + cw);
        state.hit(Rect::new(cancel_x, y, cw, 1), Hit::Button(0));
        state.hit(Rect::new(confirm_x, y, fw, 1), Hit::Button(1));
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

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn yes_no() {
        let call = ApiCall::Purge(vec![JobId(1)]);
        let mut c = ConfirmState::yes_no("t", vec![], call.clone());
        assert_eq!(c.handle_key(k('x')), ModalOutcome::Stay);
        assert_eq!(c.handle_key(k('y')), ModalOutcome::Api(call));
        assert_eq!(c.handle_key(k('n')), ModalOutcome::Close);
        assert_eq!(
            c.handle_key(code(KeyCode::Enter)),
            ModalOutcome::Api(ApiCall::Purge(vec![JobId(1)]))
        );
        assert_eq!(c.handle_key(code(KeyCode::Esc)), ModalOutcome::Close);
    }

    #[test]
    fn destructive_dialogs_confirm_on_enter_and_cancel_on_the_cancel_button() {
        let call = ApiCall::Shutdown;
        let mut c = ConfirmState::yes_no("t", vec![], call.clone())
            .label("Shut down")
            .destructive();
        assert!(c.danger);
        assert_eq!(
            c.handle_key(code(KeyCode::Enter)),
            ModalOutcome::Api(call.clone())
        );
        assert_eq!(c.handle_key(code(KeyCode::Left)), ModalOutcome::Stay);
        assert_eq!(c.handle_key(code(KeyCode::Enter)), ModalOutcome::Close);
        assert_eq!(c.handle_key(k('y')), ModalOutcome::Api(call));
    }
}
