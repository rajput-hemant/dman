//! Contextual footer: the most useful keys of the current view or modal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::State;
use crate::keymap::key_label;

/// (keys, label) pairs for the current view or modal.
pub fn hints(state: &State) -> Vec<(String, String)> {
    if let Some(m) = &state.modal {
        return m.hints();
    }
    if let crate::views::View::Add(a) = state.view()
        && let Some(h) = a.overlay_hints()
    {
        return h;
    }
    let ctx = state.context();
    state
        .view()
        .footer_actions()
        .iter()
        .filter_map(|a| {
            let keys = state.keymap.keys_with_global(ctx, *a);
            let first = keys.first()?;
            Some((key_label(*first), a.short().to_owned()))
        })
        .collect()
}

/// Draw the footer, dropping hints that do not fit.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let width = usize::from(area.width);
    let mut used = 0;
    let mut spans = Vec::new();
    for (keys, label) in hints(state) {
        let w = keys.chars().count() + label.chars().count() + 3;
        if used + w > width {
            break;
        }
        used += w;
        spans.push(Span::raw(" "));
        spans.push(Span::styled(keys, t.key));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label, t.dim));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
