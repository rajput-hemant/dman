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

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No colours (`NO_COLOR`, `color = "never"`): modifiers only.
    Mono,
    /// The 16 named ANSI colours.
    Ansi16,
    /// The xterm 256-colour palette.
    Ansi256,
    /// 24-bit colour.
    TrueColor,
}

impl ColorDepth {
    /// Depth from the `[ui] color` setting and the environment
    /// (`NO_COLOR`, `COLORTERM`, `TERM`).
    pub fn detect(setting: &str, env: &dyn Fn(&str) -> Option<String>) -> Self {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorDepth::Mono;
        }
        match setting {
            "never" | "mono" => return ColorDepth::Mono,
            "truecolor" | "24bit" => return ColorDepth::TrueColor,
            "256" => return ColorDepth::Ansi256,
            "16" => return ColorDepth::Ansi16,
            _ => {}
        }
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return ColorDepth::TrueColor;
        }
        let term = env("TERM").unwrap_or_default();
        if term.contains("256color") {
            ColorDepth::Ansi256
        } else if term.is_empty() || term == "dumb" {
            ColorDepth::Mono
        } else {
            ColorDepth::Ansi16
        }
    }
}

/// An RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parse `#rrggbb`.
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let [_, r, g, b] = v.to_be_bytes();
        Some(Rgb(r, g, b))
    }

    /// Linear mix towards `other` (`t` = 0 → self, 1 → other).
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        // Rounded channel values always fit in a u8.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    fn dist(self, o: Rgb) -> u32 {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).unsigned_abs();
        let (r, g, b) = (d(self.0, o.0), d(self.1, o.1), d(self.2, o.2));
        2 * r * r + 4 * g * g + 3 * b * b
    }

    /// Nearest xterm-256 index (6×6×6 cube or grey ramp).
    pub fn to_256(self) -> u8 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let idx = |c: u8| {
            LEVELS
                .iter()
                .enumerate()
                .min_by_key(|(_, l)| (i32::from(**l) - i32::from(c)).unsigned_abs())
                .map_or(0, |(i, _)| i)
        };
        let (ri, gi, bi) = (idx(self.0), idx(self.1), idx(self.2));
        let cube = Rgb(LEVELS[ri], LEVELS[gi], LEVELS[bi]);
        let avg = (u32::from(self.0) + u32::from(self.1) + u32::from(self.2)) / 3;
        let gi_ramp = (avg.saturating_sub(8) / 10).min(23);
        // gi_ramp ≤ 23, so the grey level fits in a u8.
        #[allow(clippy::cast_possible_truncation)]
        let grey_level = (8 + gi_ramp * 10) as u8;
        let grey = Rgb(grey_level, grey_level, grey_level);
        if self.dist(grey) < self.dist(cube) {
            #[allow(clippy::cast_possible_truncation)]
            let g = 232 + gi_ramp as u8;
            g
        } else {
            // ri, gi, bi < 6.
            #[allow(clippy::cast_possible_truncation)]
            let c = (16 + 36 * ri + 6 * gi + bi) as u8;
            c
        }
    }
}

/// A token: its RGB value and the named colour used on 16-colour terminals
/// (`Color::Reset` = no colour there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tok(pub Rgb, pub Color);

/// One colour per semantic token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Body text.
    pub text: Tok,
    /// Secondary text.
    pub muted: Tok,
    /// Hints, units, zero values, borders of inactive things.
    pub faint: Tok,
    /// Inactive pane borders.
    pub border: Tok,
    /// Focused pane / modal border.
    pub border_focus: Tok,
    /// The one accent: titles, active tab, keys, primary button.
    pub accent: Tok,
    /// Text on an accent background.
    pub accent_fg: Tok,
    /// Marked (multi-selected) rows; second graph series.
    pub mark: Tok,
    /// Raised surfaces: popups, key pills.
    pub surface: Tok,
    /// The cursor row background.
    pub selection: Tok,
    /// Done, seeding, ok.
    pub success: Tok,
    /// Paused, low disk, warnings.
    pub warning: Tok,
    /// Errors, destructive actions.
    pub danger: Tok,
    /// Active transfers, information.
    pub info: Tok,
    /// The empty part of progress bars.
    pub track: Tok,
    /// Drop shadow of modals.
    pub shadow: Tok,
    /// A light (dark-on-light) theme: decides how dimmed backdrops look.
    pub light: bool,
}

const fn t(r: u8, g: u8, b: u8, c: Color) -> Tok {
    Tok(Rgb(r, g, b), c)
}

