//! A small multi-line text editor (also used single-line) for the add view.
//!
//! Cursor movement (arrows, Home/End, word jumps with Ctrl-←/→ or Alt-b/f,
//! document start/end), editing (Backspace/Delete join lines, Ctrl-w/u/k) and
//! paste that keeps line breaks. In single-line mode line breaks become spaces.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Editable text made of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    lines: Vec<String>,
    row: usize,
    /// Cursor column in characters.
    col: usize,
    single: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(false)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn byte_at(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(i, _)| i)
}

impl Editor {
    /// An empty editor; `single` forbids line breaks.
    pub fn new(single: bool) -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            single,
        }
    }

    /// An editor holding `text` with the cursor at its end.
    pub fn from_text(text: &str, single: bool) -> Self {
        let mut e = Self::new(single);
        e.insert_str(text);
        e
    }

    /// The whole text, lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The lines.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Whether there is no text at all.
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(String::is_empty)
    }

    /// Cursor as (row, column in characters).
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Whether line breaks are forbidden.
    pub fn is_single(&self) -> bool {
        self.single
    }

    /// Replace the text; the cursor goes to the end.
    pub fn set_text(&mut self, text: &str) {
        *self = Self::from_text(text, self.single);
    }

    /// Replace the lines (e.g. after removing finished sources), keeping the
    /// cursor row and column within bounds.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        self.row = self.row.min(self.lines.len() - 1);
        self.col = self.col.min(self.line_len());
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].chars().count()
    }

    /// Insert text at the cursor. `\r\n`, `\r` and `\n` start a new line (single
    /// line mode: become a space); tabs become spaces; other control characters
    /// are dropped.
    pub fn insert_str(&mut self, s: &str) {
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' | '\n' => {
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    if self.single {
                        self.insert_char(' ');
                    } else {
                        self.newline();
                    }
                }
                '\t' => self.insert_char(' '),
                c if c.is_control() => {}
                c => self.insert_char(c),
            }
        }
    }

    fn insert_char(&mut self, c: char) {
        let i = byte_at(&self.lines[self.row], self.col);
        self.lines[self.row].insert(i, c);
        self.col += 1;
    }

    /// Split the line at the cursor.
    pub fn newline(&mut self) {
        if self.single {
            return;
        }
        let i = byte_at(&self.lines[self.row], self.col);
        let rest = self.lines[self.row].split_off(i);
        self.row += 1;
        self.lines.insert(self.row, rest);
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let i = byte_at(&self.lines[self.row], self.col);
            self.lines[self.row].remove(i);
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].push_str(&cur);
        }
    }

    fn delete(&mut self) {
        if self.col < self.line_len() {
            let i = byte_at(&self.lines[self.row], self.col);
            self.lines[self.row].remove(i);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn word_left(&mut self) {
        if self.col == 0 {
            if self.row > 0 {
                self.row -= 1;
                self.col = self.line_len();
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut i = self.col;
        while i > 0 && class(chars[i - 1]) == Class::Space {
            i -= 1;
        }
        if i > 0 {
            let c = class(chars[i - 1]);
            while i > 0 && class(chars[i - 1]) == c {
                i -= 1;
            }
        }
        self.col = i;
    }

    fn word_right(&mut self) {
        let len = self.line_len();
        if self.col >= len {
            if self.row + 1 < self.lines.len() {
                self.row += 1;
                self.col = 0;
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut i = self.col;
        let c = class(chars[i]);
        if c != Class::Space {
            while i < len && class(chars[i]) == c {
                i += 1;
            }
        }
        while i < len && class(chars[i]) == Class::Space {
            i += 1;
        }
        self.col = i;
    }

    fn delete_word_back(&mut self) {
        let end = self.col;
        let (row, _) = (self.row, 0);
        self.word_left();
        if self.row == row {
            let a = byte_at(&self.lines[row], self.col);
            let b = byte_at(&self.lines[row], end);
            self.lines[row].replace_range(a..b, "");
        } else {
            // Crossed a line start: just join like Backspace.
            self.row = row;
            self.col = 0;
            self.backspace();
        }
    }

    fn move_row(&mut self, delta: isize) -> bool {
        let new = self.row.saturating_add_signed(delta);
        if new == self.row || new >= self.lines.len() {
            return false;
        }
        self.row = new;
        self.col = self.col.min(self.line_len());
        true
    }

    /// Whether Up / Down would move within the text.
    pub fn can_move(&self, up: bool) -> bool {
        if up {
            self.row > 0
        } else {
            self.row + 1 < self.lines.len()
        }
    }

    /// Handle an editing key; `false` = not an editing key (nothing happened).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                let i = byte_at(&self.lines[self.row], self.col);
                self.lines[self.row].replace_range(..i, "");
                self.col = 0;
            }
            KeyCode::Char('k') if ctrl => {
                let i = byte_at(&self.lines[self.row], self.col);
                self.lines[self.row].truncate(i);
            }
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = self.line_len(),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Char('b') if alt => self.word_left(),
            KeyCode::Char('f') if alt => self.word_right(),
            KeyCode::Char(c) if !ctrl && !alt => self.insert_char(c),
            KeyCode::Enter if !self.single => self.newline(),
            KeyCode::Backspace if ctrl => self.delete_word_back(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if ctrl || alt => self.word_left(),
            KeyCode::Right if ctrl || alt => self.word_right(),
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len();
                }
            }
            KeyCode::Right => {
                if self.col < self.line_len() {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Home if ctrl => {
                self.row = 0;
                self.col = 0;
            }
            KeyCode::End if ctrl => {
                self.row = self.lines.len() - 1;
                self.col = self.line_len();
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(),
            KeyCode::Up => {
                self.move_row(-1);
            }
            KeyCode::Down => {
                self.move_row(1);
            }
            _ => return false,
        }
        true
    }

    /// Row `row` clipped to `width` columns for display. The cursor row scrolls
    /// horizontally to keep the cursor visible and shows a block cursor when
    /// `focused`; other rows are cut with an ellipsis.
    pub fn render_line(
        &self,
        row: usize,
        width: usize,
        focused: bool,
        theme: &Theme,
    ) -> Line<'static> {
        let chars: Vec<char> = self
            .lines
            .get(row)
            .map_or(Vec::new(), |l| l.chars().collect());
        let width = width.max(1);
        if row != self.row || !focused {
            return Line::from(clip(&chars, 0, width));
        }
        // Reserve one cell for the cursor block at the end of the line.
        let start = (self.col + 1).saturating_sub(width);
        let shown: String = chars.iter().skip(start).take(width).collect();
        let cur = self.col - start;
        let before: String = shown.chars().take(cur).collect();
        let at: String = shown.chars().skip(cur).take(1).collect();
        let after: String = shown.chars().skip(cur + 1).collect();
        let at = if at.is_empty() { " ".to_owned() } else { at };
        Line::from(vec![
            Span::raw(before),
            Span::styled(at, theme.cursor),
            Span::raw(after),
        ])
    }
}

/// `chars[start..]` cut to `width` cells, ending with `…` when cut.
fn clip(chars: &[char], start: usize, width: usize) -> String {
    let rest = &chars[start.min(chars.len())..];
    if rest.len() <= width {
        rest.iter().collect()
    } else {
        let mut s: String = rest.iter().take(width.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn typing_and_newlines() {
        let mut e = Editor::new(false);
        for c in "ab".chars() {
            e.handle_key(k(KeyCode::Char(c)));
        }
        e.handle_key(k(KeyCode::Enter));
        e.handle_key(k(KeyCode::Char('c')));
        assert_eq!(e.text(), "ab\nc");
        assert_eq!(e.cursor(), (1, 1));
        // Enter splits a line.
        e.handle_key(k(KeyCode::Up));
        e.handle_key(k(KeyCode::Enter));
        assert_eq!(e.lines(), ["a", "b", "c"]);
        let mut single = Editor::new(true);
        assert!(!single.handle_key(k(KeyCode::Enter)));
        assert_eq!(single.text(), "");
    }

    #[test]
    fn backspace_and_delete_join_lines() {
        let mut e = Editor::from_text("ab\ncd", false);
        e.handle_key(k(KeyCode::Home));
        e.handle_key(k(KeyCode::Backspace));
        assert_eq!(e.lines(), ["abcd"]);
        assert_eq!(e.cursor(), (0, 2));
        e.handle_key(k(KeyCode::Enter));
        e.handle_key(k(KeyCode::Up));
        e.handle_key(k(KeyCode::End));
        e.handle_key(k(KeyCode::Delete));
        assert_eq!(e.lines(), ["abcd"]);
        e.handle_key(k(KeyCode::Delete));
        assert_eq!(e.lines(), ["abd"]);
    }

    #[test]
    fn home_end_and_unicode() {
        let mut e = Editor::from_text("héllo wörld", true);
        e.handle_key(k(KeyCode::Home));
        e.handle_key(k(KeyCode::Right));
        e.handle_key(k(KeyCode::Right));
        e.handle_key(k(KeyCode::Delete));
        assert_eq!(e.text(), "héllo wörld".replacen('l', "", 1));
        e.handle_key(k(KeyCode::End));
        assert_eq!(e.cursor().1, e.text().chars().count());
        e.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(e.text(), "");
    }

    #[test]
    fn word_jumps() {
        let mut e = Editor::from_text("foo bar.baz  qux", true);
        let mut stops = Vec::new();
        for _ in 0..5 {
            e.handle_key(ctrl(KeyCode::Left));
            stops.push(e.cursor().1);
        }
        assert_eq!(stops, [13, 8, 7, 4, 0]);
        let mut stops = Vec::new();
        for _ in 0..5 {
            e.handle_key(ctrl(KeyCode::Right));
            stops.push(e.cursor().1);
        }
        assert_eq!(stops, [4, 7, 8, 13, 16]);
    }

    #[test]
    fn word_jump_crosses_lines_and_deletes_words() {
        let mut e = Editor::from_text("one two\nthree", false);
        e.handle_key(k(KeyCode::Home));
        e.handle_key(ctrl(KeyCode::Left));
        assert_eq!(e.cursor(), (0, 7));
        e.handle_key(ctrl(KeyCode::Right));
        assert_eq!(e.cursor(), (1, 0));
        e.handle_key(k(KeyCode::End));
        e.handle_key(ctrl(KeyCode::Char('w')));
        assert_eq!(e.lines(), ["one two", ""]);
        e.handle_key(ctrl(KeyCode::Char('w')));
        assert_eq!(e.lines(), ["one two"]);
        e.handle_key(ctrl(KeyCode::Char('w')));
        assert_eq!(e.lines(), ["one "]);
    }

    #[test]
    fn paste_keeps_lines() {
        let mut e = Editor::from_text("x", false);
        e.handle_key(k(KeyCode::Home));
        e.insert_str("a\nb\r\nc\rd\te");
        assert_eq!(e.lines(), ["a", "b", "c", "d ex"]);
        assert_eq!(e.cursor(), (3, 3));
        let mut s = Editor::new(true);
        s.insert_str("a\nb\r\nc");
        assert_eq!(s.text(), "a b c");
    }

    #[test]
    fn kill_to_end_and_vertical_moves() {
        let mut e = Editor::from_text("abc\nde", false);
        e.handle_key(k(KeyCode::Up));
        assert_eq!(e.cursor(), (0, 2), "column clamps");
        e.handle_key(ctrl(KeyCode::Char('k')));
        assert_eq!(e.lines(), ["ab", "de"]);
        assert!(e.can_move(false) && !e.can_move(true));
        e.handle_key(k(KeyCode::Down));
        e.handle_key(k(KeyCode::Down));
        assert_eq!(e.cursor().0, 1);
    }

    #[test]
    fn render_scrolls_horizontally() {
        let t = Theme::default_theme();
        let e = Editor::from_text("0123456789", true);
        let l = e.render_line(0, 5, true, &t);
        let s: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(s, "6789 ");
        let l = e.render_line(0, 5, false, &t);
        let s: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(s, "0123…");
    }
}
