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
//! Breakpoints (each view degrades on its own below these; nothing is
//! drawn outside the terminal at any size):
//! * below [`MIN_SIZE`] (30×8) only a "terminal too small" note;
//! * fewer than 64 columns: no mode pill in the footer; fewer than 100: short
//!   units and counters in the header, fewer list columns;
//! * [`PREVIEW_MIN_COLS`] (110) body columns: preview pane beside lists;
//! * 30 rows: throughput card and two-line list rows; detail keeps its full
//!   card from 18 body rows, the search form its three fields from about 16.
//!
//! Hit testing never re-derives geometry: everything clickable records its
//! rectangle while it is drawn ([`crate::app::State::hit`]), and mouse events
//! look the point up in that map ([`crate::app::State::hit_at`]).

use ratatui::layout::Rect;

use crate::action::Action;
use crate::app::State;
use crate::views::View;
use crate::widgets::status::banners;

/// Below this size only a "terminal too small" note is drawn.
pub const MIN_SIZE: (u16, u16) = (30, 8);

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
    /// A palette row (index into its entries).
    PaletteItem(usize),
    /// A palette tab (0 = All).
    PaletteTab(usize),
    /// A search result row (index into the shown rows).
    SearchRow(usize),
    /// The search results scrollbar.
    SearchScroll,
    /// A speed chart window label.
    SpeedWindow(crate::widgets::graph::Window),
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
    ///
    /// A plain top-to-bottom stack computed with saturating arithmetic (no
    /// constraint solver: this runs several times per frame and per key).
    /// When rows run out, the optional regions give way first, in this
    /// order: expanded graph, throughput card, banners; the header, context
    /// row and footer stay as long as there are rows for them.
    pub fn compute(size: (u16, u16), state: &State) -> Self {
        let (w, h) = size;
        let fixed = 3u16.min(h); // header, context row, footer
        let mut spare = h - fixed;
        // The body keeps at least a few rows before optional regions show.
        let mut take = |want: u16, keep: u16| {
            let got = want.min(spare.saturating_sub(keep));
            spare -= got;
            got
        };
        let banner_want = u16::try_from(banners(state).len()).unwrap_or(0);
        let banner = take(banner_want, 3);
        let card = if card_shown(size, state) {
            take(4, 12)
        } else {
            0
        };
        let graph = take(graph_height(h, state), 6);
        let body = spare;
        let mut y = 0u16;
        let mut row = |height: u16| {
            let r = Rect::new(0, y, w, height);
            y = y.saturating_add(height);
            r
        };
        let header = row(u16::from(h >= 1));
        let card = row(card);
        let context = row(u16::from(h >= 3));
        let banner = row(banner);
        let body = row(body);
        let graph = row(graph);
        let footer = row(u16::from(h >= 2));
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
    let list = Rect {
        width: body.width - preview,
        ..body
    };
    let pane = Rect {
        x: list.right(),
        width: preview,
        ..body
    };
    (list, Some(pane))
}
