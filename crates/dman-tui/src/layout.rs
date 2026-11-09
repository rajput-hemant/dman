//! Screen layout, responsive breakpoints and the per-frame mouse hit map.
//!
//! Regions:
//! * header (1 row): brand, compact throughput, counters, connection;
//! * throughput card (4 rows, downloads view on terminals with ≥ 30 rows);
//! * context row: filter chips in the list, a breadcrumb elsewhere;
//! * banners (disconnected, engine unavailable);
//! * the body (top view) and the optional expanded speed graph;
//! * footer (mode pill, key hints).
//!
//! Hit testing never re-derives geometry: everything clickable records its
//! rectangle while it is drawn ([`crate::app::State::hit`]), and mouse events
//! look the point up in that map ([`crate::app::State::hit_at`]).

use ratatui::layout::{Constraint, Layout, Rect};

use crate::action::Action;
use crate::app::State;
use crate::views::View;
use crate::widgets::status::banners;

/// Below this size only a "terminal too small" note is drawn.
pub const MIN_SIZE: (u16, u16) = (40, 10);

/// Width classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Breakpoint {
    /// Fewer than 100 columns (80×24 baseline): short units, fewer columns.
    Compact,
    /// 100–139 columns.
    Regular,
    /// 140 columns or more.
    Wide,
}

impl Breakpoint {
    /// Class of a width.
    pub const fn of(cols: u16) -> Breakpoint {
        if cols < 100 {
            Breakpoint::Compact
        } else if cols < 140 {
            Breakpoint::Regular
        } else {
            Breakpoint::Wide
        }
    }
}

/// Body width from which lists get a preview pane.
pub const PREVIEW_MIN_COLS: u16 = 110;

/// Something clickable drawn in the last frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// A display row of the downloads list (index into `State::list.rows`).
    ListRow(usize),
    /// The downloads list scrollbar.
    ListScroll,
    /// A filter chip.
    Chip(crate::list::Chip),
    /// A detail tab.
    DetailTab(crate::views::detail::Tab),
    /// Run an action (footer hints, empty-state pills).
    Action(Action),
    /// A toast (click runs its action or dismisses it).
    Toast(u64),
    /// A modal button (0 = cancel, 1 = confirm).
    Button(usize),
    /// A palette entry (index into the matches).
    PaletteItem(usize),
    /// A search result row (index into the shown rows).
    SearchRow(usize),
    /// The search results scrollbar.
    SearchScroll,
    /// The mouse indicator in the header (toggles mouse support).
    Mouse,
}

/// The fixed screen areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenLayout {
    /// Header line.
    pub header: Rect,
    /// Throughput card (zero height when not shown).
    pub card: Rect,
    /// Chips / breadcrumb row.
    pub context: Rect,
    /// Banner lines (zero height when there is nothing to report).
    pub banner: Rect,
    /// The top view's area.
    pub body: Rect,
    /// The expanded global speed graph (zero height unless shown).
    pub graph: Rect,
    /// Footer.
    pub footer: Rect,
}

impl ScreenLayout {
    /// Areas for a terminal of `size` (columns, rows).
    pub fn compute(size: (u16, u16), state: &State) -> Self {
        let area = Rect::new(0, 0, size.0, size.1);
        let banner = u16::try_from(banners(state).len()).unwrap_or(0);
        let card = if card_shown(size, state) { 4 } else { 0 };
        let [header, card, context, banner, body, graph, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(card),
            Constraint::Length(1),
            Constraint::Length(banner),
            Constraint::Fill(1),
            Constraint::Length(graph_height(size.1, state)),
            Constraint::Length(1),
        ])
        .areas(area);
        Self {
            header,
            card,
            context,
            banner,
            body,
            graph,
            footer,
        }
    }
}

/// Whether the throughput card is drawn (downloads view, tall terminals).
pub fn card_shown(size: (u16, u16), state: &State) -> bool {
    size.1 >= 30 && size.0 >= 60 && matches!(state.view(), View::Downloads)
}

/// Height of the expanded speed graph: a quarter of the screen (5..=10 rows),
/// and none on screens too small to leave room for the list.
fn graph_height(rows: u16, state: &State) -> u16 {
    let shown = state.graph.visible && matches!(state.view(), View::Downloads);
    if shown && rows >= 16 {
        (rows / 4).clamp(5, 10)
    } else {
        0
    }
}

/// Split a list body into the list and (on wide terminals) a preview pane.
pub fn split_preview(body: Rect, enabled: bool) -> (Rect, Option<Rect>) {
    if !enabled || body.width < PREVIEW_MIN_COLS || body.height < 8 {
        return (body, None);
    }
    let preview = (body.width * 2 / 5).clamp(40, 64);
    let [list, pane] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(preview)]).areas(body);
    (list, Some(pane))
}
