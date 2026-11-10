//! Daemon events, connection changes and call results.

use super::*;

pub(super) fn apply_event(state: &mut State, event: Event) -> Vec<Cmd> {
    match event {
        Event::Snapshot {
            jobs,
            stats,
            limits,
        } => {
            state.jobs = jobs.into_iter().map(|j| (j.id, j)).collect();
            state.stats = stats;
            state.limits = limits;
            state.refresh_list();
            views::detail::close_if_gone(state);
            return crate::widgets::welcome::on_snapshot(state);
        }
        Event::JobsUpdated { jobs } => {
            for j in jobs {
                if state.ui.animations
                    && let Some(old) = state.jobs.get(&j.id)
                {
                    state.anim.on_change(old, &j);
                }
                state.jobs.insert(j.id, j);
            }
            state.refresh_list();
        }
        Event::JobsRemoved { ids } => {
            for id in ids {
                state.jobs.remove(&id);
            }
            state.refresh_list();
            views::detail::close_if_gone(state);
        }
        Event::Stats { stats } => {
            state
                .graph
                .push_live(stats.download_speed, stats.upload_speed);
            sample_job_speeds(state);
            state.stats = stats;
        }
        Event::Limits { limits } => state.limits = limits,
        Event::Message { level, text } => state.status.set(level, text),
        Event::Engine { status } => {
            state.engines.retain(|e| e.engine != status.engine);
            state.engines.push(status);
        }
        Event::CleanupPending {
            id,
            name,
            count,
            bytes,
        } => {
            // List the files first; the answer opens the confirmation unless
            // another dialog is open by then.
            if state.connected() && state.modal.is_none() {
                return state.call_quiet(ApiCall::Cleanup { id, dry_run: true });
            }
            state.status.info(format!(
                "{name}: {count} unselected partial files ({}) can be deleted (palette: cleanup)",
                crate::format::size(bytes)
            ));
        }
        Event::SearchUpdate {
            search_id,
            provider,
            status,
            results,
        } => views::search::on_update(state, search_id, &provider, status, results),
        Event::SearchFinished { search_id } => views::search::on_finished(state, search_id),
        Event::JobFinished {
            name,
            path,
            outcome,
            ..
        } => {
            let (title, body, level) = match &outcome {
                JobOutcome::Completed => ("Download complete", name.clone(), Level::Info),
                JobOutcome::SeedCompleted => ("Seeding finished", name.clone(), Level::Info),
                JobOutcome::Failed { error } => {
                    ("Download failed", format!("{name}: {error}"), Level::Error)
                }
            };
            state.status.set(level, format!("{title}: {body}"));
            if state.desktop && state.connected() {
                let body = if level == Level::Info && path != name {
                    format!("{name}\n{path}")
                } else {
                    body
                };
                return vec![Cmd::Notify {
                    title: title.to_owned(),
                    body,
                }];
            }
        }
    }
    Vec::new()
}

fn sample_job_speeds(state: &mut State) {
    state.job_speeds.retain(|id, _| state.jobs.contains_key(id));
    for j in state.jobs.values() {
        let v = match j.state {
            JobState::Active => j.download_speed,
            JobState::Seeding => j.upload_speed,
            _ => {
                if let Some(h) = state.job_speeds.get_mut(&j.id)
                    && h.last().is_some_and(|x| *x != 0)
                {
                    h.push(0);
                }
                continue;
            }
        };
        let h = state.job_speeds.entry(j.id).or_default();
        h.push(v);
        let extra = h.len().saturating_sub(JOB_SPEED_SAMPLES);
        h.drain(..extra);
    }
}

pub(super) fn on_conn(state: &mut State, conn: ConnState) -> Vec<Cmd> {
    let mut cmds = Vec::new();
    match &conn {
        ConnState::Connected { daemon_version } => {
            if matches!(state.conn, ConnState::Disconnected { .. }) {
                state
                    .status
                    .info(format!("Reconnected to the daemon ({daemon_version})"));
            }
            // The header graph starts with the daemon's recent history
            // (not counted as pending; the answer fills the graph).
            state.graph.inflight = false;
            if state.graph.history.is_empty() {
                cmds.push(Cmd::Api(ApiCall::StatsHistory));
            }
            // The daemon only pushes engine changes; ask for the current
            // status after every (re)connect (not counted as pending).
            cmds.push(Cmd::Api(ApiCall::EngineStatus));
            cmds.push(Cmd::Api(ApiCall::VideoPrefsGet(PrefsIntent::Cache)));
        }
        ConnState::Disconnected { retry_in, .. } => {
            state.retry_left = *retry_in;
            state.pending = 0;
            views::detail::on_disconnect(state);
            graph::on_disconnect(state);
            views::add::on_disconnect(state);
            views::search::on_disconnect(state);
        }
        ConnState::Failed { error } => {
            state.status.error(format!("Cannot connect: {error}"));
            state.pending = 0;
        }
        ConnState::Connecting => {}
    }
    state.conn = conn;
    cmds
}

