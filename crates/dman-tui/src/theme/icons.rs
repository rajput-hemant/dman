//! Glyph sets (unicode, nerd, ascii) and border sets.

use dman_proto::JobState;
use ratatui::symbols::border;

/// Which glyphs to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSet {
    /// Plain Unicode symbols (default; work in every modern font).
    Unicode,
    /// Nerd Font glyphs (opt-in).
    Nerd,
    /// ASCII only (Linux console, odd fonts).
    Ascii,
}

/// Glyphs used across the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icons {
    /// The set these come from.
    pub set: IconSet,
    /// Downloading.
    pub active: &'static str,
    /// Seeding.
    pub seeding: &'static str,
    /// Queued.
    pub waiting: &'static str,
    /// Paused.
    pub paused: &'static str,
    /// Finished.
    pub done: &'static str,
    /// Failed.
    pub error: &'static str,
    /// Removed.
    pub removed: &'static str,
    /// Video source marker.
    pub video: &'static str,
    /// Marked row.
    pub marked: &'static str,
    /// Cursor bar at the left edge of the cursor row.
    pub cursor: &'static str,
    /// Download speed.
    pub down: &'static str,
    /// Upload speed.
    pub up: &'static str,
    /// Info toast.
    pub info: &'static str,
    /// Warning toast.
    pub warn: &'static str,
    /// Connected dot.
    pub online: &'static str,
    /// Disconnected dot.
    pub offline: &'static str,
    /// Spinner frames.
    pub spinner: &'static [&'static str],
    /// The search prompt.
    pub search: &'static str,
    /// Ellipsis.
    pub ellipsis: &'static str,
}

impl Icons {
    /// The glyphs of `set`.
    pub const fn of(set: IconSet) -> Icons {
        match set {
            IconSet::Unicode => Icons {
                set,
                active: "↓",
                seeding: "↑",
                waiting: "◔",
                paused: "⏸",
                done: "✓",
                error: "✗",
                removed: "⊘",
                video: "▶",
                marked: "●",
                cursor: "▌",
                down: "▼",
                up: "▲",
                info: "●",
                warn: "▲",
                online: "●",
                offline: "○",
                spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
                search: "⌕",
                ellipsis: "…",
            },
            IconSet::Nerd => Icons {
                set,
                active: "\u{f019}",
                seeding: "\u{f093}",
                waiting: "\u{f017}",
                paused: "\u{f04c}",
                done: "\u{f00c}",
                error: "\u{f00d}",
                removed: "\u{f05e}",
                video: "\u{f03d}",
                marked: "\u{f111}",
                cursor: "▌",
                down: "\u{f063}",
                up: "\u{f062}",
                info: "\u{f05a}",
                warn: "\u{f071}",
                online: "\u{f111}",
                offline: "\u{f10c}",
                spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
                search: "\u{f002}",
                ellipsis: "…",
            },
            IconSet::Ascii => Icons {
                set,
                active: "v",
                seeding: "^",
                waiting: ".",
                paused: "=",
                done: "+",
                error: "x",
                removed: "-",
                video: ">",
                marked: "*",
                cursor: ">",
                down: "v",
                up: "^",
                info: "i",
                warn: "!",
                online: "*",
                offline: "o",
                spinner: &["|", "/", "-", "\\"],
                search: "?",
                ellipsis: "~",
            },
        }
    }

    /// The glyph of a job state.
    pub const fn state(&self, s: JobState) -> &'static str {
        match s {
            JobState::Active => self.active,
            JobState::Seeding => self.seeding,
            JobState::Waiting => self.waiting,
            JobState::Paused => self.paused,
            JobState::Complete => self.done,
            JobState::Error => self.error,
            JobState::Removed => self.removed,
        }
    }

    /// Spinner frame for animation time `ms`.
    pub fn spin(&self, ms: u64) -> &'static str {
        let n = self.spinner.len().max(1) as u64;
        // The index is < spinner.len().
        #[allow(clippy::cast_possible_truncation)]
        let i = ((ms / 100) % n) as usize;
        self.spinner.get(i).copied().unwrap_or("*")
    }
}

/// Border drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderKind {
    /// `╭─╮` (default).
    Rounded,
    /// `┌─┐`.
    Plain,
    /// `┏━┓`.
    Thick,
    /// `+-+`.
    Ascii,
}

pub(super) const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};
