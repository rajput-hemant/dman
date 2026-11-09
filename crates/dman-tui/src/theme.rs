//! Colour themes. `mono` uses only text modifiers and is forced by `NO_COLOR`.

use ratatui::style::{Color, Modifier, Style};

/// Styles used by every view. Views must take colours from here only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Theme name (`default` or `mono`).
    pub name: &'static str,
    /// Plain text.
    pub text: Style,
    /// Secondary text, and all data while disconnected.
    pub dim: Style,
    /// Titles and highlighted labels.
    pub accent: Style,
    /// Table header row.
    pub header: Style,
    /// Section header rows (Active / Waiting / Stopped).
    pub section: Style,
    /// The row under the cursor.
    pub cursor: Style,
    /// Selected (marked) rows.
    pub marked: Style,
    /// Success messages, active transfers.
    pub ok: Style,
    /// Warnings, paused jobs.
    pub warn: Style,
    /// Errors.
    pub error: Style,
    /// Banner for problems (disconnected, engine unavailable).
    pub banner_error: Style,
    /// Banner for transient states (connecting).
    pub banner_warn: Style,
    /// Modal borders.
    pub border: Style,
    /// Key names in the footer and help.
    pub key: Style,
}

impl Theme {
    /// The coloured default theme.
    pub fn default_theme() -> Self {
        Self {
            name: "default",
            text: Style::new(),
            dim: Style::new().fg(Color::DarkGray),
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            header: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            section: Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            cursor: Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
            marked: Style::new().fg(Color::LightYellow),
            ok: Style::new().fg(Color::Green),
            warn: Style::new().fg(Color::Yellow),
            error: Style::new().fg(Color::Red),
            banner_error: Style::new().fg(Color::White).bg(Color::Red),
            banner_warn: Style::new().fg(Color::Black).bg(Color::Yellow),
            border: Style::new().fg(Color::Cyan),
            key: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        }
    }

    /// Monochrome theme: modifiers only.
    pub fn mono() -> Self {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        Self {
            name: "mono",
            text: Style::new(),
            dim: Style::new().add_modifier(Modifier::DIM),
            accent: bold,
            header: bold.add_modifier(Modifier::UNDERLINED),
            section: bold,
            cursor: Style::new().add_modifier(Modifier::REVERSED),
            marked: bold,
            ok: Style::new(),
            warn: bold,
            error: bold,
            banner_error: Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            banner_warn: Style::new().add_modifier(Modifier::REVERSED),
            border: Style::new(),
            key: bold,
        }
    }

    /// Theme by name; unknown names fall back to `default`. `no_color` forces mono.
    pub fn resolve(name: &str, no_color: bool) -> Self {
        if no_color || name == "mono" {
            Self::mono()
        } else {
            Self::default_theme()
        }
    }

    /// Whether `NO_COLOR` is set to a non-empty value (<https://no-color.org>).
    pub fn no_color_env() -> bool {
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_theme()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_forces_mono() {
        assert_eq!(Theme::resolve("default", true).name, "mono");
        assert_eq!(Theme::resolve("default", false).name, "default");
        assert_eq!(Theme::resolve("mono", false).name, "mono");
        assert_eq!(Theme::resolve("nope", false).name, "default");
    }

    #[test]
    fn mono_has_no_colours() {
        let t = Theme::mono();
        for s in [t.text, t.dim, t.cursor, t.error, t.banner_error, t.key] {
            assert_eq!(s.fg, None);
            assert_eq!(s.bg, None);
        }
    }
}