impl Palette {
    /// `dman-dark`: calm blue-violet accent on muted slate chrome.
    pub const DMAN_DARK: Palette = Palette {
        text: t(0xd5, 0xd9, 0xea, Color::Reset),
        muted: t(0x9a, 0xa0, 0xbd, Color::Gray),
        faint: t(0x60, 0x66, 0x84, Color::DarkGray),
        border: t(0x3d, 0x43, 0x60, Color::DarkGray),
        border_focus: t(0x8c, 0x9c, 0xf8, Color::LightBlue),
        accent: t(0x8c, 0x9c, 0xf8, Color::LightBlue),
        accent_fg: t(0x16, 0x18, 0x26, Color::Black),
        mark: t(0xc0, 0xa6, 0xf7, Color::LightMagenta),
        surface: t(0x24, 0x28, 0x3c, Color::Reset),
        selection: t(0x2f, 0x35, 0x56, Color::Blue),
        success: t(0x93, 0xd3, 0x8c, Color::Green),
        warning: t(0xe8, 0xc1, 0x72, Color::Yellow),
        danger: t(0xf2, 0x88, 0x8f, Color::LightRed),
        info: t(0x7f, 0xc8, 0xec, Color::Cyan),
        track: t(0x2c, 0x31, 0x48, Color::DarkGray),
        shadow: t(0x0c, 0x0d, 0x14, Color::Black),
        light: false,
    };

    /// `dman-light`: the same tokens for light terminals.
    pub const DMAN_LIGHT: Palette = Palette {
        text: t(0x23, 0x27, 0x3a, Color::Reset),
        muted: t(0x55, 0x5a, 0x70, Color::DarkGray),
        faint: t(0x8c, 0x91, 0xa6, Color::DarkGray),
        border: t(0xc2, 0xc7, 0xda, Color::Gray),
        border_focus: t(0x3b, 0x5b, 0xdb, Color::Blue),
        accent: t(0x3b, 0x5b, 0xdb, Color::Blue),
        accent_fg: t(0xff, 0xff, 0xff, Color::White),
        mark: t(0x70, 0x48, 0xe8, Color::Magenta),
        surface: t(0xf3, 0xf4, 0xfa, Color::Reset),
        selection: t(0xdc, 0xe2, 0xf8, Color::Cyan),
        success: t(0x2b, 0x8a, 0x3e, Color::Green),
        warning: t(0xa8, 0x6b, 0x0c, Color::Yellow),
        danger: t(0xc9, 0x2a, 0x2a, Color::Red),
        info: t(0x15, 0x6f, 0xc4, Color::Blue),
        track: t(0xe0, 0xe4, 0xef, Color::Gray),
        shadow: t(0xb8, 0xbd, 0xcf, Color::Gray),
        light: true,
    };

    /// `tokyo-night`.
    pub const TOKYO_NIGHT: Palette = Palette {
        text: t(0xc0, 0xca, 0xf5, Color::Reset),
        muted: t(0xa9, 0xb1, 0xd6, Color::Gray),
        faint: t(0x56, 0x5f, 0x89, Color::DarkGray),
        border: t(0x3b, 0x42, 0x61, Color::DarkGray),
        border_focus: t(0x7a, 0xa2, 0xf7, Color::LightBlue),
        accent: t(0x7a, 0xa2, 0xf7, Color::LightBlue),
        accent_fg: t(0x1a, 0x1b, 0x26, Color::Black),
        mark: t(0xbb, 0x9a, 0xf7, Color::LightMagenta),
        surface: t(0x1f, 0x23, 0x35, Color::Reset),
        selection: t(0x28, 0x34, 0x57, Color::Blue),
        success: t(0x9e, 0xce, 0x6a, Color::Green),
        warning: t(0xe0, 0xaf, 0x68, Color::Yellow),
        danger: t(0xf7, 0x76, 0x8e, Color::LightRed),
        info: t(0x7d, 0xcf, 0xff, Color::Cyan),
        track: t(0x29, 0x2e, 0x42, Color::DarkGray),
        shadow: t(0x0f, 0x10, 0x17, Color::Black),
        light: false,
    };

