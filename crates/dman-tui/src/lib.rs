//! Terminal user interface for dman (ratatui + crossterm).
//!
//! # Architecture (Elm-style)
//! * [`app::State`] holds everything shown on screen.
//! * [`app::Msg`] is every input: keys, mouse, paste, resize, daemon events,
//!   connection changes, results of daemon calls, ticks and signals.
//! * [`app::update`] applies a message to the state and returns [`app::Cmd`]s
//!   (daemon calls, resync, quit). It does no I/O and is unit-tested directly.
//! * [`views::render`] draws a `&State` into a ratatui frame (snapshot-tested
//!   with ratatui's `TestBackend`).
//! * [`runtime`] owns the terminal, merges all event sources into one channel,
//!   executes commands as tasks and redraws when dirty (≤ 20 fps), so no
//!   operation or modal ever blocks live updates.
//!
//! Views ([`views::View`]) form a stack with the downloads list at the bottom;
//! modals ([`widgets::Modal`]) draw on top of the current view. Keys resolve to
//! [`action::Action`]s through a per-context [`keymap::Keymap`]. See the module
//! docs of [`views`] and [`widgets`] for how to add a view or a modal.

pub mod action;
pub mod api;
pub mod app;
pub mod format;
pub mod keymap;
pub mod layout;
pub mod list;
pub mod runtime;
pub mod theme;
pub mod views;
pub mod widgets;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

use std::time::Duration;

use dman_client::ReconnectingClient;

use crate::app::{State, StateOptions};
use crate::keymap::Keymap;
use crate::theme::Theme;

/// Options for [`run`], usually built with [`TuiOptions::from_config`].
#[derive(Debug, Clone, PartialEq)]
pub struct TuiOptions {
    /// Theme name (`default` or `mono`); `NO_COLOR` forces `mono`.
    pub theme: String,
    /// Enable mouse support.
    pub mouse: bool,
    /// Tick interval in milliseconds (status countdowns); clamped to 50..=5000.
    pub refresh_ms: u64,
    /// The `[keys]` table of `config.toml`, if any.
    pub keys: Option<toml::Table>,
    /// Profile name shown in the header.
    pub profile: String,
    /// Show desktop notifications for finished jobs (`[notify] desktop`).
    pub desktop: bool,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            theme: "default".into(),
            mouse: false,
            refresh_ms: 250,
            keys: None,
            profile: "local".into(),
            desktop: false,
        }
    }
}

impl TuiOptions {
    /// Options from the `[ui]` and `[keys]` sections of the configuration.
    pub fn from_config(config: &dman_config::Config, profile: &str) -> Self {
        Self {
            theme: config.ui.theme.clone(),
            mouse: config.ui.mouse,
            refresh_ms: config.ui.refresh_ms,
            keys: config
                .extra
                .get("keys")
                .and_then(toml::Value::as_table)
                .cloned(),
            profile: profile.to_owned(),
            desktop: config.notify.desktop,
        }
    }

    /// The initial UI state for these options; key-binding problems are shown
    /// as a warning on the status line.
    pub fn initial_state(&self) -> State {
        let (keymap, warnings) = Keymap::from_config(self.keys.as_ref());
        let mut state = State::new(StateOptions {
            theme: Theme::resolve(&self.theme, Theme::no_color_env()),
            keymap,
            mouse: self.mouse,
            profile: self.profile.clone(),
            tick: Duration::from_millis(self.refresh_ms.clamp(50, 5000)),
            size: (80, 24),
            desktop: self.desktop,
        });
        if !warnings.is_empty() {
            state.status.warn(warnings.join("; "));
        }
        state
    }
}

/// Run the TUI until the user quits (the daemon keeps running) or SIGTERM /
/// SIGHUP arrives. The terminal is restored on every exit path, including
/// panics.
///
/// Logging must not go to the terminal while this runs (log to a file).
///
/// # Errors
/// Terminal I/O failures.
pub async fn run(client: ReconnectingClient, opts: TuiOptions) -> anyhow::Result<()> {
    runtime::run_loop(client, opts.initial_state()).await
}
