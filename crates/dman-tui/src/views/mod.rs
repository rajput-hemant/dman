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

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::keymap::Context;
use crate::widgets::{footer, graph, status};

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
        View::Downloads | View::Detail(_) | View::Help(_) | View::Providers => None,
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

/// Mouse input (only delivered when mouse support is on).
pub fn on_mouse(state: &mut State, m: MouseEvent) -> Vec<Cmd> {
    if !state.mouse || state.modal.is_some() {
        state.dirty = false;
        return Vec::new();
    }
    match state.view() {
        View::Downloads => downloads::on_mouse(state, m),
        View::Help(_) => {
            let delta = match m.kind {
                MouseEventKind::ScrollDown => 3,
                MouseEventKind::ScrollUp => -3,
                _ => return Vec::new(),
            };
            help::scroll(state, delta);
        }
        View::Search => search::on_mouse(state, m),
        View::Detail(_) | View::Add(_) | View::Providers => {}
    }
    Vec::new()
}

/// Bracketed paste outside a modal.
pub fn on_paste(state: &mut State, text: &str) -> Vec<Cmd> {
    match state.view() {
        View::Add(_) => return add::on_paste(state, text),
        View::Downloads => {
            // Pasting links into the list opens the add view with them.
            let mut a = add::AddState::with_text(text);
            let cmds = a.check_cmds();
            state.views.push(View::Add(Box::new(a)));
            return cmds;
        }
        View::Search => return search::on_paste(state, text),
        View::Detail(_) | View::Help(_) | View::Providers => state.dirty = false,
    }
    Vec::new()
}

/// Draw the whole screen: chrome, top view, then the modal on top.
pub fn render(state: &State, frame: &mut Frame<'_>) {
    let l = state.layout();
    status::render_header(frame, l.header, state);
    status::render_info(frame, l.info, state);
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
    status::render_status(frame, l.status, state);
    footer::render(frame, l.footer, state);
    if let Some(m) = &state.modal {
        m.render(frame, frame.area(), state);
    }
}
