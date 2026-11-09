//! A small single-line prompt dialog (e.g. "max concurrent downloads").

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::theme::Theme;
use crate::widgets::input::LineInput;
use crate::widgets::{ModalOutcome, centered, modal_frame};

/// What the prompt's value is for; interpreted by `update` on submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Global max concurrent downloads.
    MaxConcurrent,
    /// A per-download option of the detail view.
    JobOption(crate::views::detail::OptField),
    /// Search: minimum seeders.
    SearchSeeders,
    /// Search: size range.
    SearchSize,
    /// Search: source provider filter.
    SearchSource,
    /// Search: fuzzy title filter.
    SearchTitle,
    /// Providers: path of a file to import.
    ProvidersImport,
    /// Providers: path of a file to export to.
    ProvidersExport,
    /// Video quality picker: default quality.
    VideoQuality,
    /// Video quality picker: default audio.
    VideoAudio,
    /// A new name for a download (or one of its files).
    Rename {
        /// The download.
        id: dman_proto::JobId,
        /// 1-based file of a multi-file download.
        file_index: Option<u32>,
    },
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
        let width = area.width.saturating_sub(4).min(64);
        let inner_w = usize::from(width.saturating_sub(4)).max(1);
        let label_rows =
            u16::try_from(self.label.chars().count().div_ceil(inner_w).max(1)).unwrap_or(1);
        let rect = centered(area, width, 6 + label_rows);
        let inner = modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(
                format!(" {} ", self.title),
                theme.accent.patch(theme.surface_bg()),
            )),
            theme.border_focus,
        );
        let inner = inner.inner(Margin::new(1, 0));
        let [label, _, input, _, keys] = Layout::vertical([
            Constraint::Length(label_rows),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        frame.render_widget(
            Paragraph::new(Line::styled(self.label.as_str(), theme.surface))
                .wrap(Wrap { trim: false }),
            label,
        );
        let mut line = vec![Span::styled("› ", theme.accent.patch(theme.surface_bg()))];
        line.extend(self.input.line(theme, true).spans);
        let field = theme.cursor;
        frame.buffer_mut().set_style(input, field);
        frame.render_widget(Paragraph::new(Line::from(line)), input);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Enter ", theme.pill),
                Span::styled(" apply   ", theme.dim.patch(theme.surface_bg())),
                Span::styled(" Esc ", theme.pill),
                Span::styled(" cancel", theme.dim.patch(theme.surface_bg())),
            ]))
            .alignment(Alignment::Right),
            keys,
        );
    }
}
