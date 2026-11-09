//! Progress bars and sparklines as spans (so they fit in table cells and
//! lines of text).
//!
//! Bars use 8 sub-cell steps (`▏▎▍▌▋▊▉█`) with a coloured track where the
//! terminal has background colours, `░` otherwise. Unknown sizes draw an
//! indeterminate shuttle that moves with the animation clock.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::theme::{ColorDepth, IconSet, Theme};

const PARTIAL: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const SPARK: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

fn track_style(theme: &Theme, fill: Style) -> (Style, Style, &'static str) {
    // (style of filled cells, style of empty cells, empty glyph)
    let bg_track = theme.depth >= ColorDepth::Ansi256 && theme.icons.set != IconSet::Ascii;
    if bg_track {
        let bg = theme.color(theme.palette.track);
        let mut f = fill;
        let mut e = Style::new();
        if let Some(c) = bg {
            f = f.bg(c);
            e = e.bg(c);
        }
        (f, e, " ")
    } else if theme.icons.set == IconSet::Ascii {
        (fill, theme.faint, ".")
    } else {
        (fill, theme.faint, "░")
    }
}

/// A bar of `width` cells filled to `fraction` (0..=1) in `fill`.
pub fn bar(theme: &Theme, width: usize, fraction: f64, fill: Style) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let (fs, es, empty) = track_style(theme, fill);
    let ascii = theme.icons.set == IconSet::Ascii;
    let f = fraction.clamp(0.0, 1.0);
    // Truncation intended: eighths of a cell, only full when reached.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let eighths = ((f * (width * 8) as f64).floor() as usize).min(width * 8);
    let full = eighths / 8;
    let part = eighths % 8;
    let mut out = Vec::with_capacity(3);
    if full > 0 {
        let glyph = if ascii { "#" } else { "█" };
        out.push(Span::styled(glyph.repeat(full), fs));
    }
    let mut used = full;
    if part > 0 && used < width {
        let glyph = if ascii {
            if part >= 4 { "#" } else { empty }
        } else {
            PARTIAL[part]
        };
        out.push(Span::styled(glyph, fs));
        used += 1;
    }
    if used < width {
        out.push(Span::styled(empty.repeat(width - used), es));
    }
    out
}

/// An indeterminate bar: a short block shuttling back and forth with the
/// animation time `ms` (static when `ms` is `None`).
pub fn shuttle(theme: &Theme, width: usize, ms: Option<u64>, fill: Style) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let (fs, es, _) = track_style(theme, fill);
    let ascii = theme.icons.set == IconSet::Ascii;
    let dot = if ascii { "-" } else { "⠒" };
    let seg = width.min(3);
    let span = width - seg;
    let pos = match ms {
        Some(ms) if span > 0 => {
            let period = span * 2;
            // < period, which fits in usize.
            #[allow(clippy::cast_possible_truncation)]
            let step = ((ms / 60) % period as u64) as usize;
            if step <= span { step } else { period - step }
        }
        _ => 0,
    };
    let block = if ascii { "=" } else { "■" };
    let es = es.patch(theme.faint);
    let mut out = Vec::new();
    if pos > 0 {
        out.push(Span::styled(dot.repeat(pos), es));
    }
    out.push(Span::styled(block.repeat(seg), fs));
    if width > pos + seg {
        out.push(Span::styled(dot.repeat(width - pos - seg), es));
    }
    out
}

/// A one-row block sparkline of the last `width` values (newest right),
/// scaled to the largest value shown (or `max` if larger).
pub fn sparkline(values: &[u64], width: usize, max: u64) -> String {
    if width == 0 {
        return String::new();
    }
    let from = values.len().saturating_sub(width);
    let shown = &values[from..];
    let top = shown.iter().copied().max().unwrap_or(0).max(max).max(1);
    let mut out = String::with_capacity(width * 3);
    out.extend(std::iter::repeat_n(" ", width - shown.len()));
    for v in shown {
        // ≤ 8 by construction.
        #[allow(clippy::cast_possible_truncation)]
        let level = if *v == 0 {
            0
        } else {
            ((u128::from(*v) * 8).div_ceil(u128::from(top)) as usize).clamp(1, 8)
        };
        out.push_str(SPARK[level]);
    }
    out
}

/// A one-row braille area graph: two samples per cell, four levels. The last
/// `width * 2` values are shown, newest right.
pub fn braille(values: &[u64], width: usize, max: u64) -> String {
    if width == 0 {
        return String::new();
    }
    let n = width * 2;
    let from = values.len().saturating_sub(n);
    let shown = &values[from..];
    let top = shown.iter().copied().max().unwrap_or(0).max(max).max(1);
    let level = |v: u64| -> u32 {
        if v == 0 {
            0
        } else {
            // ≤ 4 by construction.
            #[allow(clippy::cast_possible_truncation)]
            let l = (u128::from(v) * 4).div_ceil(u128::from(top)) as u32;
            l.clamp(1, 4)
        }
    };
    // Dots of a column, bottom-up: left 7,3,2,1 (bits 6,2,1,0), right 8,6,5,4
    // (bits 7,5,4,3).
    const LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
    const RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
    let pad = n - shown.len();
    let at = |i: usize| -> u64 { if i < pad { 0 } else { shown[i - pad] } };
    let mut out = String::with_capacity(width * 3);
    for c in 0..width {
        let (l, r) = (level(at(c * 2)), level(at(c * 2 + 1)));
        let mut bits = 0;
        for k in 0..4 {
            if l > k {
                bits |= LEFT[k as usize];
            }
            if r > k {
                bits |= RIGHT[k as usize];
            }
        }
        // Base braille (empty) when there is nothing to show keeps a baseline.
        let base = if bits == 0 {
            0x2800 + 0x40 + 0x80
        } else {
            0x2800
        };
        out.push(char::from_u32(base + bits).unwrap_or(' '));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn sub_cell_bars() {
        let t = Theme::default_theme();
        let s = |f| text(&bar(&t, 10, f, t.info));
        assert_eq!(s(0.0), " ".repeat(10));
        assert_eq!(s(1.0), "█".repeat(10));
        // 66% and 62% differ now.
        assert_ne!(s(0.66), s(0.62));
        assert_eq!(s(0.66).chars().count(), 10);
        assert!(s(0.66).starts_with("██████▌"), "{}", s(0.66));
        let mono = Theme::mono();
        assert_eq!(text(&bar(&mono, 4, 0.5, mono.info)), "██░░");
    }

    #[test]
    fn shuttle_moves_and_stays_in_width() {
        let t = Theme::default_theme();
        let a = text(&shuttle(&t, 10, Some(0), t.info));
        let b = text(&shuttle(&t, 10, Some(300), t.info));
        assert_ne!(a, b);
        for ms in (0..5000).step_by(60) {
            assert_eq!(text(&shuttle(&t, 10, Some(ms), t.info)).chars().count(), 10);
        }
    }

    #[test]
    fn sparklines() {
        assert_eq!(sparkline(&[0, 4, 8], 3, 0), " ▄█");
        assert_eq!(sparkline(&[8], 3, 0), "  █");
        assert_eq!(braille(&[], 2, 0).chars().count(), 2);
        assert_eq!(braille(&[4, 4, 4, 4], 2, 0), "⣿⣿");
    }
}
