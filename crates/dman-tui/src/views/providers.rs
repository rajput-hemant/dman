//! Search providers settings: list, enable, reorder, test, import, export and
//! remove custom providers. Every change is a daemon call (`providers.set`, …)
//! and takes effect there at once; the list shown is what the daemon last
//! answered. Tests (`providers.test`) run in the background: the row shows a
//! spinner and the report appears below when it arrives.

use std::collections::HashMap;

use dman_proto::search::{ProviderInfo, ProviderSetParams, ProviderTestReport};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::action::Action;
use crate::api::{ApiCall, ApiReply};
use crate::app::{Cmd, State};
use crate::format::{self, plural};
use crate::views::View;
use crate::views::search::{self, render::SPINNER};
use crate::widgets::Modal;
use crate::widgets::confirm::ConfirmState;
use crate::widgets::prompt::{PromptKind, PromptState};
use crate::widgets::table::{self, Col};

/// Footer actions of this view.
pub const FOOTER: &[Action] = &[
    Action::ProviderToggle,
    Action::ProviderTest,
    Action::MoveUp,
    Action::MoveDown,
    Action::ProviderImport,
    Action::ProviderExport,
    Action::ProviderRemove,
    Action::Back,
];

/// The hint shown for an enabled provider whose definition is unverified.
pub const UNVERIFIED_HINT: &str = "definition not verified against the live site";

/// State of the last test of one provider.
#[derive(Debug, Clone, PartialEq)]
pub enum TestState {
    /// `providers.test` is in flight.
    Running,
    /// The report.
    Done(Box<ProviderTestReport>),
    /// The call itself failed.
    Failed(String),
}

/// The providers as the daemon last reported them.
#[derive(Debug, Clone, Default)]
pub struct ProvidersState {
    /// Every provider, sorted by `(order, name)`.
    pub list: Vec<ProviderInfo>,
    /// `providers.list` answered at least once.
    pub loaded: bool,
    /// Cursor in the settings list.
    pub cursor: usize,
    /// Test results by provider name.
    pub tests: HashMap<String, TestState>,
}

impl ProvidersState {
    fn sort(&mut self) {
        self.list
            .sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
    }

    /// The provider under the cursor.
    pub fn current(&self) -> Option<&ProviderInfo> {
        self.list.get(self.cursor)
    }

    /// A test is running.
    pub fn busy(&self) -> bool {
        self.tests.values().any(|t| *t == TestState::Running)
    }

    fn select(&mut self, name: &str) {
        if let Some(i) = self.list.iter().position(|p| p.name == name) {
            self.cursor = i;
        }
    }
}

/// Enabled providers, in order.
pub fn enabled(state: &State) -> impl Iterator<Item = &ProviderInfo> {
    state.providers.list.iter().filter(|p| p.enabled)
}

/// Ask the daemon for the provider list.
pub fn fetch(state: &mut State) -> Vec<Cmd> {
    state.call_quiet(ApiCall::ProvidersList)
}

/// Open the settings view.
pub fn open(state: &mut State) -> Vec<Cmd> {
    if !matches!(state.view(), View::Providers) {
        state.views.push(View::Providers);
    }
    fetch(state)
}

/// Connection lost: running tests will not be answered.
pub fn on_disconnect(state: &mut State) {
    for t in state.providers.tests.values_mut() {
        if *t == TestState::Running {
            *t = TestState::Failed("lost the connection to the daemon".into());
        }
    }
}

/// Apply a providers-view action.
pub fn apply(state: &mut State, action: Action) -> Option<Vec<Cmd>> {
    let page = isize::try_from(state.layout().body.height.saturating_sub(4).max(1)).unwrap_or(1);
    let p = &mut state.providers;
    let last = p.list.len().saturating_sub(1);
    let cmds = match action {
        Action::Up => {
            p.cursor = p.cursor.saturating_sub(1);
            Vec::new()
        }
        Action::Down => {
            p.cursor = (p.cursor + 1).min(last);
            Vec::new()
        }
        Action::PageUp => {
            p.cursor = p.cursor.saturating_add_signed(-page);
            Vec::new()
        }
        Action::PageDown => {
            p.cursor = p.cursor.saturating_add_signed(page).min(last);
            Vec::new()
        }
        Action::Top => {
            p.cursor = 0;
            Vec::new()
        }
        Action::Bottom => {
            p.cursor = last;
            Vec::new()
        }
        Action::ProviderToggle => toggle(state),
        Action::MoveUp => reorder(state, -1),
        Action::MoveDown => reorder(state, 1),
        Action::ProviderTest => test(state),
        Action::ProviderImport => {
            state.modal = Some(Modal::Prompt(PromptState::new(
                PromptKind::ProvidersImport,
                "Import providers",
                "Path of a providers TOML file to read:",
                "",
            )));
            Vec::new()
        }
        Action::ProviderExport => {
            state.modal = Some(Modal::Prompt(PromptState::new(
                PromptKind::ProvidersExport,
                "Export providers",
                "File to write the custom providers to (must not exist):",
                "dman-providers.toml",
            )));
            Vec::new()
        }
        Action::ProviderRemove => confirm_remove(state),
        _ => return None,
    };
    Some(cmds)
}

