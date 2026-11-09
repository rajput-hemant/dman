//! Command palette: fuzzy search over every [`Action`]; Enter runs it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::app::State;
use crate::keymap::{Context, Keymap};
use crate::layout::Hit;
use crate::theme::Theme;
use crate::widgets::input::LineInput;
use crate::widgets::{ModalOutcome, centered, modal_frame, scrollbar};

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

    /// Move the highlight by `delta` (clamped).
    pub fn step(&mut self, delta: isize) -> ModalOutcome {
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
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, state: &State) {
        let theme = &state.theme;
        let keymap = &state.keymap;
        let video = state.video_prefs.as_ref();
        let width = area
            .width
            .saturating_sub(4)
            .min((area.width * 7 / 10).max(76))
            .min(100);
        let inner_w = usize::from(width.saturating_sub(2));
        // Rows of each item: one, or two when the title has to wrap.
        let rows: Vec<Row> = self
            .items
            .iter()
            .map(|a| Row::new(*a, keymap, video, inner_w))
            .collect();
        let wanted = u16::try_from(rows.iter().map(|r| r.lines).sum::<usize>())
            .unwrap_or(u16::MAX)
            .saturating_add(4);
        let height = wanted.clamp(6, 22).min(area.height.saturating_sub(2));
        let rect = Rect {
            y: area.y + (area.height.saturating_sub(height)) / 4,
            ..centered(area, width, height)
        };
        let title = Line::from(vec![Span::styled(
            " Commands ",
            theme.accent.patch(theme.surface_bg()),
        )]);
        let inner = modal_frame(frame, rect, theme, title, theme.border_focus);
        let count = format!(" {}/{} ", self.items.len(), Action::ALL.len());
        let cw = u16::try_from(count.len()).unwrap_or(0);
        frame.render_widget(
            Paragraph::new(Span::styled(count, theme.faint.patch(theme.surface_bg()))),
            Rect::new(rect.right().saturating_sub(cw + 2), rect.y, cw, 1),
        );
        let [query, rule, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        let mut q = vec![Span::styled(" › ", theme.accent.patch(theme.surface_bg()))];
        q.extend(self.input.line(theme, true).spans);
        frame.render_widget(Paragraph::new(Line::from(q)).style(theme.surface), query);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(usize::from(rule.width)),
                theme.border.patch(theme.surface_bg()),
            )),
            rule,
        );
        if self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "  No command matches — Esc closes",
                    theme.dim.patch(theme.surface_bg()),
                )),
                list,
            );
            return;
        }
        // Keep the highlighted item visible.
        let height = usize::from(list.height);
        let mut start = 0;
        while start < self.selected
            && rows[start..=self.selected]
                .iter()
                .map(|r| r.lines)
                .sum::<usize>()
                > height
        {
            start += 1;
        }
        let pattern = Pattern::parse(
            self.input.text().trim(),
            CaseMatching::Ignore,
            Normalization::Smart,
        );
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut y = list.y;
        for (i, row) in rows.iter().enumerate().skip(start) {
            let h = u16::try_from(row.lines).unwrap_or(1);
            if y + h > list.bottom() {
                break;
            }
            let area = Rect::new(list.x, y, list.width, h);
            let selected = i == self.selected;
            let lines = row.lines(theme, selected, &pattern, &mut matcher, inner_w);
            let base = if selected {
                theme.cursor
            } else {
                theme.surface
            };
            frame.buffer_mut().set_style(area, base);
            frame.render_widget(Paragraph::new(lines), area);
            state.hit(area, Hit::PaletteItem(i));
            y += h;
        }
        let total: usize = rows.iter().map(|r| r.lines).sum();
        let before: usize = rows[..start].iter().map(|r| r.lines).sum();
        scrollbar(
            frame,
            Rect {
                y: list.y,
                height: list.height,
                x: rect.x,
                width: rect.width,
            },
            theme,
            total,
            before,
            height,
        );
    }
}

/// Group shown in the palette's category column.
pub fn category(a: Action) -> &'static str {
    match a.home_context() {
        Context::Detail if a != Action::Back => "Detail",
        Context::Add => "Add",
        Context::Search => "Search",
        Context::Providers => "Providers",
        Context::Global | Context::Detail | Context::Help => "General",
        Context::Downloads => match a {
            Action::Up
            | Action::Down
            | Action::PageUp
            | Action::PageDown
            | Action::Top
            | Action::Bottom => "Move",
            Action::ToggleSelect | Action::SelectAll | Action::SelectNone | Action::SelectRange => {
                "Select"
            }
            Action::SortNext
            | Action::SortReverse
            | Action::Filter
            | Action::ClearFilter
            | Action::ShowAll
            | Action::ShowActive
            | Action::ShowWaiting
            | Action::ShowDone
            | Action::ShowFailed
            | Action::StoppedNextPage
            | Action::StoppedPrevPage
            | Action::ToggleGraph
            | Action::ToggleRemoved => "View",
            Action::MoveUp | Action::MoveDown | Action::MoveTop | Action::MoveBottom => "Queue",
            Action::LimitUp | Action::LimitDown | Action::SetMaxConcurrent => "Limits",
            _ => "Downloads",
        },
    }
}

