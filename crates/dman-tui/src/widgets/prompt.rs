//! A small single-line prompt dialog (e.g. "max concurrent downloads").

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::format;
use crate::theme::Theme;
use crate::widgets::input::LineInput;
use crate::widgets::text::wrap;
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

    /// Draw the prompt. On short terminals the explanation gives way first
    /// (it is cut to fewer rows), then the key row; the input always shows.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let width = area.width.saturating_sub(4).min(64).max(area.width.min(20));
        let inner_w = usize::from(width.saturating_sub(4)).max(1);
        let mut label = wrap(&Line::styled(self.label.clone(), theme.surface), inner_w);
        let room = usize::from(area.height.saturating_sub(2));
        // input row, plus (when there is room) gaps and the key row.
        let extras = match room {
            0..=2 => 0,
            3..=4 => 1,
            _ => 3,
        };
        let label_max = room.saturating_sub(1 + extras);
        if label.len() > label_max {
            label.truncate(label_max);
            if let Some(last) = label.last_mut() {
                let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
                *last = Line::styled(
                    format::truncate(&format!("{text}…"), inner_w),
                    theme.surface,
                );
            }
        }
        let label_rows = u16::try_from(label.len()).unwrap_or(0);
        let extras_rows = u16::try_from(extras).unwrap_or(0);
        let rect = centered(area, width, label_rows + 3 + extras_rows);
        let title = format::truncate(&self.title, usize::from(width.saturating_sub(4)));
        let inner = modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(
                format!(" {title} "),
                theme.accent.patch(theme.surface_bg()),
            )),
            theme.border_focus,
        );
        let inner = inner.inner(Margin::new(1, 0));
        let row = |y: u16| Rect {
            y: inner.y.saturating_add(y),
            height: 1,
            ..inner
        };
        frame.render_widget(
            Paragraph::new(label),
            Rect {
                height: label_rows,
                ..inner
            },
        );
        let gap = u16::from(extras >= 3);
        let input = row(label_rows + gap);
        let mut line = vec![Span::styled("› ", theme.accent.patch(theme.surface_bg()))];
        line.extend(
            self.input
                .line_fit(theme, true, usize::from(input.width.saturating_sub(2)))
                .spans,
        );
        frame.buffer_mut().set_style(input, theme.cursor);
        frame.render_widget(Paragraph::new(Line::from(line)), input);
        if extras >= 1 {
            let keys = row(label_rows + gap * 2 + 1);
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
}