fn toggle(state: &mut State) -> Vec<Cmd> {
    let Some(p) = state.providers.current() else {
        return Vec::new();
    };
    let params = ProviderSetParams {
        name: p.name.clone(),
        enabled: Some(!p.enabled),
        ..ProviderSetParams::default()
    };
    state.call(ApiCall::ProvidersSet(params))
}

fn test(state: &mut State) -> Vec<Cmd> {
    let Some(name) = state.providers.current().map(|p| p.name.clone()) else {
        return Vec::new();
    };
    if state.providers.tests.get(&name) == Some(&TestState::Running) {
        state.status.info(format!("{name} is already being tested"));
        return Vec::new();
    }
    let cmds = state.call_quiet(ApiCall::ProvidersTest(name.clone()));
    if !cmds.is_empty() {
        state
            .providers
            .tests
            .insert(name.clone(), TestState::Running);
        state
            .status
            .info(format!("Testing {name} in the background…"));
    }
    cmds
}

/// Move the provider under the cursor one place and tell the daemon.
fn reorder(state: &mut State, delta: isize) -> Vec<Cmd> {
    if let Some(reason) = state.offline_reason() {
        state.status.error(format!("Move is disabled: {reason}"));
        return Vec::new();
    }
    let p = &mut state.providers;
    let i = p.cursor;
    let Some(j) = i.checked_add_signed(delta).filter(|j| *j < p.list.len()) else {
        state.status.info("Already at the end of the list");
        return Vec::new();
    };
    let before: Vec<(String, i32)> = p.list.iter().map(|x| (x.name.clone(), x.order)).collect();
    let moved = p.list[i].name.clone();
    if p.list[i].order == p.list[j].order {
        // Tied orders cannot be swapped: renumber the whole list.
        p.list.swap(i, j);
        for (n, x) in p.list.iter_mut().enumerate() {
            x.order = i32::try_from(n).unwrap_or(i32::MAX);
        }
    } else {
        let (a, b) = (p.list[i].order, p.list[j].order);
        p.list[i].order = b;
        p.list[j].order = a;
    }
    p.sort();
    p.select(&moved);
    let changes: Vec<(String, i32)> = p
        .list
        .iter()
        .filter(|x| before.iter().any(|(n, o)| *n == x.name && *o != x.order))
        .map(|x| (x.name.clone(), x.order))
        .collect();
    let mut cmds = Vec::new();
    for (name, order) in changes {
        cmds.extend(state.call_quiet(ApiCall::ProvidersSet(ProviderSetParams {
            name,
            order: Some(order),
            ..ProviderSetParams::default()
        })));
    }
    cmds
}

fn confirm_remove(state: &mut State) -> Vec<Cmd> {
    let Some(p) = state.providers.current() else {
        return Vec::new();
    };
    if p.builtin {
        state.status.info(format!(
            "{} is built in: it can be disabled but not removed",
            p.name
        ));
        return Vec::new();
    }
    let name = p.name.clone();
    state.modal = Some(Modal::Confirm(
        ConfirmState::yes_no(
            format!("Remove provider {name}?"),
            vec![
                "The custom provider is deleted from the configuration.".into(),
                "Export it first (e) if you want to keep the definition.".into(),
            ],
            ApiCall::ProvidersRemove(name),
        )
        .label("Remove")
        .destructive(),
    ));
    Vec::new()
}

/// A prompt of this view was submitted.
pub fn submit(state: &mut State, kind: PromptKind, value: &str) -> Vec<Cmd> {
    if value.is_empty() {
        state.status.error("Enter a file path");
        return Vec::new();
    }
    match kind {
        PromptKind::ProvidersImport => {
            state.status.info(format!("Reading {value}…"));
            vec![Cmd::ReadProviderFile(value.to_owned())]
        }
        PromptKind::ProvidersExport => state.call(ApiCall::ProvidersExport {
            path: value.to_owned(),
        }),
        _ => Vec::new(),
    }
}

/// The import file was read (or not).
pub fn on_file_read(state: &mut State, result: Result<(String, String), String>) -> Vec<Cmd> {
    match result {
        Ok((_, text)) => state.call(ApiCall::ProvidersImport(text)),
        Err(e) => {
            state.status.error(format!("Import failed: {e}"));
            Vec::new()
        }
    }
}

