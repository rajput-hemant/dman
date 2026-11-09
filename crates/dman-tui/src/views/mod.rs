//! Full-screen views and the top-level [`render`] function.
//!
//! # Adding a view
//! 1. Write `views/<name>.rs` with a state struct, `render(frame, area, state)`
//!    and `apply(state, action) -> Option<Vec<Cmd>>` (return `None` for actions
//!    the view does not handle). Text-entry views also implement a raw key
//!    handler, called before the keymap (see [`handle_raw_key`]).
//! 2. Add a [`View`] variant, a [`crate::keymap::Context`] (with default
//!    bindings in `keymap::DEFAULTS`, at least `Back`), and dispatch in the
//!    `match`es of this module.
//! 3. Open it by pushing onto `State::views` (usually from an [`Action`] in
//!    [`crate::app::apply_action`]); [`Action::Back`] pops it.

pub mod add;
pub mod detail;
pub mod downloads;
pub mod help;
pub mod providers;
pub mod rename;
pub mod search;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::keymap::Context;
use crate::layout::{Hit, MIN_SIZE};
use crate::widgets::toast::ToastAction;
use crate::widgets::{Modal, ModalOutcome, footer, graph, status, toast, whichkey};

/// A full-screen view. `State::views[0]` is always [`View::Downloads`].
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// The downloads list (state lives in `State::list`).
    Downloads,
    /// Detail of one job.
    Detail(Box<detail::DetailState>),
    /// Key bindings.
    Help(help::HelpState),
    /// Add downloads.
    Add(Box<add::AddState>),
    /// Search torrents (state lives in `State::search`).
    Search,
    /// Search providers settings (state lives in `State::providers`).
    Providers,
}

impl View {
    /// Key context.
    pub const fn context(&self) -> Context {
        match self {
            View::Downloads => Context::Downloads,
            View::Detail(_) => Context::Detail,
            View::Help(_) => Context::Help,
            View::Add(_) => Context::Add,
            View::Search => Context::Search,
            View::Providers => Context::Providers,
        }
    }

    /// Actions listed in the footer, most important first.
    pub fn footer_actions(&self) -> &'static [Action] {
        match self {
            View::Downloads => downloads::FOOTER,
            View::Detail(_) => detail::FOOTER,
            View::Help(_) => &[Action::Down, Action::Up, Action::PageDown, Action::Back],
            View::Add(_) => add::FOOTER,
            View::Search => search::FOOTER,
            View::Providers => providers::FOOTER,
        }
    }
}

/// Let text-entry views consume a key before the keymap. `None` = not consumed.
pub fn handle_raw_key(state: &mut State, key: KeyEvent) -> Option<Vec<Cmd>> {
    match state.view() {
        View::Add(_) => add::handle_raw_key(state, key),
        View::Search => search::handle_raw_key(state, key),
        View::Help(_) => help::handle_raw_key(state, key),
        View::Downloads | View::Detail(_) | View::Providers => None,
    }
}

/// Apply a view-specific action. `None` = the top view does not handle it.
pub fn apply_action(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    match state.view() {
        View::Downloads => downloads::apply(state, action),
        View::Help(_) => help::apply(state, action),
        View::Detail(_) => detail::apply(state, action),
        View::Add(_) => add::apply(state, action),
        View::Search => search::apply(state, action),
        View::Providers => providers::apply(state, action),
    }
}

/// Mouse input (only delivered when mouse support is on). Clicks are looked
/// up in the hit map of the last frame.
pub fn on_mouse(state: &mut State, m: MouseEvent) -> Vec<Cmd> {
    if !state.mouse {
        state.dirty = false;
        return Vec::new();
    }
    let hit = state.hit_at(m.column, m.row);
    let left = m.kind == MouseEventKind::Down(MouseButton::Left);
    if left && let Some((_, Hit::Toast(id))) = &hit {
        return click_toast(state, *id);
    }
    if let Some(modal) = state.modal.take() {
        return on_modal_mouse(state, modal, m, hit);
    }
    state.whichkey = None;
    if left {
        match &hit {
            Some((_, Hit::Chip(c))) => return downloads::show_chip(state, *c),
            Some((_, Hit::Action(a))) => return crate::app::apply_action(state, *a),
            Some((_, Hit::Mouse)) => return crate::app::toggle_mouse(state),
            Some((_, Hit::DetailTab(t))) => return detail::select_tab(state, *t),
            _ => {}
        }
    }
    match state.view() {
        View::Downloads => return downloads::on_mouse(state, m, hit),
        View::Help(_) => {
            let delta = match m.kind {
                MouseEventKind::ScrollDown => 3,
                MouseEventKind::ScrollUp => -3,
                _ => {
                    state.dirty = false;
                    return Vec::new();
                }
            };
            help::scroll(state, delta);
        }
        View::Search => search::on_mouse(state, m, hit),
        View::Detail(_) => {
            let action = match m.kind {
                MouseEventKind::ScrollDown => Action::Down,
                MouseEventKind::ScrollUp => Action::Up,
                _ => {
                    state.dirty = false;
                    return Vec::new();
                }
            };
            return detail::apply(state, action).unwrap_or_default();
        }
        View::Add(_) | View::Providers => state.dirty = false,
    }
    Vec::new()
}

