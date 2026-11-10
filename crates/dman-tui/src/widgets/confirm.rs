//! Confirmation dialogs.
//!
//! A dialog asks once (also when deleting files). It has two
//! buttons; `y` confirms and `n` / `Esc` cancel from anywhere, `←`/`→`/`Tab`
//! move the focus and `Enter` presses the focused button. Destructive dialogs
//! (deleting files, shutting the daemon down, …) have a danger border and a red
//! confirm button. The focus starts on the confirm button so `y`/`Enter`
//! answer the one question the user already asked for; `n`/`Esc`
//! or moving to Cancel back out.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::api::ApiCall;
use crate::app::State;
use crate::format;
use crate::layout::Hit;
use crate::widgets::text::wrap;
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

    /// The body rows for `cols` columns, shortened to at most `max` rows:
    /// listed items (`•` lines) give way first, replaced by a count, so the
    /// consequence lines and the buttons always stay visible.
    fn body_rows(
        &self,
        theme: &crate::theme::Theme,
        cols: usize,
        max: usize,
    ) -> Vec<Line<'static>> {
        let wrapped = |lines: &[String]| -> Vec<Line<'static>> {
            lines
                .iter()
                .flat_map(|l| wrap(&Line::styled(l.clone(), theme.surface), cols))
                .collect()
        };
        let mut lines = self.lines.clone();
        let mut rows = wrapped(&lines);
        let is_item = |l: &String| l.trim_start().starts_with('•');
        let more_at = lines
            .iter()
            .position(|l| l.trim_start().starts_with("… and "));
        let mut hidden = more_at
            .and_then(|i| lines[i].split_whitespace().nth(2)?.parse::<usize>().ok())
            .unwrap_or(0);
        while rows.len() > max {
            let Some(i) = lines.iter().rposition(is_item) else {
                break;
            };
            lines.remove(i);
            hidden += 1;
            let more = format!("   … and {hidden} more");
            match lines
                .iter()
                .position(|l| l.trim_start().starts_with("… and "))
            {
                Some(j) => lines[j] = more,
                None => lines.insert(i, more),
            }
            rows = wrapped(&lines);
        }
        if rows.len() > max {
            rows.truncate(max.saturating_sub(1));
            rows.push(Line::styled("…", theme.faint.patch(theme.surface_bg())));
            rows.truncate(max);
        }
        rows
    }

    /// Draw the dialog centred in `area`. It never grows past `area`: the
    /// body is shortened instead and the button row always shows.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, state: &State) {
        let theme = &state.theme;
        let surface = theme.surface;
        let width = area.width.min(64);
        let inner_w = usize::from(width.saturating_sub(4)).max(1);
        // Buttons: roomy when they fit, compact on narrow terminals.
        let roomy = format!("[ Cancel ]  [ {} ]  y / n", self.confirm_label);
        let (cancel, confirm, gap) = if format::width(&roomy) <= inner_w {
            (
                " Cancel ".to_owned(),
                format!(" {} ", self.confirm_label),
                "  ",
            )
        } else {
            ("Cancel".to_owned(), self.confirm_label.clone(), " ")
        };
        let button = |label: &str, focused: bool, danger: bool| {
            let style = match (focused, danger) {
                (true, true) => theme.danger_button,
                (true, false) => theme.mode,
                (false, _) => theme.dim.patch(theme.surface_bg()),
            };
            Span::styled(format!("[{label}]"), style)
        };
        let buttons_w = format::width(&cancel) + format::width(&confirm) + 4 + gap.len();
        let hint = if buttons_w + 7 <= inner_w {
            "y / n"
        } else {
            ""
        };
        let pad = inner_w.saturating_sub(buttons_w + hint.len());
        let buttons = Line::from(vec![
            Span::styled(hint, theme.faint.patch(theme.surface_bg())),
            Span::styled(" ".repeat(pad), surface),
            button(&cancel, !self.focus_confirm, false),
            Span::styled(gap, surface),
            button(&confirm, self.focus_confirm, self.danger),
        ]);
        // Rows inside the border: body, a blank line when there is room, buttons.
        let room = usize::from(area.height.saturating_sub(2)).max(1);
        let mut body = self.body_rows(theme, inner_w, room.saturating_sub(1).max(1));
        if body.len() + 2 <= room {
            body.push(Line::default());
        }
        let body_rows = body.len().min(room.saturating_sub(1));
        body.truncate(body_rows);
        body.push(buttons);
        let height = u16::try_from(body.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        let rect = centered(area, width, height);
        let (border, title_style) = if self.danger {
            (theme.border_danger, theme.error)
        } else {
            (theme.border_focus, theme.accent)
        };
        let title = format::truncate(&self.title, usize::from(width.saturating_sub(4)));
        let inner = modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(
                format!(" {title} "),
                title_style.patch(theme.surface_bg()),
            )),
            border,
        );
        let inner = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        frame.render_widget(Paragraph::new(body).style(surface), inner);
        // Buttons sit on the last inner row, right-aligned.
        let y = inner.bottom().saturating_sub(1);
        let cw = u16::try_from(format::width(&cancel) + 2).unwrap_or(0);
        let fw = u16::try_from(format::width(&confirm) + 2).unwrap_or(0);
        let gap = u16::try_from(gap.len()).unwrap_or(1);
        let right = inner.right();
        let confirm_x = right.saturating_sub(fw);
        let cancel_x = confirm_x.saturating_sub(gap + cw);
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
