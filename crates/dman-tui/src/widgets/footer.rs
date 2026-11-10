//! Contextual footer: the most useful keys for what is under the cursor, then
//! (always, never cut off) how to reach the command palette and the help.
//!
//! Each view builds its own list ([`body`]) from its state, most useful first;
//! when the terminal is narrow the least useful hints are dropped, never the
//! pinned palette / help hints at the end.

use crokey::KeyCombination;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::app::State;
use crate::keymap::key_label;
use crate::layout::Hit;
use crate::views::{self, View};

/// One footer entry: (keys, what they do).
pub type Hint = (String, String);

/// Whether printable keys are text right now (an input has the keyboard), so
/// hints must name keys that still work (Ctrl-p, F1) instead of `:` or `?`.
pub fn text_entry(state: &State) -> bool {
    match state.view() {
        View::Add(_) => true,
        View::Search => !state.search.history_open && state.search.focus.is_text(),
        _ => false,
    }
}

fn printable(k: KeyCombination) -> bool {
    matches!(k.codes.first(), KeyCode::Char(_))
        && !k.modifiers.intersects(
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
        )
}

/// The label of the key to show for `action` in the current context: the first
/// binding, or while typing the first one that is not a printable character.
pub fn key_for(state: &State, action: Action) -> Option<String> {
    let keys = state.keymap.keys_with_global(state.context(), action);
    let k = if text_entry(state) {
        keys.into_iter().find(|k| !printable(*k))
    } else {
        keys.into_iter().next()
    };
    k.map(key_label)
}

/// `(key, label)` for `action` if it has a usable key.
pub fn hint(state: &State, action: Action, label: &str) -> Option<Hint> {
    key_for(state, action).map(|k| (k, label.to_owned()))
}

/// A hint with a fixed key text (popups with their own keys).
pub fn own(keys: &str, label: &str) -> Hint {
    (keys.to_owned(), label.to_owned())
}

/// The view's (or modal's) own hints, most useful first.
pub fn body(state: &State) -> Vec<Hint> {
    if let Some(m) = &state.modal {
        return m.hints(&state.keymap);
    }
    match state.view() {
        View::Downloads => views::downloads::footer_hints(state),
        View::Detail(d) => views::detail::footer_hints(state, d),
        View::Add(a) => views::add::footer_hints(state, a),
        View::Search => views::search::footer_hints(state),
        View::Help(_) | View::Providers => {
            let ctx = state.context();
            state
                .view()
                .footer_actions()
                .iter()
                .filter(|a| !matches!(a, Action::Help | Action::Palette | Action::Quit))
                .filter_map(|a| {
                    let keys = state.keymap.keys_with_global(ctx, *a);
                    let first = keys.first()?;
                    Some((key_label(*first), a.short().to_owned()))
                })
                .collect()
        }
    }
}