    /// `high-contrast`: 16-colour-safe, no dim text.
    pub const HIGH_CONTRAST: Palette = Palette {
        text: t(0xff, 0xff, 0xff, Color::White),
        muted: t(0xff, 0xff, 0xff, Color::White),
        faint: t(0xc0, 0xc0, 0xc0, Color::Gray),
        border: t(0xff, 0xff, 0xff, Color::White),
        border_focus: t(0xff, 0xff, 0x00, Color::LightYellow),
        accent: t(0xff, 0xff, 0x00, Color::LightYellow),
        accent_fg: t(0x00, 0x00, 0x00, Color::Black),
        mark: t(0x00, 0xff, 0xff, Color::LightCyan),
        surface: t(0x00, 0x00, 0x00, Color::Black),
        selection: t(0xff, 0xff, 0xff, Color::White),
        success: t(0x00, 0xff, 0x00, Color::LightGreen),
        warning: t(0xff, 0xff, 0x00, Color::LightYellow),
        danger: t(0xff, 0x55, 0x55, Color::LightRed),
        info: t(0x00, 0xff, 0xff, Color::LightCyan),
        track: t(0x40, 0x40, 0x40, Color::DarkGray),
        shadow: t(0x00, 0x00, 0x00, Color::Black),
        light: false,
    };

    /// Set token `name` to `value` (`#rrggbb` or an ANSI colour name).
    ///
    /// # Errors
    /// The unknown token or the unparsable colour.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let tok = match name {
            "text" => &mut self.text,
            "muted" | "text_muted" => &mut self.muted,
            "faint" | "text_faint" => &mut self.faint,
            "border" => &mut self.border,
            "border_focus" => &mut self.border_focus,
            "accent" => &mut self.accent,
            "accent_fg" => &mut self.accent_fg,
            "mark" => &mut self.mark,
            "surface" | "surface_raised" => &mut self.surface,
            "selection" | "selection_bg" => &mut self.selection,
            "success" => &mut self.success,
            "warning" => &mut self.warning,
            "danger" => &mut self.danger,
            "info" => &mut self.info,
            "track" | "bar_track" => &mut self.track,
            "shadow" => &mut self.shadow,
            _ => return Err(format!("[ui.colors] unknown colour token '{name}'")),
        };
        if let Some(rgb) = Rgb::parse(value) {
            *tok = Tok(rgb, nearest_16(rgb));
            return Ok(());
        }
        match named(value) {
            Some((c, rgb)) => {
                *tok = Tok(rgb, c);
                Ok(())
            }
            None => Err(format!(
                "[ui.colors] {name}: '{value}' is not #rrggbb or an ANSI colour name"
            )),
        }
    }
}

/// The 16 named colours with typical xterm values.
const NAMED: [(&str, Color, Rgb); 16] = [
    ("black", Color::Black, Rgb(0, 0, 0)),
    ("red", Color::Red, Rgb(205, 0, 0)),
    ("green", Color::Green, Rgb(0, 205, 0)),
    ("yellow", Color::Yellow, Rgb(205, 205, 0)),
    ("blue", Color::Blue, Rgb(0, 0, 238)),
    ("magenta", Color::Magenta, Rgb(205, 0, 205)),
    ("cyan", Color::Cyan, Rgb(0, 205, 205)),
    ("gray", Color::Gray, Rgb(229, 229, 229)),
    ("darkgray", Color::DarkGray, Rgb(127, 127, 127)),
    ("lightred", Color::LightRed, Rgb(255, 0, 0)),
    ("lightgreen", Color::LightGreen, Rgb(0, 255, 0)),
    ("lightyellow", Color::LightYellow, Rgb(255, 255, 0)),
    ("lightblue", Color::LightBlue, Rgb(92, 92, 255)),
    ("lightmagenta", Color::LightMagenta, Rgb(255, 0, 255)),
    ("lightcyan", Color::LightCyan, Rgb(0, 255, 255)),
    ("white", Color::White, Rgb(255, 255, 255)),
];

fn named(name: &str) -> Option<(Color, Rgb)> {
    let n = name.to_ascii_lowercase().replace(['_', '-', ' '], "");
    let n = n.replace("grey", "gray");
    NAMED.iter().find(|x| x.0 == n).map(|x| (x.1, x.2))
}

fn nearest_16(rgb: Rgb) -> Color {
    NAMED
        .iter()
        .min_by_key(|x| rgb.dist(x.2))
        .map_or(Color::Reset, |x| x.1)
}

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

const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

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
}

impl Default for ThemeOptions {
    fn default() -> Self {
        Self {
            name: "dman-dark".into(),
            depth: ColorDepth::TrueColor,
            icons: IconSet::Unicode,
            borders: BorderKind::Rounded,
            colors: Vec::new(),
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
        let name = Theme::canonical(&opts.name).unwrap_or_else(|| {
            warnings.push(format!(
                "[ui] unknown theme '{}', using dman-dark (themes: {})",
                opts.name,
                THEMES.join(", ")
            ));
            "dman-dark"
        });
        let depth = opts.depth;
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