/// The export file was written (or not).
pub fn on_file_written(state: &mut State, result: Result<String, String>) {
    match result {
        Ok(path) => state.status.info(format!("Exported providers to {path}")),
        Err(e) => state.status.error(format!("Export failed: {e}")),
    }
}

/// Handle the reply of a providers call; `None` when it is not one of ours.
pub fn on_result(
    state: &mut State,
    call: &ApiCall,
    result: &Result<ApiReply, String>,
) -> Option<Vec<Cmd>> {
    let cmds = match (call, result) {
        (ApiCall::ProvidersList, Ok(ApiReply::Providers(list))) => {
            let keep = state.providers.current().map(|p| p.name.clone());
            let p = &mut state.providers;
            p.list = list.clone();
            p.loaded = true;
            p.sort();
            if let Some(n) = keep {
                p.select(&n);
            }
            p.cursor = p.cursor.min(p.list.len().saturating_sub(1));
            search::on_providers_loaded(state);
            Vec::new()
        }
        (ApiCall::ProvidersList, Err(e)) => {
            state.status.error(format!("Loading providers failed: {e}"));
            Vec::new()
        }
        (ApiCall::ProvidersSet(params), Ok(ApiReply::Provider(info))) => {
            let keep = state.providers.current().map(|p| p.name.clone());
            let p = &mut state.providers;
            match p.list.iter_mut().find(|x| x.name == info.name) {
                Some(x) => *x = (**info).clone(),
                None => p.list.push((**info).clone()),
            }
            p.sort();
            if let Some(n) = keep {
                p.select(&n);
            }
            if params.enabled.is_some() {
                if info.enabled && !info.verified {
                    state
                        .status
                        .warn(format!("{} enabled — {UNVERIFIED_HINT}", info.name));
                } else {
                    let what = if info.enabled { "enabled" } else { "disabled" };
                    state.status.info(format!("{} {what}", info.name));
                }
            }
            search::on_providers_loaded(state);
            Vec::new()
        }
        (ApiCall::ProvidersSet(_), Err(e)) => {
            state
                .status
                .error(format!("Saving the provider failed: {e}"));
            fetch(state)
        }
        (ApiCall::ProvidersTest(name), r) => {
            let t = match r {
                Ok(ApiReply::ProviderTest(rep)) => TestState::Done(rep.clone()),
                Ok(_) => TestState::Failed("unexpected reply".into()),
                Err(e) => TestState::Failed(e.clone()),
            };
            let text = match &t {
                TestState::Done(rep) if rep.ok => {
                    format!("{name}: ok, {}", plural(rep.results as usize, "result"))
                }
                TestState::Done(rep) => format!(
                    "{name}: test failed: {}",
                    rep.error.as_deref().unwrap_or("unknown error")
                ),
                TestState::Failed(e) => format!("{name}: test failed: {e}"),
                TestState::Running => String::new(),
            };
            if matches!(&t, TestState::Done(rep) if rep.ok) {
                state.status.info(text);
            } else {
                state.status.warn(text);
            }
            state.providers.tests.insert(name.clone(), t);
            Vec::new()
        }
        (ApiCall::ProvidersImport(_), Ok(ApiReply::Imported(i))) => {
            state.status.info(format!(
                "Imported {}: {}",
                plural(i.names.len(), "provider"),
                i.names.join(", ")
            ));
            fetch(state)
        }
        (ApiCall::ProvidersImport(_), Err(e)) => {
            state.status.error(format!("Import failed: {e}"));
            Vec::new()
        }
        (ApiCall::ProvidersExport { path }, Ok(ApiReply::Exported(toml))) => {
            vec![Cmd::WriteProviderFile {
                path: path.clone(),
                content: toml.clone(),
            }]
        }
        (ApiCall::ProvidersExport { .. }, Err(e)) => {
            state.status.error(format!("Export failed: {e}"));
            Vec::new()
        }
        (ApiCall::ProvidersRemove(name), Ok(_)) => {
            state.providers.tests.remove(name);
            state.status.info(format!("Removed provider {name}"));
            fetch(state)
        }
        (ApiCall::ProvidersRemove(name), Err(e)) => {
            state.status.error(format!("Removing {name} failed: {e}"));
            Vec::new()
        }
        _ => return None,
    };
    Some(cmds)
}

// ---------------------------------------------------------------- drawing

