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
pub mod anim;
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

use crate::app::{Density, State, StateOptions, UiPrefs};
use crate::keymap::Keymap;
use crate::theme::{BorderKind, ColorDepth, IconSet, Theme, ThemeOptions};

/// Options for [`run`], usually built with [`TuiOptions::from_config`].
#[derive(Debug, Clone, PartialEq)]
pub struct TuiOptions {
    /// Theme name (`dman-dark`, `dman-light`, `high-contrast`, `tokyo-night`,
    /// `mono`; `default` = `dman-dark`); `NO_COLOR` forces `mono`.
    pub theme: String,
    /// Colour depth: `auto`, `truecolor`, `256`, `16`, `never`.
    pub color: String,
    /// Glyphs: `unicode`, `nerd`, `ascii`.
    pub icons: String,
    /// Borders: `rounded`, `plain`, `thick`, `ascii`.
    pub borders: String,
    /// `[ui.colors]` overrides.
    pub colors: Vec<(String, String)>,
    /// Preview pane: `auto` or `off`.
    pub preview: String,
    /// Row density: `auto`, `compact`, `comfortable`.
    pub density: String,
    /// Animations (also off with `DMAN_REDUCE_MOTION`).
    pub animations: bool,
    /// Enable mouse support.
    pub mouse: bool,
    /// Save mouse toggles to `[ui] mouse` in config.toml.
    pub persist_mouse: bool,
    /// Tick interval in milliseconds (status countdowns); clamped to 50..=5000.
    pub refresh_ms: u64,
    /// The `[keys]` table of `config.toml`, if any.
    pub keys: Option<toml::Table>,
    /// Profile name shown in the header.
    pub profile: String,
    /// Show desktop notifications for finished jobs (`[notify] desktop`).
    pub desktop: bool,
    /// Client-side state directory; a `welcome-shown` marker there keeps the
    /// first-run welcome from showing again. `None` = remember it in memory
    /// only (the welcome may show on every start with an empty list).
    pub state_dir: Option<std::path::PathBuf>,
}

/// Name of the marker file in [`TuiOptions::state_dir`].
pub const WELCOME_MARKER: &str = "welcome-shown";

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            theme: "dman-dark".into(),
            color: "auto".into(),
            icons: "unicode".into(),
            borders: "rounded".into(),
            colors: Vec::new(),
            preview: "auto".into(),
            density: "auto".into(),
            animations: true,
            mouse: false,
            persist_mouse: false,
            refresh_ms: 250,
            keys: None,
            profile: "local".into(),
            desktop: false,
            state_dir: None,
        }
    }
}

impl TuiOptions {
    /// Options from the `[ui]` and `[keys]` sections of the configuration.
    pub fn from_config(config: &dman_config::Config, profile: &str) -> Self {
        Self {
            theme: config.ui.theme.clone(),
            color: config.ui.color.clone(),
            icons: config.ui.icons.clone(),
            borders: config.ui.borders.clone(),
            colors: config
                .ui
                .colors
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            preview: config.ui.preview.clone(),
            density: config.ui.density.clone(),
            animations: config.ui.animations,
            mouse: config.ui.mouse,
            persist_mouse: true,
            refresh_ms: config.ui.refresh_ms,
            keys: config
                .extra
                .get("keys")
                .and_then(toml::Value::as_table)
                .cloned(),
            profile: profile.to_owned(),
            desktop: config.notify.desktop,
            // The same discovery the CLI uses (honours DMAN_CONFIG_DIR).
            state_dir: dman_config::Paths::discover()
                .ok()
                .map(|p| p.state_dir().to_path_buf()),
        }
    }

    /// The initial UI state for these options; key-binding problems are shown
    /// as a warning on the status line.
    pub fn initial_state(&self) -> State {
        self.initial_state_with_env(&|k| std::env::var(k).ok())
    }

    /// [`TuiOptions::initial_state`] with the environment passed explicitly.
    pub fn initial_state_with_env(&self, env: &dyn Fn(&str) -> Option<String>) -> State {
        let (keymap, mut warnings) = Keymap::from_config(self.keys.as_ref());
        let marker = self.state_dir.as_ref().map(|d| d.join(WELCOME_MARKER));
        let mut warn = |w: String| warnings.push(w);
        let icons = match self.icons.as_str() {
            "unicode" | "" | "auto" => IconSet::Unicode,
            "nerd" => IconSet::Nerd,
            "ascii" => IconSet::Ascii,
            other => {
                warn(format!(
                    "[ui] icons: unknown '{other}' (unicode, nerd, ascii)"
                ));
                IconSet::Unicode
            }
        };
        let borders = match self.borders.as_str() {
            "rounded" | "" => BorderKind::Rounded,
            "plain" => BorderKind::Plain,
            "thick" => BorderKind::Thick,
            "ascii" => BorderKind::Ascii,
            other => {
                warn(format!(
                    "[ui] borders: unknown '{other}' (rounded, plain, thick, ascii)"
                ));
                BorderKind::Rounded
            }
        };
        if !matches!(
            self.color.as_str(),
            "auto" | "" | "truecolor" | "24bit" | "256" | "16" | "never" | "mono"
        ) {
            warn(format!(
                "[ui] color: unknown '{}' (auto, truecolor, 256, 16, never)",
                self.color
            ));
        }
        let depth = ColorDepth::detect(&self.color, env);
        let density = match self.density.as_str() {
            "auto" | "" => Density::Auto,
            "compact" => Density::Compact,
            "comfortable" => Density::Comfortable,
            other => {
                warn(format!(
                    "[ui] density: unknown '{other}' (auto, compact, comfortable)"
                ));
                Density::Auto
            }
        };
        let (theme, theme_warnings) = Theme::build_with_warnings(&ThemeOptions {
            name: self.theme.clone(),
            depth,
            icons,
            borders,
            colors: self.colors.clone(),
        });
        warnings.extend(theme_warnings);
        let reduce_motion = env("DMAN_REDUCE_MOTION").is_some_and(|v| !v.is_empty() && v != "0");
        let mut state = State::new(StateOptions {
            theme,
            keymap,
            mouse: self.mouse,
            profile: self.profile.clone(),
            tick: Duration::from_millis(self.refresh_ms.clamp(50, 5000)),
            size: (80, 24),
            desktop: self.desktop,
            welcome: !marker.as_ref().is_some_and(|m| m.exists()),
            welcome_marker: marker,
            ui: UiPrefs {
                animations: self.animations && !reduce_motion,
                preview: self.preview != "off",
                density,
                persist_mouse: self.persist_mouse,
            },
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
