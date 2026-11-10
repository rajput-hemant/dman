//! Colour themes built from semantic tokens.
//!
//! A [`Palette`] holds one colour per token (text, accent, success, …) as RGB
//! plus a hand-picked 16-colour fallback. [`Theme::build`] turns a palette into
//! ready-made [`Style`]s for the detected [`ColorDepth`]: truecolor as is,
//! 256 colours by nearest cube/grey index, 16 colours from the fallback table,
//! and `mono` (forced by `NO_COLOR`) with modifiers only. Views take colours
//! from the theme only.

use dman_proto::JobState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::widgets::{Block, Borders};

pub mod file;
mod icons;
mod palette;

use icons::ASCII_BORDER;
pub use icons::{BorderKind, IconSet, Icons};
use palette::nearest_16;
pub use palette::{ColorDepth, Palette, Rgb, Tok};

/// What a theme is built from (the `[ui]` settings after parsing).
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeOptions {
    /// Theme name.
    pub name: String,
    /// Colour depth.
    pub depth: ColorDepth,
    /// Glyphs.
    pub icons: IconSet,
    /// Borders.
    pub borders: BorderKind,
    /// `[ui.colors]` overrides.
    pub colors: Vec<(String, String)>,
    /// Where custom theme files live (`<config dir>/themes`).
    pub themes_dir: Option<std::path::PathBuf>,
}

impl Default for ThemeOptions {
    fn default() -> Self {
        Self {
            name: "dman-dark".into(),
            depth: ColorDepth::TrueColor,
            icons: IconSet::Unicode,
            borders: BorderKind::Rounded,
            colors: Vec::new(),
            themes_dir: None,
        }
    }
}

/// The built-in theme names.
pub const THEMES: &[&str] = &[
    "dman-dark",
    "dman-light",
    "high-contrast",
    "tokyo-night",
    "mono",
];

/// Styles used by every view. Views must take colours from here only.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Theme name.
    pub name: &'static str,
    /// Colour depth the styles were built for.
    pub depth: ColorDepth,
    /// The palette (for blends such as row flashes).
    pub palette: Palette,
    /// Glyphs.
    pub icons: Icons,
    /// Border glyphs.
    pub border_set: border::Set<'static>,
    /// Plain text.
    pub text: Style,
    /// Secondary text, and all data while disconnected.
    pub dim: Style,
    /// Hints, units and zero values (dimmer than `dim`).
    pub faint: Style,
    /// Titles and highlighted labels.
    pub accent: Style,
    /// Table header row.
    pub header: Style,
    /// Section rules (Active / Waiting / Stopped).
    pub section: Style,
    /// The row under the cursor.
    pub cursor: Style,
    /// The bar at the left edge of the cursor row.
    pub cursor_bar: Style,
    /// Selected (marked) rows.
    pub marked: Style,
    /// Success messages, seeding, done.
    pub ok: Style,
    /// Warnings, paused jobs.
    pub warn: Style,
    /// Errors.
    pub error: Style,
    /// Active transfers, information.
    pub info: Style,
    /// Banner for problems (disconnected, engine unavailable).
    pub banner_error: Style,
    /// Banner for transient states (connecting).
    pub banner_warn: Style,
    /// Inactive borders.
    pub border: Style,
    /// Focused pane / modal borders.
    pub border_focus: Style,
    /// Borders of destructive dialogs.
    pub border_danger: Style,
    /// Key names in the footer and help.
    pub key: Style,
    /// A key drawn as a pill (`[ p ]`).
    pub pill: Style,
    /// The mode pill at the left of the footer.
    pub mode: Style,
    /// The primary call to action of a view (e.g. "Enter  Add 1 download").
    pub action: Style,
    /// A destructive button with focus.
    pub danger_button: Style,
    /// Popup / modal background.
    pub surface: Style,
    /// The empty part of progress bars.
    pub bar_track: Style,
    /// Download series of graphs.
    pub graph_down: Style,
    /// Upload series of graphs.
    pub graph_up: Style,
    /// The drop shadow of modals.
    pub shadow: Style,
    /// Text of everything under a modal (the dimmed backdrop).
    pub backdrop: Style,
    /// Fuzzy-match highlights.
    pub matched: Style,
}

impl Theme {
    /// The coloured default theme (`dman-dark`, truecolor, Unicode, rounded).
    pub fn default_theme() -> Self {
        Self::build(&ThemeOptions::default())
    }

    /// Monochrome theme: modifiers only.
    pub fn mono() -> Self {
        Self::build(&ThemeOptions {
            name: "mono".into(),
            depth: ColorDepth::Mono,
            ..ThemeOptions::default()
        })
    }

