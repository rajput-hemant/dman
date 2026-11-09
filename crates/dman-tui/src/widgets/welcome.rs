//! First-run welcome: shown once, over an empty downloads list, with what dman
//! does, the handful of keys a new user needs and where to find the rest.
//!
//! Whether it was shown is remembered with a marker file in the client state
//! directory ([`crate::TuiOptions::state_dir`]); without one it is remembered
//! for the running TUI only.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::action::Action;
use crate::app::{Cmd, State};
use crate::keymap::{Context, Keymap, key_label};
use crate::theme::Theme;
use crate::views::View;
use crate::widgets::{Modal, ModalOutcome, centered};

/// The welcome dialog (no state of its own).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WelcomeState;

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
    state.modal = Some(Modal::Welcome(WelcomeState));
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
    pub fn handle_key(self, key: KeyEvent, state: &State) -> ModalOutcome {
        if let Some(a @ (Action::Add | Action::Search | Action::Help | Action::Palette)) =
            state.keymap.resolve_event(Context::Downloads, key)
        {
            return ModalOutcome::Run(a);
        }
        match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ' | 'q') => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    /// Footer hints.
    pub fn hints(keymap: &Keymap) -> Vec<(String, String)> {
        vec![
            (key(keymap, Action::Add), "add a link".into()),
            (key(keymap, Action::Search), "search torrents".into()),
            ("Enter/Esc".into(), "close".into()),
        ]
    }

    /// Draw the dialog.
    pub fn render(self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, keymap: &Keymap) {
        let k = |a: Action| key(keymap, a);
        let row = |keys: String, what: &str| {
            Line::from(vec![
                Span::styled(format!("  {keys:<9}"), theme.key),
                Span::raw(what.to_owned()),
            ])
        };
        let lines = vec![
            Line::raw("dman downloads links, magnets, torrents and videos in the"),
            Line::raw("background: downloads keep going after you close this screen."),
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
            Line::default(),
            Line::from(vec![
                Span::styled(format!(" {} ", k(Action::Add)), theme.action),
                Span::raw(" add your first download   "),
                Span::styled("Enter", theme.key),
                Span::raw(" / "),
                Span::styled("Esc", theme.key),
                Span::raw(" close"),
            ]),
        ];
        let width = area.width.min(68);
        let inner = usize::from(width.saturating_sub(2)).max(1);
        let rows: usize = lines.iter().map(|l| l.width().div_ceil(inner).max(1)).sum();
        let height = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2);
        let rect = centered(area, width, height);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Span::styled(" Welcome to dman ", theme.accent));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            rect,
        );
    }
}
