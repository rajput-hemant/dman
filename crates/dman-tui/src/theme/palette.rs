//! Colour tokens: RGB values with a 16-colour fallback, the built-in
//! palettes, `[ui.colors]` overrides and colour-depth reduction.

use ratatui::style::Color;

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

pub(super) fn nearest_16(rgb: Rgb) -> Color {
    NAMED
        .iter()
        .min_by_key(|x| rgb.dist(x.2))
        .map_or(Color::Reset, |x| x.1)
}