    /// Theme by name; unknown names fall back to `dman-dark`. `no_color`
    /// forces mono.
    pub fn resolve(name: &str, no_color: bool) -> Self {
        Self::build(&ThemeOptions {
            name: name.to_owned(),
            depth: if no_color {
                ColorDepth::Mono
            } else {
                ColorDepth::TrueColor
            },
            ..ThemeOptions::default()
        })
    }

    /// Whether `NO_COLOR` is set to a non-empty value (<https://no-color.org>).
    pub fn no_color_env() -> bool {
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
    }

    /// The canonical name of a theme setting, or `None` when unknown
    /// (`default` is the old name of `dman-dark`).
    pub fn canonical(name: &str) -> Option<&'static str> {
        match name {
            "default" | "" | "dman-dark" | "dark" => Some("dman-dark"),
            "dman-light" | "light" => Some("dman-light"),
            "high-contrast" | "contrast" => Some("high-contrast"),
            "tokyo-night" | "tokyonight" => Some("tokyo-night"),
            "mono" => Some("mono"),
            _ => None,
        }
    }

    /// Build the theme for `opts`; colour overrides that do not parse are
    /// skipped (see [`Theme::build_with_warnings`]).
    pub fn build(opts: &ThemeOptions) -> Self {
        Self::build_with_warnings(opts).0
    }

    /// Build the theme and report problems with the options.
    pub fn build_with_warnings(opts: &ThemeOptions) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let depth = opts.depth;
        let mut file_colors: Vec<(String, String)> = Vec::new();
        let name = match Theme::canonical(&opts.name) {
            Some(name) => name,
            None => {
                let custom = (depth != ColorDepth::Mono)
                    .then_some(opts.themes_dir.as_deref())
                    .flatten()
                    .filter(|_| file::is_file_name(&opts.name))
                    .filter(|d| file::path(d, &opts.name).exists())
                    .map(|d| file::load(d, &opts.name));
                match custom {
                    Some(Ok(f)) => {
                        let label = format!("theme '{}'", opts.name);
                        warnings.extend(f.warnings.iter().map(|w| format!("{label}: {w}")));
                        file_colors = f.colors;
                        f.inherits
                    }
                    Some(Err(e)) => {
                        warnings.push(format!("[ui] theme file: {e}; using dman-dark"));
                        "dman-dark"
                    }
                    None => {
                        warnings.push(format!(
                            "[ui] unknown theme '{}', using dman-dark (themes: {}{})",
                            opts.name,
                            THEMES.join(", "),
                            opts.themes_dir.as_ref().map_or(String::new(), |d| format!(
                                "; custom ones go in {}",
                                d.display()
                            ))
                        ));
                        "dman-dark"
                    }
                }
            }
        };
        let name = if depth == ColorDepth::Mono {
            "mono"
        } else {
            name
        };
        let mut palette = match name {
            "dman-light" => Palette::DMAN_LIGHT,
            "tokyo-night" => Palette::TOKYO_NIGHT,
            "high-contrast" => Palette::HIGH_CONTRAST,
            _ => Palette::DMAN_DARK,
        };
        for (k, v) in &file_colors {
            if let Err(e) = palette.set(k, v) {
                warnings.push(e.replace("[ui.colors]", &format!("theme '{}'", opts.name)));
            }
        }
        for (k, v) in &opts.colors {
            if let Err(e) = palette.set(k, v) {
                warnings.push(e);
            }
        }
        let high = name == "high-contrast";
        let borders = if high && opts.borders == BorderKind::Rounded {
            BorderKind::Thick
        } else {
            opts.borders
        };
        let border_set = match borders {
            BorderKind::Rounded => border::ROUNDED,
            BorderKind::Plain => border::PLAIN,
            BorderKind::Thick => border::THICK,
            BorderKind::Ascii => ASCII_BORDER,
        };
        let icons = Icons::of(opts.icons);
        let theme = if depth == ColorDepth::Mono {
            Self::mono_styles(name, palette, icons, border_set)
        } else {
            Self::color_styles(name, depth, palette, icons, border_set, high)
        };
        (theme, warnings)
    }

    /// A token as a colour at this depth (`None` = leave the terminal default).
    pub fn color(&self, tok: Tok) -> Option<Color> {
        to_color(self.depth, tok)
    }

    /// An arbitrary RGB colour at this depth (used for blends).
    pub fn rgb(&self, rgb: Rgb) -> Option<Color> {
        to_color(self.depth, Tok(rgb, nearest_16(rgb)))
    }

    fn mono_styles(
        name: &'static str,
        palette: Palette,
        icons: Icons,
        border_set: border::Set<'static>,
    ) -> Self {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let rev = Style::new().add_modifier(Modifier::REVERSED);
        Self {
            name,
            depth: ColorDepth::Mono,
            palette,
            icons,
            border_set,
            text: Style::new(),
            dim,
            faint: dim,
            accent: bold,
            header: bold.add_modifier(Modifier::UNDERLINED),
            section: bold,
            cursor: rev,
            cursor_bar: bold,
            marked: bold,
            ok: Style::new(),
            warn: bold,
            error: bold,
            info: Style::new(),
            banner_error: rev.add_modifier(Modifier::BOLD),
            banner_warn: rev,
            border: Style::new(),
            border_focus: bold,
            border_danger: bold,
            key: bold,
            pill: rev,
            mode: rev.add_modifier(Modifier::BOLD),
            action: rev.add_modifier(Modifier::BOLD),
            danger_button: rev.add_modifier(Modifier::BOLD),
            surface: Style::new(),
            bar_track: dim,
            graph_down: Style::new(),
            graph_up: dim,
            shadow: Style::new(),
            backdrop: dim,
            matched: bold.add_modifier(Modifier::UNDERLINED),
        }
    }

    fn color_styles(
        name: &'static str,
        depth: ColorDepth,
        p: Palette,
        icons: Icons,
        border_set: border::Set<'static>,
        high: bool,
    ) -> Self {
        let c = |tok: Tok| to_color(depth, tok);
        let fg = |tok: Tok| match c(tok) {
            Some(col) => Style::new().fg(col),
            None => Style::new(),
        };
        let bg = |s: Style, tok: Tok| match c(tok) {
            Some(col) => s.bg(col),
            None => s,
        };
        let bold = Modifier::BOLD;
        let text = fg(p.text);
        let cursor = if high {
            Style::new().add_modifier(Modifier::REVERSED | bold)
        } else if depth == ColorDepth::Ansi16 {
            bg(Style::new().fg(Color::White), p.selection)
        } else {
            bg(fg(p.text), p.selection)
        };
        let pill = if depth == ColorDepth::Ansi16 || high {
            fg(p.accent).add_modifier(bold | Modifier::REVERSED)
        } else {
            bg(fg(p.accent).add_modifier(bold), p.surface)
        };
        let solid = |tok: Tok| {
            let s = bg(fg(p.accent_fg), tok).add_modifier(bold);
            if c(p.accent_fg).is_none() {
                s.add_modifier(Modifier::REVERSED)
            } else {
                s
            }
        };
        let surface = if depth == ColorDepth::Ansi16 {
            Style::new()
        } else {
            bg(fg(p.text), p.surface)
        };
        let shadow = if depth == ColorDepth::Ansi16 {
            Style::new().bg(Color::Black)
        } else {
            bg(Style::new(), p.shadow)
        };
        Self {
            name,
            depth,
            palette: p,
            icons,
            border_set,
            text,
            dim: fg(p.muted),
            faint: if high { fg(p.muted) } else { fg(p.faint) },
            accent: fg(p.accent).add_modifier(bold),
            header: fg(p.muted).add_modifier(bold),
            section: fg(p.faint),
            cursor,
            cursor_bar: fg(p.accent).add_modifier(bold),
            marked: fg(p.mark),
            ok: fg(p.success),
            warn: fg(p.warning),
            error: fg(p.danger),
            info: fg(p.info),
            banner_error: solid(p.danger),
            banner_warn: solid(p.warning),
            border: fg(p.border),
            border_focus: fg(p.border_focus),
            border_danger: fg(p.danger),
            key: fg(p.accent).add_modifier(bold),
            pill,
            mode: solid(p.accent),
            action: solid(p.success),
            danger_button: solid(p.danger),
            surface,
            bar_track: fg(p.track),
            graph_down: fg(p.accent),
            graph_up: fg(p.mark),
            shadow,
            backdrop: if high {
                fg(p.faint)
            } else {
                fg(p.faint).add_modifier(Modifier::DIM)
            },
            matched: fg(p.accent).add_modifier(bold | Modifier::UNDERLINED),
        }
    }

    /// Colour of a job state (glyph, bar fill).
    pub fn state(&self, s: JobState) -> Style {
        match s {
            JobState::Active => self.info,
            JobState::Seeding | JobState::Complete => self.ok,
            JobState::Waiting | JobState::Removed => self.dim,
            JobState::Paused => self.warn,
            JobState::Error => self.error,
        }
    }

    /// A bordered block with the theme's border glyphs, inactive colour.
    pub fn block(&self) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_set(self.border_set)
            .border_style(self.border)
    }

    /// A bordered block, focused (accent border) or not.
    pub fn block_focus(&self, focused: bool) -> Block<'static> {
        self.block().border_style(if focused {
            self.border_focus
        } else {
            self.border
        })
    }

    /// A raised block for popups (surface background, focus border).
    pub fn popup(&self) -> Block<'static> {
        self.block()
            .border_style(self.border_focus.patch(self.surface_bg()))
            .style(self.surface)
    }

    /// Only the background of the popup surface.
    pub fn surface_bg(&self) -> Style {
        match self.surface.bg {
            Some(c) => Style::new().bg(c),
            None => Style::new(),
        }
    }
}

