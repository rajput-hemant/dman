//! Screen layout shared by rendering and mouse hit-testing.

use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::State;
use crate::widgets::status::banners;

/// The fixed screen areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenLayout {
    /// Header line.
    pub header: Rect,
    /// Info bar.
    pub info: Rect,
    /// Banner lines (zero height when there is nothing to report).
    pub banner: Rect,
    /// The top view's area.
    pub body: Rect,
    /// Status line.
    pub status: Rect,
    /// Footer.
    pub footer: Rect,
}

impl ScreenLayout {
    /// Areas for a terminal of `size` (columns, rows).
    pub fn compute(size: (u16, u16), state: &State) -> Self {
        let area = Rect::new(0, 0, size.0, size.1);
        let banner = u16::try_from(banners(state).len()).unwrap_or(0);
        let [header, info, banner, body, status, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(banner),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        Self {
            header,
            info,
            banner,
            body,
            status,
            footer,
        }
    }
}