/// The title without the category prefix ("Detail: next tab" → "Next tab").
fn display_title(a: Action) -> String {
    let t = a.title();
    for p in ["Detail: ", "Add view: ", "Search: ", "Providers: "] {
        if let Some(rest) = t.strip_prefix(p) {
            let mut c = rest.chars();
            return match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            };
        }
    }
    t.to_owned()
}

/// Width of the key column (keys are right-aligned in it).
const KEY_COL: usize = 11;

/// One palette entry laid out for a width.
struct Row {
    title: String,
    category: &'static str,
    keys: String,
    show_category: bool,
    lines: usize,
}

impl Row {
    fn new(
        a: Action,
        keymap: &Keymap,
        video: Option<&dman_proto::VideoPreferences>,
        width: usize,
    ) -> Self {
        let mut title = display_title(a);
        if let (Action::VideoQualityToggle, Some(p)) = (a, video) {
            title.insert_str(
                0,
                if p.ask_quality {
                    "[ask on] "
                } else {
                    "[ask off] "
                },
            );
        }
        let keys = keymap.label(a.home_context(), a);
        let keys = keys.split(", ").next().unwrap_or("").to_owned();
        let category = category(a);
        let key_w = KEY_COL + 1;
        let title_w = title.chars().count() + 3;
        let cat_w = category.len() + 2;
        let (show_category, lines) = if title_w + cat_w + key_w <= width {
            (true, 1)
        } else if title_w + key_w <= width {
            (false, 1)
        } else {
            (false, 2)
        };
        Self {
            title,
            category,
            keys,
            show_category,
            lines,
        }
    }

    fn lines(
        &self,
        theme: &Theme,
        selected: bool,
        pattern: &Pattern,
        matcher: &mut Matcher,
        width: usize,
    ) -> Vec<Line<'static>> {
        let base = if selected {
            theme.cursor
        } else {
            theme.surface
        };
        let bg = |s: Style| match base.bg {
            Some(c) => s.bg(c),
            None => s,
        };
        let mut idx = Vec::new();
        let mut buf = Vec::new();
        let _ = pattern.indices(Utf32Str::new(&self.title, &mut buf), matcher, &mut idx);
        let mut head = vec![Span::styled(
            if selected { theme.icons.cursor } else { " " },
            bg(theme.cursor_bar),
        )];
        head.push(Span::styled(" ", base));
        let mut title_spans = Vec::new();
        for (i, ch) in self.title.chars().enumerate() {
            let hit = u32::try_from(i).is_ok_and(|i| idx.contains(&i));
            title_spans.push(Span::styled(
                ch.to_string(),
                if hit { bg(theme.matched) } else { base },
            ));
        }
        let mut tail = Vec::new();
        if self.show_category {
            tail.push(Span::styled(
                format!("{}  ", self.category),
                bg(theme.faint),
            ));
        }
        let pill_w = if self.keys.is_empty() {
            0
        } else {
            self.keys.chars().count() + 2
        };
        tail.push(Span::styled(
            " ".repeat(KEY_COL.saturating_sub(pill_w)),
            base,
        ));
        if !self.keys.is_empty() {
            tail.push(Span::styled(format!(" {} ", self.keys), theme.pill));
        }
        tail.push(Span::styled(" ", base));
        let tail_w: usize = tail.iter().map(Span::width).sum();
        if self.lines == 1 {
            let used = 2 + self.title.chars().count();
            let mut spans = head;
            spans.extend(title_spans);
            spans.push(Span::styled(
                " ".repeat(width.saturating_sub(used + tail_w)),
                base,
            ));
            spans.extend(tail);
            return vec![Line::from(spans)];
        }
        // Wrap the title onto two lines; keys go at the end of the second.
        let first_w = width.saturating_sub(3);
        let chars: Vec<char> = self.title.chars().collect();
        let mut cut_chars = first_w.min(chars.len());
        if let Some(p) = chars[..cut_chars].iter().rposition(|c| *c == ' ') {
            cut_chars = p;
        }
        let skip = if chars.get(cut_chars) == Some(&' ') {
            cut_chars + 1
        } else {
            cut_chars
        };
        let mut l1 = head;
        l1.extend(title_spans.iter().take(cut_chars).cloned());
        let mut l2 = vec![Span::styled("   ", base)];
        l2.extend(title_spans.into_iter().skip(skip));
        let used2: usize = l2.iter().map(Span::width).sum();
        l2.push(Span::styled(
            " ".repeat(width.saturating_sub(used2 + tail_w)),
            base,
        ));
        l2.extend(tail);
        vec![Line::from(l1), Line::from(l2)]
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
    fn titles_lose_their_category_prefix() {
        assert_eq!(display_title(Action::DetailNextTab), "Next tab");
        assert_eq!(category(Action::DetailNextTab), "Detail");
        assert_eq!(display_title(Action::PauseAll), "Pause all");
        assert_eq!(category(Action::LimitUp), "Limits");
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