pub(super) fn on_result(
    state: &mut State,
    call: &ApiCall,
    result: &Result<ApiReply, String>,
) -> Vec<Cmd> {
    if let ApiCall::Detail(id) = call {
        views::detail::on_detail_result(state, *id, result);
        return Vec::new();
    }
    if matches!(call, ApiCall::StatsHistory) {
        graph::on_result(state, result);
        return Vec::new();
    }
    if matches!(call, ApiCall::VideoPrefsGet(PrefsIntent::Cache)) {
        views::add::quality::on_prefs(state, PrefsIntent::Cache, result);
        return Vec::new();
    }
    if matches!(call, ApiCall::EngineStatus) {
        // Background refresh: never counted, silent on failure.
        if let Ok(ApiReply::Engines(list)) = result {
            for status in list {
                state.engines.retain(|e| e.engine != status.engine);
                state.engines.push(status.clone());
            }
        }
        return Vec::new();
    }
    state.pending = state.pending.saturating_sub(1);
    if let ApiCall::PreviewMagnet { uri } = call {
        views::add::on_magnet_preview(state, uri, result);
        return Vec::new();
    }
    if let ApiCall::VideoProbe(params) = call {
        return views::add::on_video_probe(state, params, result);
    }
    if let ApiCall::VideoPrefsGet(intent) = call {
        return views::add::quality::on_prefs(state, *intent, result);
    }
    if let (ApiCall::VideoPrefsSet(_), Ok(ApiReply::VideoPreferences(p))) = (call, result) {
        state.video_prefs = Some((**p).clone());
    }
    if matches!(call, ApiCall::FsList(_)) {
        views::add::on_fs_listing(state, result);
        return Vec::new();
    }
    if let Some(cmds) = views::search::on_result(state, call, result)
        .or_else(|| views::providers::on_result(state, call, result))
    {
        return cmds;
    }
    if let ApiCall::Cleanup { id, dry_run: true } = call {
        return match result {
            Ok(ApiReply::Cleanup(r)) if r.files.is_empty() => {
                state.status.info("No unselected partial files to delete");
                Vec::new()
            }
            Ok(ApiReply::Cleanup(_)) if state.modal.is_some() => {
                state
                    .status
                    .info("Unselected partial files can be deleted (palette: cleanup)");
                Vec::new()
            }
            Ok(ApiReply::Cleanup(r)) => {
                let name = state
                    .jobs
                    .get(id)
                    .map(|j| j.name.clone())
                    .unwrap_or_default();
                let files: Vec<(String, u64)> =
                    r.files.iter().map(|f| (f.path.clone(), f.bytes)).collect();
                state.modal = Some(Modal::Confirm(views::downloads::cleanup_confirm(
                    *id, &name, &files,
                )));
                Vec::new()
            }
            Err(e) => {
                state.status.error(format!("Cleanup failed: {e}"));
                Vec::new()
            }
            Ok(_) => Vec::new(),
        };
    }
    if let Ok(ApiReply::Limits(l)) = result {
        state.limits = l.clone();
    }
    let (outcome, text) = summarize(call, result);
    let own_message = matches!(
        (call, result),
        (ApiCall::Remove { .. } | ApiCall::Restore(_), Ok(ApiReply::Batch(b))) if b.failed.is_empty()
    );
    match outcome {
        Outcome::Ok if own_message => {}
        Outcome::Ok => state.status.info(text),
        Outcome::Partial => state.status.warn(text),
        Outcome::Failed => state.status.error(text),
    }
    if let (ApiCall::Remove { ids, delete_files }, Ok(ApiReply::Batch(b))) = (call, result)
        && b.failed.is_empty()
    {
        let text = views::downloads::removed_message(state, ids, *delete_files);
        if *delete_files {
            // Files are gone: never offer an undo for these.
            state.status.info(text);
        } else {
            // Files were kept: the daemon can restore the jobs for a while.
            state.status.toasts.drop_undo();
            state.status.toasts.push_full(
                Level::Info,
                text.clone(),
                Some(ToastAction::Undo(ids.clone())),
                UNDO_TTL,
            );
            state.status.progress(text);
        }
    }
    if let (ApiCall::Restore(ids), Ok(ApiReply::Batch(b))) = (call, result)
        && b.failed.is_empty()
    {
        let text = views::downloads::restored_message(state, ids);
        state.status.info(text);
        if let Some(id) = ids.first() {
            state.list.focus_when_listed(*id);
        }
    }
    if let ApiCall::Add(params) = call {
        views::add::on_add_result(state, params, result);
    }
    if *call == ApiCall::Shutdown && result.is_ok() {
        return vec![Cmd::Quit];
    }
    if let ApiCall::SetOptions { id, .. } | ApiCall::Rename { id, .. } = call {
        // Success or refusal, re-read so the view shows what is really stored.
        return views::detail::refetch(state, *id);
    }
    Vec::new()
}
