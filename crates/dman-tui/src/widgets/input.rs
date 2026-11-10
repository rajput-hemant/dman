//! A minimal single-line text input (cursor movement, editing, paste).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Single-line editable text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    /// Cursor position in characters.
    cursor: usize,
}

impl LineInput {
    /// Input pre-filled with `text`, cursor at the end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// Current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Insert text at the cursor; line breaks become spaces.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            let c = if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            };
            if !c.is_control() {
                let i = self.byte_index(self.cursor);
                self.text.insert(i, c);
                self.cursor += 1;
            }
        }
    }

    /// Handle an editing key. Returns `true` when the key was consumed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.text.chars().count();
        match key.code {
            KeyCode::Char('u') if ctrl => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char(c) if !ctrl => self.insert_str(&c.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let i = self.byte_index(self.cursor);
                self.text.remove(i);
            }
            KeyCode::Delete if self.cursor < len => {
                let i = self.byte_index(self.cursor);
                self.text.remove(i);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }

    fn delete_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1] == ' ' {
            start -= 1;
        }
        while start > 0 && chars[start - 1] != ' ' {
            start -= 1;
        }
        let (a, b) = (self.byte_index(start), self.byte_index(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor = start;
    }

    /// The text with a block cursor, for rendering.
    pub fn line(&self, theme: &Theme, focused: bool) -> Line<'static> {
        if !focused {
            return Line::from(self.text.clone());
        }
        let before: String = self.text.chars().take(self.cursor).collect();
        let at: String = self.text.chars().skip(self.cursor).take(1).collect();
        let after: String = self.text.chars().skip(self.cursor + 1).collect();
        let at = if at.is_empty() { " ".to_owned() } else { at };
        Line::from(vec![
            Span::raw(before),
            Span::styled(at, theme.cursor),
            Span::raw(after),
        ])
    }

    /// Like [`Self::line`], but scrolled horizontally so that the cursor stays
    /// visible in `width` columns; cut ends are marked with `…`.
    pub fn line_fit(&self, theme: &Theme, focused: bool, width: usize) -> Line<'static> {
        use crate::format::width as w;
        let chars: Vec<char> = self.text.chars().collect();
        let cursor = self.cursor.min(chars.len());
        // Columns needed: the text plus one cell for a cursor at the end.
        let total = w(&self.text) + usize::from(focused && cursor == chars.len());
        if total <= width || width < 3 {
            return self.line(theme, focused);
        }
        let cw = |c: &char| unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
        // Start at the cursor and grow to the left first (the text being
        // typed), then to the right.
        let mut start = cursor;
        let mut end = (cursor + 1).min(chars.len());
        let mut used = chars.get(cursor).map_or(1, cw);
        let room = width.saturating_sub(2);
        while start > 0 && used + cw(&chars[start - 1]) <= room {
            start -= 1;
            used += cw(&chars[start]);
        }
        while end < chars.len() && used + cw(&chars[end]) <= room {
            used += cw(&chars[end]);
            end += 1;
        }
        let text = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
        let mut spans = Vec::new();
        if start > 0 {
            spans.push(Span::styled("…", theme.faint));
        }
        if focused {
            spans.push(Span::raw(text(start, cursor)));
            let at = if cursor < chars.len() {
                text(cursor, cursor + 1)
            } else {
                " ".to_owned()
            };
            spans.push(Span::styled(at, theme.cursor));
            spans.push(Span::raw(text((cursor + 1).min(end), end)));
        } else {
            spans.push(Span::raw(text(start, end)));
        }
        if end < chars.len() {
            spans.push(Span::styled("…", theme.faint));
        }
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn editing() {
        let mut i = LineInput::new("héllo");
        assert!(i.handle_key(k(KeyCode::Backspace)));
        assert_eq!(i.text(), "héll");
        i.handle_key(k(KeyCode::Home));
        i.handle_key(k(KeyCode::Char('>')));
        assert_eq!(i.text(), ">héll");
        i.handle_key(k(KeyCode::Right));
        i.handle_key(k(KeyCode::Delete));
        assert_eq!(i.text(), ">hll");
        i.insert_str("a\nb");
        assert_eq!(i.text(), ">ha bll");
        i.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), ">ha ll");
        assert!(!i.handle_key(k(KeyCode::Enter)));
    }

    #[test]
    fn long_text_scrolls_to_keep_the_cursor_visible() {
        let t = Theme::default_theme();
        let i = LineInput::new("archlinux-2026.11.01-x86_64.iso");
        let line = i.line_fit(&t, true, 12);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(line.width() <= 12, "{text}");
        assert!(text.starts_with('…') && text.ends_with("iso "), "{text}");
        let mut i = i;
        i.handle_key(k(KeyCode::Home));
        let line = i.line_fit(&t, true, 12);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            line.width() <= 12 && text.starts_with("arch") && text.ends_with('…'),
            "{text}"
        );
        // Wide characters never overflow.
        let wide = LineInput::new("日本語のファイル名日本語のファイル名");
        assert!(wide.line_fit(&t, true, 9).width() <= 9);
    }
}
