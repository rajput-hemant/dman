//! Command palette: fuzzy search over every [`Action`]; Enter runs it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::action::Action;
use crate::keymap::Keymap;
use crate::theme::Theme;
use crate::widgets::input::LineInput;
use crate::widgets::{ModalOutcome, centered};

/// Palette state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteState {
    /// Query.
    pub input: LineInput,
    /// Matching actions, best first.
    pub items: Vec<Action>,
    /// Highlighted item.
    pub selected: usize,
}

impl Default for PaletteState {
    fn default() -> Self {
        let mut s = Self {
            input: LineInput::default(),
            items: Vec::new(),
            selected: 0,
        };
        s.refilter();
        s
    }
}

impl PaletteState {
    /// Recompute [`Self::items`] from the query.
    pub fn refilter(&mut self) {
        let query = self.input.text().trim();
        self.selected = 0;
        if query.is_empty() {
            self.items = Action::ALL.to_vec();
            return;
        }
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize, Action)> = Action::ALL
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                let hay = format!("{} {}", a.title(), a.id());
                let score = pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher)?;
                Some((score, i, *a))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.items = scored.into_iter().map(|(_, _, a)| a).collect();
    }

    /// Handle a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> ModalOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => ModalOutcome::Close,
            KeyCode::Enter => match self.items.get(self.selected) {
                Some(a) => ModalOutcome::Run(*a),
                None => ModalOutcome::Stay,
            },
            KeyCode::Up => self.step(-1),
            KeyCode::Char('p' | 'k') if ctrl => self.step(-1),
            KeyCode::Down | KeyCode::Tab => self.step(1),
            KeyCode::Char('n' | 'j') if ctrl => self.step(1),
            _ => {
                if self.input.handle_key(key) {
                    self.refilter();
                }
                ModalOutcome::Stay
            }
        }
    }

    /// Insert pasted text into the query.
    pub fn paste(&mut self, text: &str) {
        self.input.insert_str(text);
        self.refilter();
    }

    fn step(&mut self, delta: isize) -> ModalOutcome {
        if !self.items.is_empty() {
            let last = self.items.len() - 1;
            self.selected = self.selected.saturating_add_signed(delta).min(last);
        }
        ModalOutcome::Stay
    }

    /// Footer hints.
    pub fn hints() -> Vec<(String, String)> {
        vec![
            ("Enter".into(), "run".into()),
            ("↑↓".into(), "choose".into()),
            ("Esc".into(), "close".into()),
        ]
    }

    /// Draw the palette.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, keymap: &Keymap) {
        let wanted = u16::try_from(self.items.len())
            .unwrap_or(u16::MAX)
            .saturating_add(3);
        let height = wanted.clamp(5, 20).min(area.height.saturating_sub(2));
        let rect = centered(area, 64, height);
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(" Command palette ", theme.accent));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let [query, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        let mut q = vec![Span::styled("> ", theme.accent)];
        q.extend(self.input.line(theme, true).spans);
        frame.render_widget(Paragraph::new(Line::from(q)), query);

        let key_width = 14usize;
        let title_width = usize::from(list.width).saturating_sub(key_width + 1);
        let items: Vec<ListItem<'_>> = self
            .items
            .iter()
            .map(|a| {
                let keys = keymap.label(a.home_context(), *a);
                let title = crate::format::truncate(a.title(), title_width);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{title:<title_width$} ")),
                    Span::styled(crate::format::truncate(&keys, key_width), theme.key),
                ]))
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(
            List::new(items).highlight_style(theme.cursor),
            list,
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_action_initially() {
        let p = PaletteState::default();
        assert_eq!(p.items.len(), Action::ALL.len());
        for a in Action::ALL {
            assert!(p.items.contains(a), "{a:?} missing from palette");
        }
    }

    #[test]
    fn fuzzy_search_and_run() {
        let mut p = PaletteState::default();
        p.paste("pause all");
        assert_eq!(p.items.first(), Some(&Action::PauseAll));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(p.handle_key(enter), ModalOutcome::Run(Action::PauseAll));
    }

    #[test]
    fn navigation_clamps() {
        let mut p = PaletteState::default();
        p.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(p.selected, 0);
        p.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(p.selected, 1);
    }
}