/// The pinned hints at the end: palette and help (none while a dialog is open,
/// where those keys are not available).
pub fn tail(state: &State) -> Vec<Hint> {
    if state.modal.is_some() {
        return Vec::new();
    }
    let help_label = if matches!(state.view(), View::Help(_)) {
        "close help"
    } else {
        "help"
    };
    [
        hint(state, Action::Palette, "palette"),
        hint(state, Action::Help, help_label),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Everything the footer would show on an unlimited width.
pub fn hints(state: &State) -> Vec<Hint> {
    let mut h = body(state);
    h.extend(tail(state));
    h
}

fn width(h: &Hint) -> usize {
    h.0.chars().count() + h.1.chars().count() + 4
}

/// The first key of `action` in its home context (or globally), for texts
/// that name a key outside the footer (toasts).
pub fn key_label_for(state: &State, action: Action) -> Option<String> {
    state
        .keymap
        .keys_with_global(action.home_context(), action)
        .first()
        .map(|k| key_label(*k))
}

/// Whether a hint is the way out of the current screen (back, cancel,
/// close, quit): such hints are never dropped for lack of room.
/// (`q quit` on the downloads list is not one: it leaves the app.)
fn way_out(h: &Hint) -> bool {
    h.0.contains("Esc")
        || ["back", "cancel", "close", "leave"]
            .iter()
            .any(|w| h.1.starts_with(w))
}

/// The hints that fit in `cols` columns, in their original order. Room is
/// handed out by importance: the way out, help, the main action (the first
/// hint), the palette, then the other hints in the view's order, as many as
/// the width allows.
pub fn fitted(state: &State, cols: usize) -> Vec<Hint> {
    let body = body(state);
    let tail = tail(state);
    let exit = body.iter().position(way_out);
    // Indices into body ++ tail, by importance.
    let n = body.len();
    let find = |label: &str| {
        tail.iter()
            .position(|h| h.1.ends_with(label))
            .map(|i| n + i)
    };
    let (help, palette) = (find("help"), find("palette"));
    let mut order: Vec<usize> = Vec::new();
    order.extend(exit);
    order.extend(help);
    if !body.is_empty() && exit != Some(0) {
        order.push(0);
    }
    order.extend(palette);
    order.extend((0..n).filter(|i| Some(*i) != exit && *i != 0));
    let all: Vec<Hint> = body.into_iter().chain(tail).collect();
    let mut keep = vec![false; all.len()];
    let mut used = 0;
    for i in order {
        let w = width(&all[i]);
        if used + w <= cols {
            used += w;
            keep[i] = true;
        }
    }
    all.into_iter()
        .zip(keep)
        .filter_map(|(h, k)| k.then_some(h))
        .collect()
}

/// Narrower than this, the footer leaves out the mode pill (the hints
/// matter more).
const MODE_MIN_COLS: u16 = 90;

/// The mode shown in the pill at the left of the footer.
pub fn mode(state: &State) -> &'static str {
    use crate::widgets::Modal;
    match &state.modal {
        Some(Modal::Filter(_)) => return "FILTER",
        Some(Modal::Confirm(_)) => return "CONFIRM",
        Some(Modal::Palette(_)) => return "COMMAND",
        Some(Modal::Prompt(_)) => return "INPUT",
        Some(Modal::Welcome(_)) => return "WELCOME",
        None => {}
    }
    match state.view() {
        View::Downloads if !state.list.selected.is_empty() => "MARK",
        View::Downloads => "LIST",
        View::Detail(_) => "DETAIL",
        View::Add(_) => "ADD",
        View::Search => "SEARCH",
        View::Providers => "PROVIDERS",
        View::Help(_) => "HELP",
    }
}

/// Draw the footer: mode pill, key hints as pills, and a spinner while
/// calls are in flight.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let mode = if area.width >= MODE_MIN_COLS {
        format!(" {} ", mode(state))
    } else {
        String::new()
    };
    let mut spans = vec![Span::styled(mode.clone(), t.mode), Span::raw(" ")];
    let mut right: Vec<Span<'static>> = Vec::new();
    if state.pending > 0 {
        let spin = state
            .anim_ms()
            .map_or(t.icons.spinner[0], |ms| t.icons.spin(ms));
        right.push(Span::styled(format!("{spin} "), t.accent));
        right.push(Span::styled("working ", t.dim));
    }
    let right_w: usize = right.iter().map(Span::width).sum();
    let cols = usize::from(area.width).saturating_sub(mode.chars().count() + 1 + right_w);
    let mut x = area.x + u16::try_from(mode.chars().count() + 1).unwrap_or(0);
    for (keys, label) in fitted(state, cols) {
        let w = u16::try_from(width(&(keys.clone(), label.clone()))).unwrap_or(0);
        if let Some(a) = action_of(state, &keys) {
            state.hit(Rect::new(x, area.y, w.saturating_sub(1), 1), Hit::Action(a));
        }
        x += w;
        spans.push(Span::styled(format!(" {keys} "), t.pill));
        spans.push(Span::styled(format!(" {label} "), t.dim));
    }
    let used: usize = spans.iter().map(Span::width).sum();
    let width = usize::from(area.width);
    if used + right_w <= width {
        spans.push(Span::raw(" ".repeat(width - used - right_w)));
        spans.extend(right);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The action a footer key text runs in the current context (no modal).
fn action_of(state: &State, keys: &str) -> Option<Action> {
    if state.modal.is_some() {
        return None;
    }
    Action::ALL
        .iter()
        .copied()
        .find(|a| key_for(state, *a).as_deref() == Some(keys))
}
