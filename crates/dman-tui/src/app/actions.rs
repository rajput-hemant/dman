//! Keys, paste, modal outcomes and actions: how input turns into state
//! changes and commands.

use super::*;

pub(super) fn on_key(state: &mut State, key: KeyEvent) -> Vec<Cmd> {
    if key.kind == KeyEventKind::Release {
        state.dirty = false;
        return Vec::new();
    }
    if let Some(mut modal) = state.modal.take() {
        let outcome = modal.handle_key(key, state);
        state.modal = Some(modal);
        return on_modal_outcome(state, outcome);
    }
    if let Some(cmds) = crate::widgets::whichkey::handle_key(state, key) {
        return cmds;
    }
    if let Some(cmds) = views::handle_raw_key(state, key) {
        return cmds;
    }
    match state.keymap.resolve_event(state.context(), key) {
        Some(action) => apply_action(state, action),
        None => {
            state.dirty = false;
            Vec::new()
        }
    }
}

pub(super) fn on_paste(state: &mut State, text: &str) -> Vec<Cmd> {
    if let Some(mut modal) = state.modal.take() {
        modal.paste(text, state);
        state.modal = Some(modal);
        return Vec::new();
    }
    views::on_paste(state, text)
}

/// Act on what a modal decided.
pub fn on_modal_outcome(state: &mut State, outcome: ModalOutcome) -> Vec<Cmd> {
    match outcome {
        ModalOutcome::Stay => Vec::new(),
        ModalOutcome::Close => {
            state.modal = None;
            Vec::new()
        }
        ModalOutcome::Run(action) => {
            state.modal = None;
            apply_action(state, action)
        }
        ModalOutcome::Api(call) => {
            state.modal = None;
            state.call(call)
        }
        ModalOutcome::LeaveView => {
            state.modal = None;
            if state.views.len() > 1 {
                state.views.pop();
            }
            Vec::new()
        }
        ModalOutcome::Replace(m) => {
            state.modal = Some(m);
            Vec::new()
        }
        ModalOutcome::Submit(kind, value) => {
            state.modal = None;
            on_submit(state, kind, &value)
        }
    }
}

fn on_submit(state: &mut State, kind: PromptKind, value: &str) -> Vec<Cmd> {
    match kind {
        PromptKind::JobOption(field) => views::detail::submit_option(state, field, value),
        PromptKind::Rename { id, file_index } => {
            views::rename::submit(state, id, file_index, value)
        }
        PromptKind::SearchSeeders
        | PromptKind::SearchSize
        | PromptKind::SearchSource
        | PromptKind::SearchTitle => views::search::submit(state, kind, value),
        PromptKind::ProvidersImport | PromptKind::ProvidersExport => {
            views::providers::submit(state, kind, value)
        }
        PromptKind::VideoQuality => views::add::quality::submit_default(state, false, value),
        PromptKind::VideoAudio => views::add::quality::submit_default(state, true, value),
        PromptKind::MaxConcurrent => match value.parse::<u32>() {
            Ok(n) if (1..=1000).contains(&n) => {
                state.call(ApiCall::SetLimits(dman_proto::method::LimitsUpdate {
                    max_concurrent: Some(n),
                    ..Default::default()
                }))
            }
            _ => {
                state.status.error(format!(
                    "Max concurrent must be a number from 1 to 1000, got '{value}'"
                ));
                Vec::new()
            }
        },
    }
}

/// Apply an action in the context of the top view.
pub fn apply_action(state: &mut State, action: Action) -> Vec<Cmd> {
    if action.needs_connection()
        && let Some(reason) = state.offline_reason()
    {
        state
            .status
            .error(format!("{} is disabled: {reason}", action.title()));
        return Vec::new();
    }
    // Add-view actions run from the palette elsewhere open the view first.
    let mut cmds = Vec::new();
    if action.home_context() == Context::Add && !matches!(state.view(), View::Add(_)) {
        cmds = views::add::push(state, views::add::AddState::default());
    }
    cmds.extend(apply_action_inner(state, action));
    cmds
}

fn apply_action_inner(state: &mut State, action: Action) -> Vec<Cmd> {
    match action {
        Action::Quit => vec![Cmd::Quit],
        Action::Palette => {
            state.modal = Some(Modal::Palette(PaletteState::new(state)));
            Vec::new()
        }
        Action::Help => {
            if matches!(state.view(), View::Help(_)) {
                state.views.pop();
            } else {
                state
                    .views
                    .push(View::Help(views::help::HelpState::default()));
            }
            Vec::new()
        }
        Action::Back => {
            if let View::Add(a) = state.view()
                && a.overlay.is_none()
                && a.is_dirty()
            {
                state.modal = Some(Modal::Confirm(views::add::discard_confirm()));
                return Vec::new();
            }
            if state.views.len() > 1 {
                state.views.pop();
            }
            Vec::new()
        }
        Action::Add => views::add::push(state, views::add::AddState::default()),
        Action::Shutdown => {
            state.modal = Some(Modal::Confirm(views::downloads::shutdown_confirm()));
            Vec::new()
        }
        Action::VideoQualityToggle => views::add::quality::toggle_ask(state),
        Action::VideoDefaultQuality => views::add::quality::edit_default(state, false),
        Action::VideoDefaultAudio => views::add::quality::edit_default(state, true),
        Action::Search => views::search::open(state),
        Action::ToggleMouse => toggle_mouse(state),
        Action::Undo => undo(state),
        Action::SearchProviders => views::providers::open(state),
        Action::Filter if !matches!(state.view(), View::Search) => {
            state.views.truncate(1);
            state.modal = Some(Modal::Filter(FilterState::new(&state.list.filter)));
            Vec::new()
        }
        _ => {
            let handled = views::apply_action(state, action);
            handled.unwrap_or_else(|| {
                state
                    .status
                    .info(format!("{} is not available in this view", action.title()));
                Vec::new()
            })
        }
    }
}

/// `m`: switch mouse support on or off now (and remember it in the config).
pub fn toggle_mouse(state: &mut State) -> Vec<Cmd> {
    state.mouse = !state.mouse;
    let mut cmds = vec![Cmd::SetMouseCapture(state.mouse)];
    if state.ui.persist_mouse {
        cmds.push(Cmd::SaveMouse(state.mouse));
    }
    let key = crate::widgets::footer::key_label_for(state, Action::ToggleMouse).unwrap_or_default();
    state.status.info(if state.mouse {
        format!("Mouse on — click, wheel, tabs and buttons work; Shift-drag selects text ({key} turns it off)")
    } else {
        format!("Mouse off — the terminal selects text again ({key} turns it on)")
    });
    cmds
}

/// `u`: undo the last remove-from-list while its toast is up; otherwise
/// unmark (the key's old meaning).
fn undo(state: &mut State) -> Vec<Cmd> {
    if let Some(ids) = state.status.toasts.take_undo() {
        return state.call(ApiCall::Restore(ids));
    }
    if matches!(state.view(), View::Downloads) {
        return apply_action_inner(state, Action::SelectNone);
    }
    state.status.info("Nothing to undo");
    Vec::new()
}