fn to_color(depth: ColorDepth, tok: Tok) -> Option<Color> {
    match depth {
        ColorDepth::Mono => None,
        ColorDepth::Ansi16 => (tok.1 != Color::Reset).then_some(tok.1),
        ColorDepth::Ansi256 => Some(Color::Indexed(tok.0.to_256())),
        ColorDepth::TrueColor => Some(Color::Rgb(tok.0.0, tok.0.1, tok.0.2)),
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
        assert_eq!(Theme::resolve("default", false).name, "dman-dark");
        assert_eq!(Theme::resolve("mono", false).name, "mono");
        assert_eq!(Theme::resolve("nope", false).name, "dman-dark");
    }

    #[test]
    fn mono_has_no_colours() {
        let t = Theme::mono();
        for s in [
            t.text,
            t.dim,
            t.cursor,
            t.error,
            t.banner_error,
            t.key,
            t.action,
            t.pill,
            t.surface,
            t.bar_track,
        ] {
            assert_eq!(s.fg, None);
            assert_eq!(s.bg, None);
        }
    }

    #[test]
    fn depth_detection() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(a, _)| *a == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let d = |s: &str, e: &dyn Fn(&str) -> Option<String>| ColorDepth::detect(s, e);
        assert_eq!(
            d("auto", &env(&[("COLORTERM", "truecolor")])),
            ColorDepth::TrueColor
        );
        assert_eq!(
            d("auto", &env(&[("TERM", "xterm-256color")])),
            ColorDepth::Ansi256
        );
        assert_eq!(d("auto", &env(&[("TERM", "linux")])), ColorDepth::Ansi16);
        assert_eq!(d("truecolor", &env(&[("NO_COLOR", "1")])), ColorDepth::Mono);
        assert_eq!(d("16", &env(&[])), ColorDepth::Ansi16);
        assert_eq!(d("never", &env(&[])), ColorDepth::Mono);
    }

    #[test]
    fn colour_conversion() {
        assert_eq!(Rgb(0, 0, 0).to_256(), 16);
        assert_eq!(Rgb(255, 255, 255).to_256(), 231);
        assert_eq!(Rgb(128, 128, 128).to_256(), 244);
        assert_eq!(Rgb::parse("#7aa2f7"), Some(Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(Rgb::parse("7aa2f7"), None);
        let t16 = Theme::build(&ThemeOptions {
            depth: ColorDepth::Ansi16,
            ..ThemeOptions::default()
        });
        assert_eq!(t16.accent.fg, Some(Color::LightBlue));
        let t256 = Theme::build(&ThemeOptions {
            depth: ColorDepth::Ansi256,
            ..ThemeOptions::default()
        });
        assert!(matches!(t256.accent.fg, Some(Color::Indexed(_))));
    }

    #[test]
    fn overrides_and_unknown_names_warn() {
        let (t, w) = Theme::build_with_warnings(&ThemeOptions {
            name: "solarized".into(),
            colors: vec![
                ("accent".into(), "#ff0000".into()),
                ("nope".into(), "#000000".into()),
                ("danger".into(), "lightmagenta".into()),
                ("info".into(), "bogus".into()),
            ],
            ..ThemeOptions::default()
        });
        assert_eq!(t.name, "dman-dark");
        assert_eq!(t.accent.fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(t.error.fg, Some(Color::Rgb(255, 0, 255)));
        assert_eq!(w.len(), 3, "{w:?}");
    }

    #[test]
    fn every_builtin_theme_builds() {
        for name in THEMES {
            let t = Theme::build(&ThemeOptions {
                name: (*name).into(),
                ..ThemeOptions::default()
            });
            assert_eq!(t.name, *name);
        }
    }
}
