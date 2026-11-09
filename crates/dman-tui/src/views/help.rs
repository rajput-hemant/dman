//! Help screen: the full keymap grouped by context, plus the keys of popups.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::keymap::{Context, Keymap};
use crate::theme::Theme;
use crate::views::View;
use crate::views::add::browser::{DirPicker, FileBrowser};
use crate::views::add::preview::Preview;
use crate::widgets::palette::PaletteState;
use crate::widgets::prompt::PromptState;

/// Help view state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpState {
    /// First line shown.
    pub scroll: usize,
}

/// Popups and overlays with their (non-configurable) keys. The lists come
/// from the popups' own footer hints where they have them, so the help cannot
/// drift from what the popup does.
fn popups() -> Vec<(&'static str, Vec<(String, String)>)> {
    let own = |keys: &[(&str, &str)]| -> Vec<(String, String)> {
        keys.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    vec![
        ("Command palette", {
            let mut h = PaletteState::hints();
            h.insert(0, ("type".into(), "fuzzy search".into()));
            h.push(("Ctrl-n / Ctrl-p".into(), "choose".into()));
            h
        }),
        (
            "Confirmation",
            own(&[
                ("y", "confirm"),
                ("n / Esc", "cancel (the default answer is no)"),
                (
                    "DELETE + Enter",
                    "second confirmation before files are deleted",
                ),
            ]),
        ),
        ("Prompt (rename, option, max concurrent)", {
            let mut h = PromptState::hints();
            h.extend(own(&[
                ("← → Home End", "move"),
                ("Ctrl-u / Ctrl-w", "clear / delete word"),
            ]));
            h
        }),
        (
            "Search form (query, category, providers)",
            own(&[
                ("Enter", "run the search"),
                ("Tab / Shift-Tab", "next / previous field"),
                ("← →  Space", "providers: move / switch one on or off"),
                ("Esc", "cancel the running search, else leave"),
            ]),
        ),
        ("File browser (Add: Ctrl-o)", FileBrowser::hints()),
        ("Directory picker (Add: Ctrl-f)", DirPicker::hints()),
        ("Preview (Add: Ctrl-s)", Preview::key_help()),
    ]
}

/// The help text as lines.
pub fn lines(keymap: &Keymap, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let key_w = 22;
    for ctx in Context::ALL {
        let bindings = keymap.bindings(*ctx);
        if bindings.is_empty() {
            continue;
        }
        out.push(Line::styled(format!("{} keys", title(*ctx)), theme.section));
        let mut seen: Vec<Action> = Vec::new();
        for b in bindings {
            if seen.contains(&b.action) {
                continue;
            }
            seen.push(b.action);
            let keys = keymap.keys(*ctx, b.action);
            let label: Vec<String> = keys.into_iter().map(crate::keymap::key_label).collect();
            out.push(Line::from(vec![
                Span::styled(format!("  {:<key_w$}", label.join(" ")), theme.key),
                Span::raw(b.action.title()),
            ]));
        }
        out.push(Line::default());
    }
    out.push(Line::styled("Popups", theme.section));
    for (name, keys) in popups() {
        out.push(Line::styled(format!("  {name}"), theme.accent));
        for (k, what) in keys {
            out.push(Line::from(vec![
                Span::styled(format!("    {k:<w$}", w = key_w - 2), theme.key),
                Span::raw(what),
            ]));
        }
    }
    out.push(Line::default());
    out.push(Line::styled(
        "Keys can be changed in config.toml: [keys] pause = [\"p\", \"ctrl-s\"]",
        theme.dim,
    ));
    out
}

const fn title(ctx: Context) -> &'static str {
    match ctx {
        Context::Global => "Global",
        Context::Downloads => "Downloads",
        Context::Detail => "Detail",
        Context::Help => "Help",
        Context::Add => "Add",
        Context::Search => "Search",
        Context::Providers => "Search providers",
    }
}

fn max_scroll(state: &State) -> usize {
    let total = lines(&state.keymap, &state.theme).len();
    let visible = usize::from(state.layout().body.height.saturating_sub(2));
    total.saturating_sub(visible)
}

/// Scroll by `delta` lines (clamped).
pub fn scroll(state: &mut State, delta: isize) {
    let max = max_scroll(state);
    if let Some(View::Help(h)) = state.views.last_mut() {
        h.scroll = h.scroll.saturating_add_signed(delta).min(max);
    }
}

/// Apply a help-view action.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    let page = isize::try_from(state.layout().body.height.saturating_sub(2)).unwrap_or(1);
    let delta = match action {
        Action::Up => -1,
        Action::Down => 1,
        Action::PageUp => -page,
        Action::PageDown => page,
        Action::Top => isize::MIN / 2,
        Action::Bottom => isize::MAX / 2,
        _ => return None,
    };
    scroll(state, delta);
    Some(Vec::new())
}

/// Draw the help screen.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, h: &HelpState) {
    let t = &state.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border)
        .title(Span::styled(" Help — key bindings ", t.accent));
    let scroll = u16::try_from(h.scroll).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines(&state.keymap, t))
            .block(block)
            .scroll((scroll, 0)),
        area,
    );
}
