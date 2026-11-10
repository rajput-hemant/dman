//! Help screen: the full keymap grouped by context, plus the keys of popups.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::format;
use crate::keymap::{Context, Keymap};
use crate::theme::Theme;
use crate::views::View;
use crate::views::add::browser::{DirPicker, FileBrowser};
use crate::views::add::preview::Preview;
use crate::widgets::input::LineInput;
use crate::widgets::palette::PaletteState;
use crate::widgets::prompt::PromptState;
use crate::widgets::scrollbar;

/// Help view state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpState {
    /// First line shown.
    pub scroll: usize,
    /// Search text (`/`).
    pub query: LineInput,
    /// The search has the keyboard.
    pub searching: bool,
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
                ("y", "yes, do it"),
                ("n / Esc", "no, cancel"),
                (
                    "Enter",
                    "press the focused button (Cancel first when files are deleted)",
                ),
                ("← → / Tab", "switch between the buttons"),
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
        (
            "Add form",
            own(&[
                ("Enter", "review what will be added (then Enter again adds)"),
                ("Ctrl-j / Alt-Enter", "new line in the links box"),
                ("Tab / Shift-Tab", "next / previous option"),
                ("Space", "tick a checkbox; show / hide advanced options"),
            ]),
        ),
        ("Review before adding (Add: Enter)", Preview::key_help()),
        (
            "Video quality picker (video sources, before adding)",
            own(&[
                ("Enter", "download the selection"),
                (
                    "Tab / Shift-Tab",
                    "next / previous part (video, audio, presets, checkboxes)",
                ),
                ("↑ ↓", "move in the list (selection follows the cursor)"),
                ("1-5", "preset: Best, 1080p, 720p, 480p, audio only"),
                (
                    "u / n",
                    "tick \"Use as my default\" / \"Never ask again\" (Space on a focused box)",
                ),
                ("Esc", "cancel and go back to the form"),
            ]),
        ),
        (
            "First-run welcome",
            own(&[
                ("a / f / ?", "add, search or help right away"),
                ("Enter / Esc", "close"),
            ]),
        ),
    ]
}

/// One group of the cheat sheet: heading and its lines.
type Group = (String, Vec<Line<'static>>);

fn entry(theme: &Theme, keys: &str, what: &str, key_w: usize, width: usize) -> Line<'static> {
    let pad = key_w.saturating_sub(keys.chars().count() + 2);
    let room = width.saturating_sub(3 + key_w.max(keys.chars().count() + 2));
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!(" {keys} "), theme.pill),
        Span::raw(" ".repeat(pad)),
        Span::styled(
            format!(" {}", crate::format::truncate(what, room)),
            theme.text,
        ),
    ])
}

/// The cheat sheet groups: the contexts (the one in use first), then the
/// popups. With a `query` only matching entries are kept.
pub fn groups(
    keymap: &Keymap,
    theme: &Theme,
    first: Context,
    query: &str,
    width: usize,
) -> Vec<Group> {
    let q = query.trim().to_lowercase();
    let keep = |k: &str, what: &str| {
        q.is_empty() || k.to_lowercase().contains(&q) || what.to_lowercase().contains(&q)
    };
    let key_w = 16;
    let mut order: Vec<Context> = vec![first];
    if first != Context::Global {
        order.push(Context::Global);
    }
    let rest: Vec<Context> = Context::ALL
        .iter()
        .copied()
        .filter(|c| !order.contains(c))
        .collect();
    order.extend(rest);
    let mut out = Vec::new();
    for ctx in order {
        let bindings = keymap.bindings(ctx);
        let mut seen: Vec<Action> = Vec::new();
        let mut lines = Vec::new();
        for b in bindings {
            if seen.contains(&b.action) {
                continue;
            }
            seen.push(b.action);
            let keys = keymap.keys(ctx, b.action);
            let label: Vec<String> = keys.into_iter().map(crate::keymap::key_label).collect();
            let label = label.join(" ");
            if keep(&label, b.action.title()) {
                lines.push(entry(theme, &label, b.action.title(), key_w, width));
            }
        }
        if !lines.is_empty() {
            out.push((format!("{} keys", title(ctx)), lines));
        }
    }
    for (name, keys) in popups() {
        let lines: Vec<Line<'static>> = keys
            .iter()
            .filter(|(k, what)| keep(k, what) || keep("", name))
            .map(|(k, what)| entry(theme, k, what, key_w, width))
            .collect();
        if !lines.is_empty() {
            out.push((format!("Popup: {name}"), lines));
        }
    }
    out
}

/// The help text as lines (one column, contexts in the usual order).
pub fn lines(keymap: &Keymap, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let all = groups(keymap, theme, Context::Global, "", usize::MAX / 2);
    let mut popups_started = false;
    for (name, lines) in all {
        if let Some(p) = name.strip_prefix("Popup: ") {
            if !popups_started {
                out.push(Line::styled("Popups", theme.section));
                popups_started = true;
            }
            out.push(Line::styled(format!("  {p}"), theme.accent));
        } else {
            out.push(Line::styled(name, theme.header));
        }
        out.extend(lines);
        out.push(Line::default());
    }
    out.push(Line::styled(
        "Keys can be changed in config.toml: [keys] pause = [\"p\", \"ctrl-s\"]",
        theme.dim,
    ));
    out
}

