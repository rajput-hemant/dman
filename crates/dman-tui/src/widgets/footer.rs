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
    h.0.chars().count() + h.1.chars().count() + 3
}

/// The hints that fit in `cols` columns: as many of the body as fit, always
/// followed by the whole tail.
pub fn fitted(state: &State, cols: usize) -> Vec<Hint> {
    let tail = tail(state);
    let tail_w: usize = tail.iter().map(width).sum();
    let mut used = tail_w;
    let mut out = Vec::new();
    for h in body(state) {
        let w = width(&h);
        if used + w > cols {
            break;
        }
        used += w;
        out.push(h);
    }
    out.extend(tail);
    out
}

/// Draw the footer.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let mut spans = Vec::new();
    for (keys, label) in fitted(state, usize::from(area.width)) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(keys, t.key));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label, t.dim));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
