//! First-run welcome: shown once, over an empty downloads list, with what dman
//! does, the handful of keys a new user needs and where to find the rest.
//!
//! Whether it was shown is remembered with a marker file in the client state
//! directory ([`crate::TuiOptions::state_dir`]); without one it is remembered
//! for the running TUI only.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::keymap::{Context, Keymap, key_label};
use crate::theme::Theme;
use crate::views::View;
use crate::widgets::text::wrap_all;
use crate::widgets::{Modal, ModalOutcome, centered, modal_frame};

/// The welcome dialog.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WelcomeState {
    /// First text row shown (the text scrolls on short terminals).
    pub scroll: u16,
}

/// The first snapshot arrived: show the welcome if this looks like a first
/// run (no jobs, nothing else open) and it was not shown before.
pub fn on_snapshot(state: &mut State) -> Vec<Cmd> {
    if !state.welcome_pending {
        return Vec::new();
    }
    state.welcome_pending = false;
    let mark: Vec<Cmd> = state
        .welcome_marker
        .clone()
        .map(Cmd::MarkWelcomeShown)
        .into_iter()
        .collect();
    if !state.jobs.is_empty() {
        // Not a first run: never show it later either.
        return mark;
    }
    if state.modal.is_some() || !matches!(state.view(), View::Downloads) {
        return Vec::new();
    }
    state.modal = Some(Modal::Welcome(WelcomeState::default()));
    mark
}

fn key(keymap: &Keymap, action: Action) -> String {
    [Context::Downloads, Context::Global]
        .iter()
        .find_map(|c| keymap.keys(*c, action).first().copied())
        .map(key_label)
        .unwrap_or_default()
}

impl WelcomeState {
    /// Handle a key: the shortcuts listed in the dialog work right away, any
    /// of Enter / Esc / Space / q closes it.
    pub fn handle_key(&mut self, key: KeyEvent, state: &State) -> ModalOutcome {
        if let Some(
            a @ (Action::Add
            | Action::Search
            | Action::Help
            | Action::Palette
            | Action::ToggleMouse),
        ) = state.keymap.resolve_event(Context::Downloads, key)
        {
            return ModalOutcome::Run(a);
        }
        match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ' | 'q') => ModalOutcome::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1);
                ModalOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                ModalOutcome::Stay
            }
            _ => ModalOutcome::Stay,
        }
    }

    /// Footer hints.
    pub fn hints(keymap: &Keymap) -> Vec<(String, String)> {
        vec![
            (key(keymap, Action::Add), "add a link".into()),
            (key(keymap, Action::Search), "search torrents".into()),
            (key(keymap, Action::ToggleMouse), "mouse".into()),
            ("Enter/Esc".into(), "close".into()),
            ("↑↓".into(), "scroll".into()),
        ]
    }

    /// Draw the dialog (scrolled when it does not fit).
    pub fn render(self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, keymap: &Keymap) {
        let k = |a: Action| key(keymap, a);
        let row = |keys: String, what: &str| {
            Line::from(vec![
                Span::styled(format!("  {keys:<9}"), theme.key),
                Span::raw(what.to_owned()),
            ])
        };
        let lines = vec![
            Line::raw(
                "dman downloads links, magnets, torrents and videos in the background: \
                 downloads keep going after you close this screen.",
            ),
            Line::default(),
            row(k(Action::Add), "add a link, magnet or .torrent file"),
            row(k(Action::Search), "search for torrents"),
            row(k(Action::OpenDetail), "details of the selected download"),
            row(
                format!("{} / {}", k(Action::Pause), k(Action::Resume)),
                "pause / resume",
            ),
            row(
                k(Action::Remove),
                "remove a download (asks first; files are kept)",
            ),
            Line::default(),
            Line::from(vec![
                Span::styled("Web UI  ", theme.accent),
                Span::raw("run "),
                Span::styled("dman serve", theme.key),
                Span::raw(" in a terminal and open the address it shows"),
            ]),
            Line::from(vec![
                Span::styled("Help    ", theme.accent),
                Span::styled(k(Action::Help), theme.key),
                Span::raw(" lists every key; "),
                Span::styled(k(Action::Palette), theme.key),
                Span::raw(" finds any command by name"),
            ]),
            Line::from(vec![
                Span::styled("Mouse   ", theme.accent),
                Span::styled(k(Action::ToggleMouse), theme.key),
                Span::raw(" turns mouse support on (clicks, wheel, buttons)"),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled(
                    format!(" {}  Add a download ", k(Action::Add)),
                    theme.action,
                ),
                Span::raw("   "),
                Span::styled(" Enter  Close ", theme.pill),
            ]),
        ];
        let mut lines = lines;
        let logo = [
            Line::from(vec![
                Span::styled(format!("{} dman", theme.icons.down), theme.accent),
                Span::styled("  the download manager for your terminal", theme.dim),
            ]),
            Line::default(),
        ];
        let width = area.width.saturating_sub(2).min(72);
        let inner = usize::from(width.saturating_sub(4)).max(1);
        if area.height >= 23 {
            for (i, l) in logo.into_iter().enumerate() {
                lines.insert(i, l);
            }
        }
        let rows = wrap_all(&lines, inner);
        let room = usize::from(area.height.saturating_sub(2));
        let height = u16::try_from(rows.len().min(room))
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        let rect = centered(area, width, height);
        let inner_rect = modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(
                " Welcome to dman ",
                theme.accent.patch(theme.surface_bg()),
            )),
            theme.border_focus,
        );
        let max_scroll = rows.len().saturating_sub(room);
        let scroll = usize::from(self.scroll).min(max_scroll);
        let shown: Vec<Line<'static>> = rows.into_iter().skip(scroll).take(room).collect();
        frame.render_widget(
            Paragraph::new(shown).style(theme.surface),
            inner_rect.inner(Margin::new(1, 0)),
        );
        if max_scroll > 0 {
            crate::widgets::scrollbar(frame, rect, theme, max_scroll + room, scroll, room);
        }
    }
}