fn click_toast(state: &mut State, id: u64) -> Vec<Cmd> {
    let undo = state
        .status
        .toasts
        .items
        .iter()
        .find(|t| t.id == id)
        .and_then(|t| t.action.clone());
    state.status.toasts.dismiss(id);
    match undo {
        Some(ToastAction::Undo(ids)) => state.call(crate::api::ApiCall::Restore(ids)),
        None => Vec::new(),
    }
}

fn on_modal_mouse(
    state: &mut State,
    mut modal: Modal,
    m: MouseEvent,
    hit: Option<(ratatui::layout::Rect, Hit)>,
) -> Vec<Cmd> {
    let left = m.kind == MouseEventKind::Down(MouseButton::Left);
    let outcome = match (&mut modal, &hit, m.kind) {
        (Modal::Confirm(c), Some((_, Hit::Button(i))), _) if left => c.press(*i == 1),
        (Modal::Palette(p), Some((_, Hit::PaletteItem(i))), _) if left => {
            p.selected = *i;
            match p.items.get(*i) {
                Some(a) => ModalOutcome::Run(*a),
                None => ModalOutcome::Stay,
            }
        }
        (Modal::Palette(p), _, MouseEventKind::ScrollDown) => {
            p.step(3);
            ModalOutcome::Stay
        }
        (Modal::Palette(p), _, MouseEventKind::ScrollUp) => {
            p.step(-3);
            ModalOutcome::Stay
        }
        // A click outside a palette or prompt closes it; confirmations need
        // an explicit button.
        (Modal::Palette(_) | Modal::Prompt(_) | Modal::Welcome(_), None, _) if left => {
            ModalOutcome::Close
        }
        _ => {
            state.dirty = false;
            ModalOutcome::Stay
        }
    };
    state.modal = Some(modal);
    crate::app::on_modal_outcome(state, outcome)
}

/// Bracketed paste outside a modal.
pub fn on_paste(state: &mut State, text: &str) -> Vec<Cmd> {
    match state.view() {
        View::Add(_) => return add::on_paste(state, text),
        View::Downloads => {
            // Pasting links into the list opens the add view with them.
            return add::push(state, add::AddState::with_text(text));
        }
        View::Search => return search::on_paste(state, text),
        View::Detail(_) | View::Help(_) | View::Providers => state.dirty = false,
    }
    Vec::new()
}

/// Draw the whole screen: chrome, top view, toasts, then the modal on top.
pub fn render(state: &State, frame: &mut Frame<'_>) {
    state.hits.borrow_mut().clear();
    let area = frame.area();
    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        let msg = vec![
            Line::styled("Terminal too small", state.theme.warn),
            Line::styled(
                format!("need {}×{}", MIN_SIZE.0, MIN_SIZE.1),
                state.theme.dim,
            ),
        ];
        let y = area.height / 2;
        let rect = ratatui::layout::Rect {
            y: y.saturating_sub(1),
            height: 2.min(area.height),
            ..area
        };
        frame.render_widget(Paragraph::new(msg).alignment(Alignment::Center), rect);
        return;
    }
    let l = state.layout();
    status::render_header(frame, l.header, state);
    if l.card.height > 0 {
        status::render_card(frame, l.card, state);
    }
    status::render_context(frame, l.context, state);
    status::render_banners(frame, l.banner, state);
    match state.view() {
        View::Downloads => downloads::render(frame, l.body, state),
        View::Detail(d) => detail::render(frame, l.body, state, d),
        View::Help(h) => help::render(frame, l.body, state, h),
        View::Add(a) => add::render(frame, l.body, state, a),
        View::Search => search::render(frame, l.body, state),
        View::Providers => providers::render(frame, l.body, state),
    }
    if l.graph.height > 0 {
        graph::render_panel(frame, l.graph, state);
    }
    footer::render(frame, l.footer, state);
    whichkey::render(frame, l.body, state);
    let toast_area = ratatui::layout::Rect {
        height: (l.body.height + l.graph.height).saturating_sub(if state.whichkey.is_some() {
            5
        } else {
            1
        }),
        width: l.body.width.saturating_sub(1),
        ..l.body
    };
    toast::render(frame, toast_area, state);
    if let Some(m) = &state.modal {
        m.render(frame, frame.area(), state);
    }
}