fn heading(theme: &Theme, name: &str, width: usize) -> Line<'static> {
    let used = name.chars().count() + 4;
    Line::from(vec![
        Span::styled("── ", theme.section),
        Span::styled(name.to_owned(), theme.accent),
        Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(used))),
            theme.section,
        ),
    ])
}

/// The sheet laid out in `cols` columns of `width`: rows of (left, right).
fn layout(state: &State, h: &HelpState, cols: usize, width: usize) -> Vec<Vec<Line<'static>>> {
    let t = &state.theme;
    let first = state
        .views
        .iter()
        .rev()
        .nth(1)
        .map_or(Context::Downloads, crate::views::View::context);
    let gs = groups(&state.keymap, t, first, h.query.text(), width);
    let blocks: Vec<Vec<Line<'static>>> = gs
        .into_iter()
        .map(|(name, lines)| {
            let mut v = vec![heading(t, &name, width)];
            v.extend(lines);
            v.push(Line::default());
            v
        })
        .collect();
    let total: usize = blocks.iter().map(Vec::len).sum();
    let mut columns: Vec<Vec<Line<'static>>> = vec![Vec::new(); cols.max(1)];
    let per = total.div_ceil(cols.max(1));
    let mut c = 0;
    for b in blocks {
        if c + 1 < columns.len() && !columns[c].is_empty() && columns[c].len() + b.len() > per {
            c += 1;
        }
        columns[c].extend(b);
    }
    columns
}

fn columns_for(width: u16) -> usize {
    if width >= 110 { 2 } else { 1 }
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
    let Some(View::Help(h)) = state.views.last() else {
        return 0;
    };
    let body = state.layout().body;
    let cols = columns_for(body.width);
    let w = usize::from(body.width.saturating_sub(4)) / cols;
    let total = layout(state, h, cols, w)
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    let visible = usize::from(body.height.saturating_sub(3));
    total.saturating_sub(visible)
}

/// Scroll by `delta` lines (clamped).
pub fn scroll(state: &mut State, delta: isize) {
    let max = max_scroll(state);
    if let Some(View::Help(h)) = state.views.last_mut() {
        h.scroll = h.scroll.saturating_add_signed(delta).min(max);
    }
}

/// `/` starts a search; while typing, keys edit it (Enter keeps it, Esc
/// clears it).
pub fn handle_raw_key(state: &mut State, key: KeyEvent) -> Option<Vec<Cmd>> {
    let Some(View::Help(h)) = state.views.last_mut() else {
        return None;
    };
    if !h.searching {
        if key.code == KeyCode::Char('/')
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            h.searching = true;
            return Some(Vec::new());
        }
        if key.code == KeyCode::Esc && !h.query.text().is_empty() {
            h.query = LineInput::default();
            h.scroll = 0;
            return Some(Vec::new());
        }
        return None;
    }
    match key.code {
        KeyCode::Enter => h.searching = false,
        KeyCode::Esc => {
            h.searching = false;
            h.query = LineInput::default();
        }
        _ => {
            h.query.handle_key(key);
        }
    }
    h.scroll = 0;
    Some(Vec::new())
}

/// Apply a help-view action.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    let page = isize::try_from(state.layout().body.height.saturating_sub(3)).unwrap_or(1);
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

/// Draw the help screen: a searchable cheat sheet in one or two columns.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State, h: &HelpState) {
    let t = &state.theme;
    let block = t
        .block_focus(true)
        .title(Span::styled(" Keys ", t.accent))
        .title_bottom(Line::from(Span::styled(
            format::truncate(
                " Keys can be changed in config.toml [keys] pause = [\"p\"] ",
                usize::from(area.width).saturating_sub(2),
            ),
            t.faint,
        )));
    let inner = block.inner(area).inner(Margin::new(1, 0));
    frame.render_widget(block, area);
    let [search, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    let mut q = vec![Span::styled(format!("{} ", t.icons.search), t.accent)];
    if h.searching || !h.query.text().is_empty() {
        q.extend(h.query.line(t, h.searching).spans);
    } else {
        q.push(Span::styled("/ to search keys and commands", t.faint));
    }
    frame.render_widget(Paragraph::new(Line::from(q)), search);
    let cols = columns_for(body.width);
    let gap = 3;
    let col_w = (usize::from(body.width) + gap) / cols - gap;
    let columns = layout(state, h, cols, col_w);
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    if rows == 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "Nothing matches — Esc clears the search",
                t.dim,
            )),
            body,
        );
        return;
    }
    for (i, col) in columns.into_iter().enumerate() {
        let x = body.x + u16::try_from(i * (col_w + gap)).unwrap_or(0);
        let rect = Rect::new(x, body.y, u16::try_from(col_w).unwrap_or(0), body.height);
        let scroll = u16::try_from(h.scroll).unwrap_or(u16::MAX);
        frame.render_widget(Paragraph::new(col).scroll((scroll, 0)), rect);
    }
    scrollbar(
        frame,
        area.inner(Margin::new(0, 1)),
        t,
        rows,
        h.scroll,
        usize::from(body.height),
    );
}