fn test_lines(state: &State, p: &ProviderInfo) -> Vec<Line<'static>> {
    let t = &state.theme;
    match state.providers.tests.get(&p.name) {
        None => vec![Line::styled(
            format!(
                "Not tested yet — press {} to test {}",
                state
                    .keymap
                    .label(crate::keymap::Context::Providers, Action::ProviderTest),
                p.name
            ),
            t.dim,
        )],
        Some(TestState::Running) => vec![Line::styled(
            format!(
                "Testing {} {}",
                p.name,
                SPINNER[state.search.spin % SPINNER.len()]
            ),
            t.warn,
        )],
        Some(TestState::Failed(e)) => vec![Line::styled(format!("✗ test failed: {e}"), t.error)],
        Some(TestState::Done(r)) => {
            let mut out = Vec::new();
            if r.ok {
                out.push(Line::styled(
                    format!(
                        "✓ ok: {} in {} ms",
                        plural(r.results as usize, "result"),
                        r.elapsed_ms
                    ),
                    t.ok,
                ));
            } else {
                out.push(Line::styled(
                    format!(
                        "✗ failed after {} ms: {}",
                        r.elapsed_ms,
                        r.error.as_deref().unwrap_or("unknown error")
                    ),
                    t.error,
                ));
            }
            if let Some(s) = &r.sample {
                let size = s.size.map_or("-".into(), format::size);
                let seeders = s.seeders.map_or("-".into(), |n| n.to_string());
                out.push(Line::styled(
                    format!("  sample: {} ({size}, {seeders} seeders)", s.title),
                    t.dim,
                ));
            }
            out
        }
    }
}

fn detail_lines(state: &State) -> Vec<Line<'static>> {
    let t = &state.theme;
    let Some(p) = state.providers.current() else {
        return Vec::new();
    };
    let mut out = vec![Line::from(vec![
        Span::styled(format!("{} ", p.name), t.accent),
        Span::styled(p.url.clone(), t.dim),
    ])];
    if let Some(d) = &p.description {
        out.push(Line::styled(d.clone(), t.text));
    }
    let mut facts = vec![format!("max {} results", p.max_results)];
    if let Some(r) = p.rate_limit_per_min {
        facts.push(format!("rate limit {r}/min"));
    }
    if let Some(k) = &p.api_key_ref {
        facts.push(format!("API key: secret '{k}'"));
    }
    out.push(Line::styled(facts.join(" · "), t.dim));
    if !p.verified {
        let text = if p.enabled {
            format!("⚠ enabled — {UNVERIFIED_HINT}")
        } else {
            format!("unverified: {UNVERIFIED_HINT}")
        };
        out.push(Line::styled(text, if p.enabled { t.warn } else { t.dim }));
    }
    out.extend(test_lines(state, p));
    out
}

/// Draw the providers settings.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let p = &state.providers;
    let block = t.block_focus(true).title(Span::styled(
        format!(
            " Search providers · {} enabled of {} ",
            enabled(state).count(),
            p.list.len()
        ),
        t.accent,
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let detail_h = if inner.height >= 12 { 7 } else { 0 };
    let list = Rect {
        height: inner.height - detail_h,
        ..inner
    };
    let detail = Rect {
        y: list.bottom(),
        height: detail_h,
        ..inner
    };
    if p.list.is_empty() {
        let text = if p.loaded {
            "No providers"
        } else {
            "Loading providers…"
        };
        let msg = Rect {
            y: list.y + list.height / 2,
            height: 1.min(list.height),
            ..list
        };
        frame.render_widget(
            Paragraph::new(Span::styled(text, t.dim)).alignment(Alignment::Center),
            msg,
        );
        return;
    }
    let cols = [
        Col::fixed("On", 3, 0),
        Col::fill("Name", 8, 0),
        Col::fixed("Kind", 7, 3),
        Col::fixed("Verified", 10, 1),
        Col::fixed("Origin", 7, 4),
        Col::fixed("Order", 5, 5).right(),
        Col::fixed("Timeout", 7, 2).right(),
    ];
    let rows: Vec<(Vec<table::Cell>, Style)> = p
        .list
        .iter()
        .map(|x| {
            let (badge, badge_style) = if x.verified {
                ("verified", t.ok)
            } else {
                ("unverified", t.warn)
            };
            let testing = p.tests.get(&x.name) == Some(&TestState::Running);
            let name = if testing {
                format!("{} {}", x.name, SPINNER[state.search.spin % SPINNER.len()])
            } else {
                x.name.clone()
            };
            let plain = |s: String| (s, Style::new());
            (
                vec![
                    (
                        if x.enabled { "[x]" } else { "[ ]" }.to_owned(),
                        if x.enabled { t.ok } else { t.dim },
                    ),
                    plain(name),
                    plain(x.kind.clone()),
                    (badge.to_owned(), badge_style),
                    plain(if x.builtin { "builtin" } else { "custom" }.to_owned()),
                    plain(x.order.to_string()),
                    plain(format!("{}s", x.timeout_secs)),
                ],
                Style::new(),
            )
        })
        .collect();
    table::render(frame, list, t, &cols, &rows, Some(p.cursor));
    if detail_h > 0 {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(t.border);
        let d = block.inner(detail);
        frame.render_widget(block, detail);
        frame.render_widget(
            Paragraph::new(detail_lines(state)).wrap(Wrap { trim: false }),
            d,
        );
    }
}
