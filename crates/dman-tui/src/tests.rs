//! Tests of `update` (behaviour) and full-screen snapshots of `render`.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use dman_client::{ClientError, ConnState};
use dman_proto::method::{BatchResult, EngineStatus, Event, LimitsUpdate, Reorder};
use dman_proto::{Engine, JobId, JobState};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::action::Action;
use crate::api::{ApiCall, ApiReply};
use crate::app::{Cmd, Msg, State, StateOptions, update};
use crate::fixtures::{job, sample_detail, sample_jobs, sample_limits, sample_stats};
use crate::theme::Theme;
use crate::views::{self, View};
use crate::widgets::Modal;

fn state_with(mouse: bool) -> State {
    let mut s = State::new(StateOptions {
        theme: Theme::default_theme(),
        mouse,
        ..StateOptions::default()
    });
    update(
        &mut s,
        Msg::Conn(ConnState::Connected {
            daemon_version: "0.1.0".into(),
        }),
    );
    update(
        &mut s,
        Msg::Event(Event::Snapshot {
            jobs: sample_jobs(),
            stats: sample_stats(),
            limits: sample_limits(),
        }),
    );
    s
}

fn state() -> State {
    state_with(false)
}

fn press(s: &mut State, code: KeyCode, mods: KeyModifiers) -> Vec<Cmd> {
    update(s, Msg::Key(KeyEvent::new(code, mods)))
}

fn key(s: &mut State, c: char) -> Vec<Cmd> {
    let mods = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    press(s, KeyCode::Char(c), mods)
}

fn code(s: &mut State, c: KeyCode) -> Vec<Cmd> {
    press(s, c, KeyModifiers::NONE)
}

fn typed(s: &mut State, text: &str) {
    for c in text.chars() {
        key(s, c);
    }
}

fn disconnect(s: &mut State) {
    update(
        s,
        Msg::Conn(ConnState::Disconnected {
            retry_in: Duration::from_secs(3),
            last_error: "cannot connect to daemon: connection refused".into(),
        }),
    );
}

fn status_text(s: &State) -> String {
    s.status
        .last
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- behaviour

#[test]
fn navigation_keys_move_the_cursor() {
    let mut s = state();
    assert_eq!(s.list.cursor, Some(JobId(1)));
    key(&mut s, 'j');
    code(&mut s, KeyCode::Down);
    assert_eq!(s.list.cursor, Some(JobId(3)));
    key(&mut s, 'G');
    assert_eq!(s.list.cursor, Some(JobId(7)));
    code(&mut s, KeyCode::Home);
    assert_eq!(s.list.cursor, Some(JobId(1)));
    key(&mut s, 'k');
    assert_eq!(s.list.cursor, Some(JobId(1)));
}

#[test]
fn unbound_key_does_nothing() {
    let mut s = state();
    s.dirty = false;
    assert!(key(&mut s, 'z').is_empty());
    assert!(!s.dirty);
}

#[test]
fn live_filter_and_cancel() {
    let mut s = state();
    key(&mut s, '/');
    assert!(matches!(s.modal, Some(Modal::Filter(_))));
    typed(&mut s, "iso");
    assert_eq!(s.list.filter, "iso");
    assert_eq!(s.list.order, vec![JobId(1), JobId(3), JobId(5)]);
    code(&mut s, KeyCode::Enter);
    assert!(s.modal.is_none());
    assert_eq!(s.list.filter, "iso");
    // Edit again and cancel: the previous filter comes back.
    key(&mut s, '/');
    typed(&mut s, "zzz");
    assert!(s.list.order.is_empty());
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.list.filter, "iso");
    assert_eq!(s.list.order.len(), 3);
    press(&mut s, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert!(s.list.filter.is_empty());
    assert_eq!(s.list.order.len(), 7);
}

#[test]
fn filter_keys_are_text_not_actions() {
    let mut s = state();
    key(&mut s, '/');
    // `q` and `p` would quit / pause outside the filter.
    assert!(key(&mut s, 'q').is_empty());
    assert!(key(&mut s, 'p').is_empty());
    assert_eq!(s.list.filter, "qp");
}

#[test]
fn sort_cycle_and_reverse() {
    let mut s = state();
    key(&mut s, 's');
    assert_eq!(s.list.sort, crate::list::SortKey::Name);
    assert_eq!(status_text(&s), "Sorted by name");
    // Active section by name: big-dataset, debian, ubuntu.
    assert_eq!(&s.list.order[..3], &[JobId(2), JobId(3), JobId(1)]);
    key(&mut s, 'S');
    assert!(s.list.reverse);
    assert_eq!(&s.list.order[..3], &[JobId(1), JobId(3), JobId(2)]);
}

#[test]
fn selection_drives_targets() {
    let mut s = state();
    key(&mut s, ' ');
    assert_eq!(s.list.cursor, Some(JobId(2)), "space moves down");
    key(&mut s, 'j');
    key(&mut s, 'v');
    let cmds = key(&mut s, 'p');
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::Pause(vec![JobId(1), JobId(2), JobId(3)]))]
    );
    code(&mut s, KeyCode::Esc);
    assert!(s.list.selected.is_empty());
    press(&mut s, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(s.list.selected.len(), 7);
    // Without a selection the cursor job is the target.
    key(&mut s, 'u');
    let cmds = key(&mut s, 'r');
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::Resume(vec![JobId(3)]))]);
    assert_eq!(s.pending, 2);
}

#[test]
fn remove_needs_confirmation() {
    let mut s = state();
    assert!(key(&mut s, 'd').is_empty());
    let Some(Modal::Confirm(c)) = &s.modal else {
        panic!("no confirmation")
    };
    assert_eq!(c.title, "Remove 1 download?");
    assert!(key(&mut s, 'n').is_empty());
    assert!(s.modal.is_none());
    key(&mut s, 'd');
    let cmds = key(&mut s, 'y');
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::Remove {
            ids: vec![JobId(1)],
            delete_files: false
        })]
    );
    assert!(s.modal.is_none());
}

#[test]
fn delete_files_needs_two_distinct_confirmations() {
    let mut s = state();
    key(&mut s, 'D');
    assert!(
        key(&mut s, 'y').is_empty(),
        "first yes only opens the second dialog"
    );
    let Some(Modal::Confirm(c)) = &s.modal else {
        panic!("no second confirmation")
    };
    assert_eq!(c.title, "Delete files from disk?");
    // A second `y` or Enter is not enough.
    assert!(key(&mut s, 'y').is_empty());
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    assert!(s.modal.is_some());
    code(&mut s, KeyCode::Backspace);
    typed(&mut s, "DELETE");
    let cmds = code(&mut s, KeyCode::Enter);
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::Remove {
            ids: vec![JobId(1)],
            delete_files: true
        })]
    );
}

#[test]
fn delete_files_can_be_cancelled_at_second_step() {
    let mut s = state();
    key(&mut s, 'D');
    key(&mut s, 'y');
    typed(&mut s, "DELETE");
    assert!(code(&mut s, KeyCode::Esc).is_empty());
    assert!(s.modal.is_none());
}

#[test]
fn purge_targets_stopped_jobs() {
    let mut s = state();
    key(&mut s, 'x');
    let cmds = key(&mut s, 'y');
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::Purge(vec![JobId(6), JobId(7)]))]
    );
}

#[test]
fn shutdown_confirms_and_quits_on_success() {
    let mut s = state();
    key(&mut s, 'Q');
    let cmds = key(&mut s, 'y');
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::Shutdown)]);
    let cmds = update(
        &mut s,
        Msg::ActionResult {
            call: ApiCall::Shutdown,
            result: Ok(ApiReply::Done),
        },
    );
    assert_eq!(cmds, vec![Cmd::Quit]);
}

#[test]
fn cleanup_event_lists_then_asks_then_deletes_only_after_yes() {
    use dman_proto::method::{CleanupFile, CleanupResult};
    let mut s = state();
    let cmds = update(
        &mut s,
        Msg::Event(Event::CleanupPending {
            id: JobId(7),
            name: "Album".into(),
            count: 2,
            bytes: 8,
        }),
    );
    // The daemon is asked for the real file list first; nothing is deleted.
    let list = ApiCall::Cleanup {
        id: JobId(7),
        dry_run: true,
    };
    assert_eq!(cmds, vec![Cmd::Api(list.clone())]);
    assert!(s.modal.is_none());
    let found = CleanupResult {
        files: vec![
            CleanupFile {
                path: "Album/sub/02.bin".into(),
                bytes: 5,
            },
            CleanupFile {
                path: "Album/03.bin".into(),
                bytes: 3,
            },
        ],
        bytes: 8,
        deleted: 0,
        dry_run: true,
    };
    update(
        &mut s,
        Msg::ActionResult {
            call: list,
            result: Ok(ApiReply::Cleanup(Box::new(found))),
        },
    );
    let Some(Modal::Confirm(c)) = &s.modal else {
        panic!("expected a confirmation dialog");
    };
    assert_eq!(c.title, "Delete 2 unselected partial files (8 B)?");
    // Declining sends nothing.
    assert!(code(&mut s, KeyCode::Esc).is_empty());
    assert!(s.modal.is_none());
    // The cleanup key asks the daemon for the list again.
    let cmds = key(&mut s, 'C');
    assert!(matches!(
        cmds.as_slice(),
        [Cmd::Api(ApiCall::Cleanup { dry_run: true, .. })]
    ));
}

#[test]
fn cleanup_confirmation_calls_the_real_delete() {
    let mut s = state();
    s.modal = Some(Modal::Confirm(views::downloads::cleanup_confirm(
        JobId(7),
        "Album",
        &[("a.bin".into(), 10)],
    )));
    let cmds = key(&mut s, 'y');
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::Cleanup {
            id: JobId(7),
            dry_run: false
        })]
    );
}

#[test]
fn reload_config_action_and_summary() {
    use dman_proto::method::ReloadResult;
    let mut s = state();
    let cmds = crate::app::apply_action(&mut s, Action::ReloadConfig);
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::ReloadConfig)]);
    let r = ReloadResult {
        applied: vec!["limits".into()],
        restart_required: vec!["aria2".into()],
    };
    let (o, text) =
        crate::api::summarize(&ApiCall::ReloadConfig, &Ok(ApiReply::Reloaded(Box::new(r))));
    assert_eq!(o, crate::api::Outcome::Partial);
    assert_eq!(
        text,
        "Config reloaded: applied limits; restart needed for aria2"
    );
}

#[test]
fn disconnected_disables_actions_but_not_navigation() {
    let mut s = state();
    disconnect(&mut s);
    assert!(key(&mut s, 'p').is_empty());
    assert!(status_text(&s).contains("disabled"), "{}", status_text(&s));
    assert!(key(&mut s, 'd').is_empty());
    assert!(s.modal.is_none(), "no confirmation while disconnected");
    assert!(key(&mut s, 'P').is_empty());
    key(&mut s, 'j');
    assert_eq!(s.list.cursor, Some(JobId(2)));
    // Help and quit still work.
    key(&mut s, '?');
    assert!(matches!(s.view(), View::Help(_)));
    assert_eq!(key(&mut s, 'Q'), Vec::<Cmd>::new());
}

#[test]
fn confirmation_open_during_disconnect_does_not_send() {
    let mut s = state();
    key(&mut s, 'd');
    disconnect(&mut s);
    assert!(s.modal.is_some(), "modal survives updates");
    assert!(key(&mut s, 'y').is_empty());
    assert!(status_text(&s).contains("disabled"));
}

#[test]
fn failed_connection_explains_and_allows_quit() {
    let mut s = state();
    update(
        &mut s,
        Msg::Conn(ConnState::Failed {
            error: ClientError::Unauthorized,
        }),
    );
    assert!(
        s.status
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("unauthorized")
    );
    assert!(key(&mut s, 'r').is_empty());
    assert_eq!(key(&mut s, 'q'), vec![Cmd::Quit]);
}

#[test]
fn retry_countdown_ticks() {
    let mut s = state();
    disconnect(&mut s);
    for _ in 0..4 {
        update(&mut s, Msg::Tick);
    }
    assert_eq!(s.retry_left, Duration::from_secs(2));
    // Ticks while connected do not dirty the screen.
    let mut s = state();
    s.dirty = false;
    update(&mut s, Msg::Tick);
    assert!(!s.dirty);
}

#[test]
fn results_update_status_line() {
    let mut s = state();
    let cmds = key(&mut s, 'p');
    let Some(Cmd::Api(call)) = cmds.into_iter().next() else {
        panic!("no call")
    };
    update(
        &mut s,
        Msg::ActionResult {
            call: call.clone(),
            result: Err("request timed out".into()),
        },
    );
    assert_eq!(status_text(&s), "Pause failed: request timed out");
    assert_eq!(s.pending, 0);
    update(
        &mut s,
        Msg::ActionResult {
            call,
            result: Ok(ApiReply::Batch(BatchResult {
                ok: vec![JobId(1)],
                failed: vec![],
            })),
        },
    );
    assert_eq!(status_text(&s), "Pause: 1 download ok");
    assert_eq!(
        s.status.last_error.as_deref(),
        Some("Pause failed: request timed out"),
        "last error is kept"
    );
}

#[test]
fn palette_runs_actions() {
    let mut s = state();
    key(&mut s, ':');
    assert!(matches!(s.modal, Some(Modal::Palette(_))));
    update(&mut s, Msg::Paste("resume all".into()));
    let cmds = code(&mut s, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::ResumeAll)]);
    assert!(s.modal.is_none());
}

#[test]
fn every_action_runs_from_the_palette_without_panicking() {
    for a in Action::ALL {
        let mut s = state();
        crate::app::apply_action(&mut s, *a);
    }
}

#[test]
fn help_toggles_and_q_goes_back() {
    let mut s = state();
    key(&mut s, '?');
    assert!(matches!(s.view(), View::Help(_)));
    key(&mut s, 'j');
    assert!(matches!(s.view(), View::Help(h) if h.scroll == 1));
    assert!(key(&mut s, 'q').is_empty());
    assert!(matches!(s.view(), View::Downloads));
    assert_eq!(key(&mut s, 'q'), vec![Cmd::Quit]);
}

#[test]
fn detail_opens_and_closes() {
    let mut s = state();
    code(&mut s, KeyCode::Enter);
    assert!(matches!(s.view(), View::Detail(d) if d.id == JobId(1)));
    code(&mut s, KeyCode::Esc);
    assert!(matches!(s.view(), View::Downloads));
}

#[test]
fn queue_moves_only_for_queued_jobs() {
    let mut s = state();
    assert!(key(&mut s, 'J').is_empty());
    assert!(status_text(&s).contains("Only waiting"));
    for _ in 0..3 {
        key(&mut s, 'j');
    }
    assert_eq!(
        key(&mut s, 'J'),
        vec![Cmd::Api(ApiCall::Reorder {
            id: JobId(4),
            to: Reorder::By(1)
        })]
    );
    assert_eq!(
        key(&mut s, 'T'),
        vec![Cmd::Api(ApiCall::Reorder {
            id: JobId(4),
            to: Reorder::Top
        })]
    );
}

#[test]
fn limits_and_max_concurrent() {
    let mut s = state();
    assert!(key(&mut s, '+').is_empty(), "already unlimited");
    let cmds = key(&mut s, '-');
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::SetLimits(LimitsUpdate {
            download: Some(8 << 20),
            ..LimitsUpdate::default()
        }))]
    );
    key(&mut s, 'c');
    assert!(matches!(s.modal, Some(Modal::Prompt(_))));
    press(&mut s, KeyCode::Char('u'), KeyModifiers::CONTROL);
    typed(&mut s, "8");
    let cmds = code(&mut s, KeyCode::Enter);
    assert_eq!(
        cmds,
        vec![Cmd::Api(ApiCall::SetLimits(LimitsUpdate {
            max_concurrent: Some(8),
            ..LimitsUpdate::default()
        }))]
    );
    key(&mut s, 'c');
    typed(&mut s, "x");
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    assert!(status_text(&s).contains("must be a number"));
}

#[test]
fn events_update_jobs_while_a_modal_is_open() {
    let mut s = state();
    key(&mut s, ' ');
    key(&mut s, 'd');
    let mut changed = job(8, "new.bin", JobState::Active);
    changed.download_speed = 1;
    update(
        &mut s,
        Msg::Event(Event::JobsUpdated {
            jobs: vec![changed],
        }),
    );
    assert!(s.jobs.contains_key(&JobId(8)));
    update(
        &mut s,
        Msg::Event(Event::JobsRemoved {
            ids: vec![JobId(1)],
        }),
    );
    assert!(!s.jobs.contains_key(&JobId(1)));
    assert!(
        s.list.selected.is_empty(),
        "removed jobs leave the selection"
    );
    assert!(matches!(s.modal, Some(Modal::Confirm(_))));
}

#[test]
fn engine_and_message_events() {
    let mut s = state();
    update(
        &mut s,
        Msg::Event(Event::Engine {
            status: EngineStatus {
                engine: Engine::Aria2,
                available: false,
                version: None,
                features: vec![],
                reason: Some("aria2c not found in PATH".into()),
            },
        }),
    );
    assert_eq!(s.unavailable_engines().count(), 1);
    update(
        &mut s,
        Msg::Event(Event::Message {
            level: dman_proto::method::Level::Warn,
            text: "disk almost full".into(),
        }),
    );
    assert_eq!(s.status.last_error.as_deref(), Some("disk almost full"));
}

#[test]
fn mouse_click_and_wheel() {
    let mut s = state_with(true);
    let click = |row| {
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    // Rows: 0 header, 1 info, 2 table header, 3 "Active", 4 job 1, 5 job 2.
    update(&mut s, click(5));
    assert_eq!(s.list.cursor, Some(JobId(2)));
    update(&mut s, click(3));
    assert_eq!(
        s.list.cursor,
        Some(JobId(2)),
        "section rows are not selectable"
    );
    update(
        &mut s,
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }),
    );
    assert_eq!(s.list.cursor, Some(JobId(5)));
    let mut off = state_with(false);
    update(&mut off, click(5));
    assert_eq!(off.list.cursor, Some(JobId(1)), "mouse disabled");
}

#[test]
fn paste_in_list_opens_add_view() {
    let mut s = state();
    update(&mut s, Msg::Paste("magnet:?xt=urn:btih:abc".into()));
    assert!(matches!(s.view(), View::Add(a) if a.sources.text().starts_with("magnet:")));
    // The pasted text is a non-empty form: Esc asks before discarding.
    code(&mut s, KeyCode::Esc);
    assert!(matches!(s.modal, Some(Modal::Confirm(_))));
    key(&mut s, 'y');
    assert!(matches!(s.view(), View::Downloads));
}

#[test]
fn lag_resyncs_and_signals_quit() {
    let mut s = state();
    assert_eq!(update(&mut s, Msg::Lagged), vec![Cmd::Resync]);
    assert_eq!(update(&mut s, Msg::Terminate), vec![Cmd::Quit]);
}

#[test]
fn key_override_warnings_reach_the_status_line() {
    let table: toml::Table = toml::from_str("frobnicate = [\"f\"]").unwrap_or_default();
    let opts = crate::TuiOptions {
        keys: Some(table),
        ..crate::TuiOptions::default()
    };
    let s = opts.initial_state();
    assert!(
        s.status
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("frobnicate")
    );
}

// ---------------------------------------------------------------- snapshots

fn render(s: &mut State, w: u16, h: u16) -> String {
    update(s, Msg::Resize(w, h));
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    terminal
        .draw(|f| views::render(s, f))
        .expect("draw succeeds");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[test]
fn snapshot_downloads_80x24() {
    let mut s = state();
    key(&mut s, 'j');
    key(&mut s, ' ');
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_downloads_120x40() {
    let mut s = state();
    insta::assert_snapshot!(render(&mut s, 120, 40));
}

#[test]
fn snapshot_disconnected_80x24() {
    let mut s = state();
    disconnect(&mut s);
    key(&mut s, 'p');
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_failed_and_engine_unavailable_80x24() {
    let mut s = state();
    update(
        &mut s,
        Msg::Conn(ConnState::Failed {
            error: ClientError::IncompatibleVersion {
                daemon: 2,
                client: 1,
            },
        }),
    );
    update(
        &mut s,
        Msg::Event(Event::Engine {
            status: EngineStatus {
                engine: Engine::Aria2,
                available: false,
                version: None,
                features: vec![],
                reason: Some("aria2c not found in PATH".into()),
            },
        }),
    );
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_delete_confirmation_80x24() {
    let mut s = state();
    key(&mut s, ' ');
    key(&mut s, ' ');
    key(&mut s, 'D');
    key(&mut s, 'y');
    typed(&mut s, "DEL");
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_remove_confirmation_120x40() {
    let mut s = state();
    key(&mut s, 'd');
    insta::assert_snapshot!(render(&mut s, 120, 40));
}

#[test]
fn snapshot_palette_80x24() {
    let mut s = state();
    key(&mut s, ':');
    typed(&mut s, "pause");
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_filter_80x24() {
    let mut s = state();
    key(&mut s, '/');
    typed(&mut s, "iso");
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_help_120x40() {
    let mut s = state();
    key(&mut s, '?');
    insta::assert_snapshot!(render(&mut s, 120, 40));
}

#[test]
fn snapshot_detail_80x24() {
    let mut s = state();
    key(&mut s, 'G');
    code(&mut s, KeyCode::Enter);
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_empty_connecting_80x24() {
    let mut s = State::new(StateOptions::default());
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn offline_rows_are_greyed() {
    let mut s = state();
    disconnect(&mut s);
    update(&mut s, Msg::Resize(80, 24));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    terminal
        .draw(|f| views::render(&s, f))
        .expect("draw succeeds");
    let buf = terminal.backend().buffer();
    // Row 6 (two banner lines… here one) holds a job; its status cell must be dim.
    let dim = s.theme.dim.fg;
    let job_row = (0..24u16)
        .find(|y| {
            let line: String = (0..80u16).map(|x| buf[(x, *y)].symbol()).collect();
            line.contains("big-dataset")
        })
        .expect("job row rendered");
    let status_x = (0..80u16)
        .find(|x| buf[(*x, job_row)].symbol() == "a" && buf[(*x + 1, job_row)].symbol() == "c")
        .expect("status cell");
    assert_eq!(buf[(status_x, job_row)].fg, dim.expect("dim colour"));
}

// ------------------------------------------------------------------ add view

mod add_view {
    use super::*;
    use crate::views::add::browser::{BrowseEntry, LocalListing, read_local_dir};
    use crate::views::add::source::PathStatus;
    use crate::views::add::{Focus, Overlay};
    use dman_proto::method::{FsEntry, FsListParams, FsListing};
    use dman_proto::{AddOutcome, AddParams, AddSource};
    use std::path::PathBuf;

    const TORRENT: &[u8] = b"d4:infod5:filesld6:lengthi3e4:pathl1:a1:xeed6:lengthi4e4:pathl1:beee4:name3:dir12:piece lengthi16384e6:pieces40:AAAAAAAAAAAAAAAAAAAABBBBBBBBBBBBBBBBBBBBee";

    fn open() -> State {
        let mut s = state();
        key(&mut s, 'a');
        assert!(matches!(s.view(), View::Add(_)));
        s
    }

    fn ctrl(s: &mut State, c: char) -> Vec<Cmd> {
        press(s, KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_lines(s: &mut State, lines: &[&str]) {
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                code(s, KeyCode::Enter);
            }
            typed(s, l);
        }
    }

    fn add(s: &State) -> &crate::views::add::AddState {
        match s.view() {
            View::Add(a) => a,
            v => panic!("not the add view: {v:?}"),
        }
    }

    fn add_call(cmds: &[Cmd]) -> AddParams {
        match cmds {
            [Cmd::Api(ApiCall::Add(p))] => (**p).clone(),
            other => panic!("expected an add call, got {other:?}"),
        }
    }

    fn added(id: u64) -> AddOutcome {
        AddOutcome::Added {
            id: JobId(id),
            extra_ids: vec![],
        }
    }

    fn failed(msg: &str) -> AddOutcome {
        AddOutcome::Failed {
            error: dman_proto::RpcError::invalid(msg),
        }
    }

    #[test]
    fn typing_keeps_keys_as_text_and_esc_leaves() {
        let mut s = open();
        // `q`, `p`, `:` and `?` are text, not actions.
        typed(&mut s, "qp:?");
        assert_eq!(add(&s).sources.text(), "qp:?");
        code(&mut s, KeyCode::Esc);
        key(&mut s, 'y');
        assert!(matches!(s.view(), View::Downloads));
        // An empty form closes without asking.
        key(&mut s, 'a');
        code(&mut s, KeyCode::Esc);
        assert!(s.modal.is_none());
        assert!(matches!(s.view(), View::Downloads));
    }

    #[test]
    fn multi_line_paste_and_edit() {
        let mut s = open();
        update(
            &mut s,
            Msg::Paste("http://a/1\r\nhttp://a/2\nmagnet:?xt=urn:btih:abc".into()),
        );
        assert_eq!(add(&s).sources.lines().len(), 3);
        code(&mut s, KeyCode::Up);
        press(&mut s, KeyCode::End, KeyModifiers::NONE);
        typed(&mut s, "x");
        assert_eq!(add(&s).sources.lines()[1], "http://a/2x");
        press(&mut s, KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(add(&s).sources.cursor(), (1, 9));
    }

    #[test]
    fn empty_and_unsupported_sources_are_refused() {
        let mut s = open();
        assert!(ctrl(&mut s, 's').is_empty());
        assert!(status_text(&s).contains("Nothing to add"));
        typed(&mut s, "file:///x/y.iso");
        assert!(ctrl(&mut s, 's').is_empty());
        assert!(
            status_text(&s).contains("Cannot add"),
            "{}",
            status_text(&s)
        );
        assert!(add(&s).overlay.is_none());
    }

    #[test]
    fn preview_then_confirm_adds_and_returns_to_list() {
        let mut s = open();
        type_lines(
            &mut s,
            &["https://example.org/a.iso", "magnet:?xt=urn:btih:abc"],
        );
        assert!(ctrl(&mut s, 's').is_empty());
        let Some(Overlay::Preview(p)) = &add(&s).overlay else {
            panic!("no preview")
        };
        assert_eq!(p.items.len(), 2);
        // Esc closes the preview only.
        code(&mut s, KeyCode::Esc);
        assert!(add(&s).overlay.is_none());
        ctrl(&mut s, 's');
        let cmds = code(&mut s, KeyCode::Enter);
        let params = add_call(&cmds);
        assert_eq!(params.sources.len(), 2);
        assert!(!params.paused);
        assert_eq!(s.pending, 1);
        // A second submit while in flight is refused.
        update(
            &mut s,
            Msg::ActionResult {
                call: ApiCall::Add(Box::new(params)),
                result: Ok(ApiReply::Added(vec![added(10), added(11)])),
            },
        );
        assert!(matches!(s.view(), View::Downloads), "back to the list");
        assert_eq!(status_text(&s), "Added 2 downloads");
    }

    #[test]
    fn partial_failure_keeps_failed_sources() {
        let mut s = open();
        type_lines(&mut s, &["http://a/ok", "http://a/bad http://a/ok2"]);
        ctrl(&mut s, 's');
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(params.sources.len(), 3);
        update(
            &mut s,
            Msg::ActionResult {
                call: ApiCall::Add(Box::new(params)),
                result: Ok(ApiReply::Added(vec![added(1), failed("refused"), added(2)])),
            },
        );
        assert!(matches!(s.view(), View::Add(_)));
        assert_eq!(add(&s).sources.lines(), ["http://a/bad"]);
        assert_eq!(status_text(&s), "Added 2, 1 failed: refused");
        assert!(add(&s).submitting.is_none());
    }

    #[test]
    fn mirror_toggle_groups_uris() {
        let mut s = open();
        type_lines(
            &mut s,
            &["http://a/f", "https://b/f", "magnet:?xt=urn:btih:abc"],
        );
        ctrl(&mut s, 't');
        ctrl(&mut s, 's');
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(params.sources.len(), 2);
        assert!(matches!(&params.sources[0], AddSource::Uris { uris } if uris.len() == 2));
    }

    #[test]
    fn options_are_validated_and_sent() {
        let mut s = open();
        typed(&mut s, "http://a/f");
        // Tab to the options form: Mirror, then Directory.
        code(&mut s, KeyCode::Tab);
        assert_eq!(
            add(&s).focus,
            Focus::Field(crate::views::add::options::Field::Mirror)
        );
        code(&mut s, KeyCode::Down);
        typed(&mut s, "/data/x");
        code(&mut s, KeyCode::Down); // output name
        code(&mut s, KeyCode::Down); // paused
        key(&mut s, ' ');
        code(&mut s, KeyCode::Down); // down limit
        typed(&mut s, "fast");
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(
            status_text(&s).contains("Down limit"),
            "{}",
            status_text(&s)
        );
        assert!(add(&s).overlay.is_none());
        for _ in 0..4 {
            code(&mut s, KeyCode::Backspace);
        }
        typed(&mut s, "2MiB");
        // Enter in a single-line field previews.
        code(&mut s, KeyCode::Enter);
        assert!(matches!(add(&s).overlay, Some(Overlay::Preview(_))));
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert!(params.paused);
        assert_eq!(params.options.dir.as_deref(), Some("/data/x"));
        assert_eq!(params.options.max_download_limit, Some(2 << 20));
        // Shift-Tab goes back.
        let mut s = open();
        press(&mut s, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(
            add(&s).focus,
            Focus::Field(crate::views::add::options::Field::Engine)
        );
    }

    #[test]
    fn local_torrent_flow_reads_and_uploads() {
        let mut s = open();
        let path = "/home/me/dir.torrent";
        let cmds = update(&mut s, Msg::Paste(path.into()));
        assert_eq!(cmds, vec![Cmd::CheckPaths(vec![path.into()])]);
        update(
            &mut s,
            Msg::PathsChecked(vec![(path.into(), PathStatus::Ready { size: 100 })]),
        );
        let cmds = ctrl(&mut s, 's');
        assert_eq!(cmds, vec![Cmd::ReadFiles(vec![path.into()])]);
        let mut files = crate::views::add::preview::Loaded::new();
        files.insert(path.into(), Ok(TORRENT.to_vec()));
        update(&mut s, Msg::FilesRead(files));
        let Some(Overlay::Preview(p)) = &add(&s).overlay else {
            panic!("no preview")
        };
        assert_eq!(p.items[0].name, "dir");
        assert_eq!(p.items[0].size, Some(7));
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert!(
            matches!(&params.sources[0], AddSource::Torrent { name, .. } if name == "dir.torrent")
        );
    }

    #[test]
    fn unreadable_local_file_blocks_confirm() {
        let mut s = open();
        update(&mut s, Msg::Paste("/nope/x.torrent".into()));
        ctrl(&mut s, 's');
        let mut files = crate::views::add::preview::Loaded::new();
        files.insert("/nope/x.torrent".into(), Err("not found".into()));
        update(&mut s, Msg::FilesRead(files));
        assert!(code(&mut s, KeyCode::Enter).is_empty(), "blocked");
        assert!(matches!(add(&s).overlay, Some(Overlay::Preview(_))));
    }

    #[test]
    fn file_browser_adds_paths() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("one.torrent"), b"x").unwrap();
        std::fs::write(d.path().join("two.metalink"), b"x").unwrap();
        let mut s = open();
        let cmds = ctrl(&mut s, 'o');
        assert_eq!(cmds, vec![Cmd::ReadDir(None)]);
        update(&mut s, Msg::LocalDir(read_local_dir(Some(d.path()))));
        // Typing is not text while the browser is open; Space marks and moves.
        key(&mut s, ' ');
        key(&mut s, ' ');
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(cmds.len(), 1, "check for the new paths: {cmds:?}");
        assert!(add(&s).overlay.is_none());
        let text = add(&s).sources.text();
        assert!(
            text.contains("one.torrent") && text.contains("two.metalink"),
            "{text}"
        );
        assert_eq!(add(&s).sources.lines().len(), 3, "trailing empty line");
    }

    #[test]
    fn clipboard_success_and_failure() {
        let mut s = open();
        assert_eq!(ctrl(&mut s, 'v'), vec![Cmd::ReadClipboard]);
        update(&mut s, Msg::Clipboard(Ok("http://a/1\nhttp://a/2".into())));
        assert_eq!(add(&s).sources.lines().len(), 2);
        update(&mut s, Msg::Clipboard(Err("no display".into())));
        let t = status_text(&s);
        assert!(t.contains("no display") && t.contains("terminal"), "{t}");
        assert!(s.status.last_error.is_some());
        update(&mut s, Msg::Clipboard(Ok("  ".into())));
        assert!(status_text(&s).contains("empty"));
    }

    #[test]
    fn directory_picker_uses_the_daemon() {
        let mut s = open();
        let cmds = ctrl(&mut s, 'f');
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::FsList(FsListParams {
                path: None,
                dirs_only: true
            }))]
        );
        let listing = FsListing {
            path: "/srv/dl".into(),
            parent: Some("/srv".into()),
            entries: vec![FsEntry {
                name: "tv".into(),
                is_dir: true,
                size: None,
            }],
            truncated: false,
        };
        let call = ApiCall::FsList(FsListParams::default());
        update(
            &mut s,
            Msg::ActionResult {
                call: call.clone(),
                result: Ok(ApiReply::Listing(listing)),
            },
        );
        // Enter descends through the daemon.
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::FsList(FsListParams {
                path: Some("/srv/dl/tv".into()),
                dirs_only: true
            }))]
        );
        update(
            &mut s,
            Msg::ActionResult {
                call,
                result: Err("fs.list: not found".into()),
            },
        );
        key(&mut s, '~');
        update(
            &mut s,
            Msg::ActionResult {
                call: ApiCall::FsList(FsListParams::default()),
                result: Ok(ApiReply::Listing(FsListing {
                    path: "/srv/dl".into(),
                    parent: None,
                    entries: vec![],
                    truncated: false,
                })),
            },
        );
        key(&mut s, ' ');
        assert!(add(&s).overlay.is_none());
        assert_eq!(
            add(&s)
                .form
                .editor(crate::views::add::options::Field::Dir)
                .map(|e| e.text()),
            Some("/srv/dl".to_owned())
        );
        // Offline: refused with a message.
        disconnect(&mut s);
        assert!(ctrl(&mut s, 'f').is_empty());
        assert!(status_text(&s).contains("disabled"));
        assert!(add(&s).overlay.is_none());
    }

    #[test]
    fn palette_actions_open_the_view_from_the_list() {
        let mut s = state();
        key(&mut s, ':');
        typed(&mut s, "browse local");
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(cmds, vec![Cmd::ReadDir(None)]);
        assert!(matches!(add(&s).overlay, Some(Overlay::Files(_))));
    }

    #[test]
    fn offline_confirm_keeps_the_preview() {
        let mut s = open();
        typed(&mut s, "http://a/f");
        ctrl(&mut s, 's');
        disconnect(&mut s);
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(matches!(add(&s).overlay, Some(Overlay::Preview(_))));
        assert!(add(&s).submitting.is_none());
    }

    // ------------------------------------------------------- discard confirm

    #[test]
    fn esc_with_a_filled_form_asks_before_discarding() {
        let mut s = open();
        typed(&mut s, "http://a/f");
        code(&mut s, KeyCode::Esc);
        let Some(Modal::Confirm(c)) = &s.modal else {
            panic!("expected a confirmation, got {:?}", s.modal);
        };
        assert!(c.title.contains("Discard this form?"), "{}", c.title);
        assert!(matches!(s.view(), View::Add(_)));
        // `n` (and Esc) keep editing; the text is still there.
        key(&mut s, 'n');
        assert!(s.modal.is_none());
        assert_eq!(add(&s).sources.text(), "http://a/f");
        code(&mut s, KeyCode::Esc);
        code(&mut s, KeyCode::Esc);
        assert!(s.modal.is_none() && matches!(s.view(), View::Add(_)));
        // `y` discards.
        code(&mut s, KeyCode::Esc);
        assert!(key(&mut s, 'y').is_empty());
        assert!(s.modal.is_none());
        assert!(matches!(s.view(), View::Downloads));
    }

    #[test]
    fn a_touched_option_counts_as_a_filled_form() {
        let mut s = open();
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Down); // from the mirror checkbox to the directory
        typed(&mut s, "/data");
        code(&mut s, KeyCode::Esc);
        assert!(matches!(s.modal, Some(Modal::Confirm(_))));
        key(&mut s, 'y');
        assert!(matches!(s.view(), View::Downloads));
    }

    // ----------------------------------------------------- magnet file list

    const MAGNET: &str = "magnet:?xt=urn:btih:abc&dn=Some+Movie";

    fn preview_of(s: &State) -> &crate::views::add::preview::Preview {
        match &add(s).overlay {
            Some(Overlay::Preview(p)) => p,
            other => panic!("no preview open: {other:?}"),
        }
    }

    fn magnet_preview_reply() -> ApiReply {
        let files = [
            ("Some.Movie/movie.mkv", 700u64),
            ("Some.Movie/sub.srt", 1),
            ("Some.Movie/extras.zip", 90),
        ]
        .iter()
        .zip(1u32..)
        .map(|((path, mib), index)| dman_proto::FileInfo {
            index,
            path: (*path).into(),
            length: mib * (1 << 20),
            completed: 0,
            selected: true,
        })
        .collect();
        ApiReply::MagnetPreview(Box::new(dman_proto::method::MagnetPreview {
            info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
            name: "Some.Movie".into(),
            total_bytes: 791 * (1 << 20),
            files,
            torrent_base64: "ZDQ6aW5mb2Vl".into(),
        }))
    }

    fn deliver_magnet(s: &mut State, result: Result<ApiReply, String>) {
        update(
            s,
            Msg::ActionResult {
                call: ApiCall::PreviewMagnet { uri: MAGNET.into() },
                result,
            },
        );
    }

    /// A magnet in the preview, fetch requested and answered.
    fn fetched() -> State {
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        deliver_magnet(&mut s, Ok(magnet_preview_reply()));
        s
    }

    #[test]
    fn fetch_file_list_select_and_add_the_torrent() {
        use crate::views::add::preview::Fetch;
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        assert_eq!(preview_of(&s).fetchable_uri(), Some(MAGNET));
        let cmds = key(&mut s, 'f');
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::PreviewMagnet { uri: MAGNET.into() })]
        );
        assert!(preview_of(&s).fetching());
        deliver_magnet(&mut s, Ok(magnet_preview_reply()));
        let Fetch::Done(m) = &preview_of(&s).fetch else {
            panic!("not fetched: {:?}", preview_of(&s).fetch);
        };
        assert_eq!(m.chosen(), vec![1, 2, 3], "everything starts selected");
        // Space unchecks the file under the cursor and moves on; uncheck file 2.
        key(&mut s, 'j');
        key(&mut s, ' ');
        let Fetch::Done(m) = &preview_of(&s).fetch else {
            unreachable!()
        };
        assert_eq!(m.chosen(), vec![1, 3]);
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(
            params.sources,
            vec![AddSource::Torrent {
                name: "Some.Movie.torrent".into(),
                data_base64: "ZDQ6aW5mb2Vl".into(),
            }]
        );
        assert_eq!(params.options.select_files, Some(vec![1, 3]));
    }

    #[test]
    fn all_toggles_every_file_and_an_empty_choice_blocks_adding() {
        let mut s = fetched();
        key(&mut s, 'a'); // all are selected: this clears them
        assert_eq!(preview_of(&s).blocker(), Some("select at least one file"));
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        key(&mut s, 'a');
        assert!(preview_of(&s).blocker().is_none());
        key(&mut s, 'G');
        key(&mut s, ' ');
        key(&mut s, 'a');
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(params.options.select_files, Some(vec![1, 2, 3]));
    }

    #[test]
    fn without_fetching_the_magnet_is_added_as_before() {
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(
            params.sources,
            vec![AddSource::Uris {
                uris: vec![MAGNET.into()]
            }]
        );
        assert_eq!(params.options.select_files, None);
    }

    #[test]
    fn esc_cancels_the_fetch_and_drops_its_answer() {
        use crate::views::add::preview::Fetch;
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        // While fetching, Enter does not add and other keys are ignored.
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(preview_of(&s).fetching());
        // Esc cancels the fetch only; the preview stays.
        assert!(code(&mut s, KeyCode::Esc).is_empty());
        assert_eq!(preview_of(&s).fetch, Fetch::Idle);
        // The late answer is dropped.
        deliver_magnet(&mut s, Ok(magnet_preview_reply()));
        assert_eq!(preview_of(&s).fetch, Fetch::Idle);
        // The magnet can still be added as it is.
        let params = add_call(&code(&mut s, KeyCode::Enter));
        assert!(matches!(&params.sources[..], [AddSource::Uris { .. }]));
    }

    #[test]
    fn fetch_failure_can_be_retried() {
        use crate::views::add::preview::Fetch;
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        deliver_magnet(&mut s, Err("timed out after 120 s".into()));
        assert!(
            matches!(&preview_of(&s).fetch, Fetch::Failed(e) if e.contains("timed out")),
            "{:?}",
            preview_of(&s).fetch
        );
        assert_eq!(key(&mut s, 'f').len(), 1);
        assert!(preview_of(&s).fetching());
    }

    #[test]
    fn fetch_is_refused_offline_and_fails_on_disconnect() {
        use crate::views::add::preview::Fetch;
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        disconnect(&mut s);
        assert!(key(&mut s, 'f').is_empty());
        assert_eq!(preview_of(&s).fetch, Fetch::Idle);
        assert!(status_text(&s).contains("disabled"), "{}", status_text(&s));
        // A fetch running when the connection drops can never be answered.
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        disconnect(&mut s);
        assert!(matches!(preview_of(&s).fetch, Fetch::Failed(_)));
    }

    #[test]
    fn only_a_single_magnet_can_be_fetched() {
        let mut s = open();
        typed(&mut s, "http://a/f");
        ctrl(&mut s, 's');
        assert!(key(&mut s, 'f').is_empty());
        let mut s = open();
        type_lines(&mut s, &[MAGNET, "magnet:?xt=urn:btih:def"]);
        ctrl(&mut s, 's');
        assert_eq!(preview_of(&s).fetchable_uri(), None);
        assert!(key(&mut s, 'f').is_empty());
        assert!(!preview_of(&s).fetching());
    }

    #[test]
    fn the_spinner_keeps_redrawing_only_while_fetching() {
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        s.dirty = false;
        update(&mut s, Msg::Tick);
        assert!(!s.dirty);
        key(&mut s, 'f');
        s.dirty = false;
        update(&mut s, Msg::Tick);
        assert!(s.dirty);
        assert!(render(&mut s, 80, 24).contains("fetching metadata"));
    }

    #[test]
    fn preselected_files_seed_the_checkboxes() {
        let mut s = open();
        typed(&mut s, MAGNET);
        code(&mut s, KeyCode::Tab);
        // Walk to the select-files field.
        while add(&s).focus != Focus::Field(crate::views::add::options::Field::SelectFiles) {
            code(&mut s, KeyCode::Tab);
        }
        typed(&mut s, "2");
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        deliver_magnet(&mut s, Ok(magnet_preview_reply()));
        let crate::views::add::preview::Fetch::Done(m) = &preview_of(&s).fetch else {
            panic!("not fetched");
        };
        assert_eq!(m.chosen(), vec![2]);
    }

    #[test]
    fn snapshot_magnet_file_list_80x24() {
        let mut s = fetched();
        key(&mut s, 'j');
        key(&mut s, ' ');
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_magnet_fetching_80x24() {
        let mut s = open();
        typed(&mut s, MAGNET);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    // ------------------------------------------------------------ snapshots

    fn mixed() -> State {
        let mut s = open();
        type_lines(
            &mut s,
            &[
                "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=ubuntu",
                "https://example.org/files/big-dataset-2026.tar.zst",
                "ftp://mirror.example.net/pub/a.iso  ftp://mirror2.example.net/pub/a.iso",
                "/home/me/Downloads/ubuntu-24.04.torrent",
                "/home/me/Downloads/gone.metalink",
                "file:///tmp/x.iso",
            ],
        );
        update(
            &mut s,
            Msg::PathsChecked(vec![
                (
                    "/home/me/Downloads/ubuntu-24.04.torrent".into(),
                    PathStatus::Ready { size: 46_000 },
                ),
                (
                    "/home/me/Downloads/gone.metalink".into(),
                    PathStatus::Missing,
                ),
            ]),
        );
        s
    }

    #[test]
    fn snapshot_add_mixed_80x24() {
        let mut s = mixed();
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_add_mixed_120x40() {
        let mut s = mixed();
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Down);
        typed(&mut s, "/data/dl");
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Down);
        typed(&mut s, "fast");
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }

    fn browser_state() -> State {
        let mut s = open();
        ctrl(&mut s, 'o');
        let entry = |n: &str, d: bool, sz: Option<u64>| BrowseEntry {
            name: n.into(),
            is_dir: d,
            size: sz,
        };
        update(
            &mut s,
            Msg::LocalDir(Ok(LocalListing {
                dir: PathBuf::from("/home/me/torrents"),
                entries: vec![
                    entry("archive", true, None),
                    entry("linux", true, None),
                    entry("debian-13.2.0-netinst.iso.torrent", false, Some(41_000)),
                    entry("ubuntu-24.04.torrent", false, Some(46_080)),
                    entry("pack.meta4", false, Some(1_200)),
                ],
                truncated: false,
            })),
        );
        key(&mut s, 'j');
        key(&mut s, 'j');
        key(&mut s, ' ');
        key(&mut s, ' ');
        s
    }

    #[test]
    fn snapshot_file_browser_80x24() {
        let mut s = browser_state();
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_file_browser_120x40() {
        let mut s = browser_state();
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }

    fn preview_state() -> State {
        let mut s = open();
        let path = "/home/me/dir.torrent";
        type_lines(
            &mut s,
            &[
                "https://example.org/a.iso",
                "magnet:?xt=urn:btih:abc&dn=Some+Movie",
                path,
            ],
        );
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Down);
        typed(&mut s, "/data/dl");
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Down);
        key(&mut s, ' ');
        ctrl(&mut s, 's');
        let mut files = crate::views::add::preview::Loaded::new();
        files.insert(path.into(), Ok(TORRENT.to_vec()));
        update(&mut s, Msg::FilesRead(files));
        s
    }

    #[test]
    fn snapshot_preview_80x24() {
        let mut s = preview_state();
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_preview_120x40() {
        let mut s = preview_state();
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }
}

// ------------------------------------------------------------------- detail

use dman_proto::JobOptions;

/// Open the detail of `id` and deliver a fixture reply.
fn open_detail(s: &mut State, id: u64) -> Vec<Cmd> {
    s.list.cursor = Some(JobId(id));
    let cmds = code(s, KeyCode::Enter);
    let reply = sample_detail(&s.jobs[&JobId(id)]);
    update(
        s,
        Msg::ActionResult {
            call: ApiCall::Detail(JobId(id)),
            result: Ok(ApiReply::Detail(Box::new(reply))),
        },
    );
    cmds
}

fn detail_state(s: &State) -> &views::detail::DetailState {
    match s.view() {
        View::Detail(d) => d,
        other => panic!("not the detail view: {other:?}"),
    }
}

fn ticks(s: &mut State, n: usize) -> Vec<Cmd> {
    (0..n).flat_map(|_| update(s, Msg::Tick)).collect()
}

fn set_options(id: u64, options: JobOptions) -> Cmd {
    Cmd::Api(ApiCall::SetOptions {
        id: JobId(id),
        options: Box::new(options),
    })
}

#[test]
fn opening_detail_fetches_it_once() {
    let mut s = state();
    let cmds = open_detail(&mut s, 1);
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::Detail(JobId(1)))]);
    assert!(detail_state(&s).detail.is_some());
    assert!(!detail_state(&s).inflight);
}

#[test]
fn tabs_switch_with_tab_and_digits() {
    use views::detail::Tab;
    let mut s = state();
    open_detail(&mut s, 1);
    code(&mut s, KeyCode::Tab);
    assert_eq!(detail_state(&s).tab, Tab::Peers);
    press(&mut s, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(detail_state(&s).tab, Tab::Files);
    press(&mut s, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(detail_state(&s).tab, Tab::Options);
    for (c, tab) in ['1', '2', '3', '4', '5', '6'].into_iter().zip(Tab::ALL) {
        key(&mut s, c);
        assert_eq!(detail_state(&s).tab, tab);
    }
    key(&mut s, 'h');
    assert!(matches!(s.view(), View::Downloads));
}

#[test]
fn refresh_is_requested_on_tick_only_while_open() {
    let mut s = state();
    assert!(ticks(&mut s, 8).is_empty(), "no refresh on the list");
    open_detail(&mut s, 1);
    assert!(ticks(&mut s, 3).is_empty());
    assert_eq!(ticks(&mut s, 1), vec![Cmd::Api(ApiCall::Detail(JobId(1)))]);
    // A call in flight is not doubled.
    assert!(ticks(&mut s, 8).is_empty());
    update(
        &mut s,
        Msg::ActionResult {
            call: ApiCall::Detail(JobId(1)),
            result: Err("timeout".into()),
        },
    );
    assert!(detail_state(&s).error.is_some());
    assert!(detail_state(&s).detail.is_some(), "stale data is kept");
    assert_eq!(ticks(&mut s, 4).len(), 1);
    code(&mut s, KeyCode::Esc);
    assert!(ticks(&mut s, 8).is_empty());
}

#[test]
fn no_refresh_while_disconnected() {
    let mut s = state();
    open_detail(&mut s, 1);
    disconnect(&mut s);
    assert!(ticks(&mut s, 8).is_empty());
}

#[test]
fn file_toggle_refused_while_active() {
    let mut s = state();
    open_detail(&mut s, 1);
    assert!(key(&mut s, ' ').is_empty());
    let text = status_text(&s);
    assert!(text.contains("pause the download first"), "{text}");
    assert!(key(&mut s, 'a').is_empty());
    assert!(detail_state(&s).detail.as_ref().unwrap().files[0].selected);
}

#[test]
fn file_toggle_allowed_while_paused() {
    let mut s = state();
    open_detail(&mut s, 5);
    // Deselect file 1: files 2 and 4 stay selected.
    let cmds = key(&mut s, ' ');
    assert_eq!(
        cmds,
        vec![set_options(
            5,
            JobOptions {
                select_files: Some(vec![2, 4]),
                ..JobOptions::default()
            }
        )]
    );
    // Select-all adds file 1 and 3 back.
    let cmds = key(&mut s, 'a');
    assert_eq!(
        cmds,
        vec![set_options(
            5,
            JobOptions {
                select_files: Some(vec![1, 2, 3, 4]),
                ..JobOptions::default()
            }
        )]
    );
    // All selected: nothing more to do.
    assert!(key(&mut s, 'a').is_empty());
}

#[test]
fn last_selected_file_cannot_be_deselected() {
    let mut s = state();
    open_detail(&mut s, 5);
    if let View::Detail(d) = s.views.last_mut().unwrap() {
        for f in &mut d.detail.as_mut().unwrap().files {
            f.selected = f.index == 1;
        }
    }
    assert!(key(&mut s, ' ').is_empty());
    assert!(status_text(&s).contains("At least one file"));
    assert!(detail_state(&s).detail.as_ref().unwrap().files[0].selected);
}

#[test]
fn file_toggle_needs_a_connection() {
    let mut s = state();
    open_detail(&mut s, 5);
    disconnect(&mut s);
    assert!(key(&mut s, ' ').is_empty());
    assert!(status_text(&s).contains("disabled"));
}

#[test]
fn option_edit_sends_set_options() {
    let mut s = state();
    open_detail(&mut s, 1);
    key(&mut s, '6');
    // Row 2 is max_download_limit (prefilled "2M").
    key(&mut s, 'j');
    key(&mut s, 'j');
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    let Some(Modal::Prompt(p)) = &s.modal else {
        panic!("no prompt: {:?}", s.modal);
    };
    assert_eq!(p.input.text(), "2M");
    for _ in 0..2 {
        code(&mut s, KeyCode::Backspace);
    }
    typed(&mut s, "500K");
    let cmds = code(&mut s, KeyCode::Enter);
    assert_eq!(
        cmds,
        vec![set_options(
            1,
            JobOptions {
                max_download_limit: Some(500 * 1024),
                ..JobOptions::default()
            }
        )]
    );
    assert!(s.modal.is_none());
}

#[test]
fn option_edit_rejects_bad_input_and_read_only_rows() {
    let mut s = state();
    open_detail(&mut s, 1);
    key(&mut s, '6');
    key(&mut s, 'j');
    key(&mut s, 'j');
    code(&mut s, KeyCode::Enter);
    typed(&mut s, "x");
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    assert!(status_text(&s).contains("not a size"));
    // select_files (row 4) is read-only.
    key(&mut s, 'j');
    key(&mut s, 'j');
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    assert!(s.modal.is_none());
    assert!(status_text(&s).contains("Files tab"));
}

#[test]
fn set_options_result_refetches_and_shows_refusals() {
    let mut s = state();
    open_detail(&mut s, 1);
    let call = ApiCall::SetOptions {
        id: JobId(1),
        options: Box::new(JobOptions::default()),
    };
    let cmds = update(
        &mut s,
        Msg::ActionResult {
            call,
            result: Err(
                "`dir` cannot be changed while the download runs; pause the download first".into(),
            ),
        },
    );
    assert!(status_text(&s).contains("pause the download first"));
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::Detail(JobId(1)))]);
}

#[test]
fn pause_and_remove_target_the_detail_job() {
    let mut s = state();
    open_detail(&mut s, 2);
    assert_eq!(
        key(&mut s, 'p'),
        vec![Cmd::Api(ApiCall::Pause(vec![JobId(2)]))]
    );
    assert_eq!(
        key(&mut s, 'r'),
        vec![Cmd::Api(ApiCall::Resume(vec![JobId(2)]))]
    );
    key(&mut s, 'd');
    assert!(matches!(s.modal, Some(Modal::Confirm(_))));
}

#[test]
fn job_removed_while_open_returns_to_the_list() {
    let mut s = state();
    open_detail(&mut s, 1);
    update(
        &mut s,
        Msg::Event(Event::JobsRemoved {
            ids: vec![JobId(1)],
        }),
    );
    assert!(matches!(s.view(), View::Downloads));
    assert!(status_text(&s).contains("#1 was removed"));
    // Other removals leave the view alone.
    open_detail(&mut s, 2);
    update(
        &mut s,
        Msg::Event(Event::JobsRemoved {
            ids: vec![JobId(3)],
        }),
    );
    assert!(matches!(s.view(), View::Detail(_)));
}

#[test]
fn live_job_updates_reach_the_detail_header() {
    let mut s = state();
    open_detail(&mut s, 1);
    let mut j = s.jobs[&JobId(1)].clone();
    j.download_speed = 7 * (1 << 20);
    update(&mut s, Msg::Event(Event::JobsUpdated { jobs: vec![j] }));
    assert!(render(&mut s, 80, 24).contains("▼ 7.0 MiB/s"));
}

#[test]
fn peers_are_sorted_by_download_speed() {
    let mut s = state();
    open_detail(&mut s, 1);
    let peers = &detail_state(&s).detail.as_ref().unwrap().peers;
    let speeds: Vec<u64> = peers.iter().map(|p| p.download_speed).collect();
    assert!(speeds.windows(2).all(|w| w[0] >= w[1]), "{speeds:?}");
}

fn detail_snapshot(tab: char, id: u64, w: u16, h: u16) -> String {
    let mut s = state();
    open_detail(&mut s, id);
    key(&mut s, tab);
    render(&mut s, w, h)
}

#[test]
fn snapshot_detail_files_80x24() {
    insta::assert_snapshot!(detail_snapshot('1', 5, 80, 24));
}

#[test]
fn snapshot_detail_files_120x40() {
    insta::assert_snapshot!(detail_snapshot('1', 1, 120, 40));
}

#[test]
fn snapshot_detail_peers_80x24() {
    insta::assert_snapshot!(detail_snapshot('2', 1, 80, 24));
}

#[test]
fn snapshot_detail_peers_120x40() {
    insta::assert_snapshot!(detail_snapshot('2', 1, 120, 40));
}

#[test]
fn snapshot_detail_trackers_80x24() {
    insta::assert_snapshot!(detail_snapshot('3', 1, 80, 24));
}

#[test]
fn snapshot_detail_pieces_80x24() {
    insta::assert_snapshot!(detail_snapshot('4', 1, 80, 24));
}

#[test]
fn snapshot_detail_pieces_120x40() {
    insta::assert_snapshot!(detail_snapshot('4', 1, 120, 40));
}

#[test]
fn snapshot_detail_speed_80x24() {
    insta::assert_snapshot!(detail_snapshot('5', 1, 80, 24));
}

#[test]
fn snapshot_detail_speed_120x40() {
    insta::assert_snapshot!(detail_snapshot('5', 1, 120, 40));
}

#[test]
fn snapshot_detail_options_80x24() {
    insta::assert_snapshot!(detail_snapshot('6', 1, 80, 24));
}

#[test]
fn snapshot_detail_options_120x40() {
    insta::assert_snapshot!(detail_snapshot('6', 1, 120, 40));
}

#[test]
fn snapshot_detail_disconnected_80x24() {
    let mut s = state();
    open_detail(&mut s, 1);
    disconnect(&mut s);
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

// ------------------------------------------------------------------- rename

use crate::widgets::prompt::PromptKind;

fn prompt(s: &State) -> &crate::widgets::prompt::PromptState {
    match &s.modal {
        Some(Modal::Prompt(p)) => p,
        other => panic!("no prompt open: {other:?}"),
    }
}

fn select_job(s: &mut State, id: u64) {
    s.list.cursor = Some(JobId(id));
}

fn rename_call(id: u64, name: &str, file_index: Option<u32>) -> Cmd {
    Cmd::Api(ApiCall::Rename {
        id: JobId(id),
        name: name.into(),
        file_index,
    })
}

fn replace_prompt_text(s: &mut State, text: &str) {
    press(s, KeyCode::Char('u'), KeyModifiers::CONTROL);
    typed(s, text);
}

#[test]
fn rename_is_offered_for_waiting_and_paused_jobs() {
    for (id, name) in [
        (4, "Big Buck Bunny (1080p).mp4"),
        (5, "archlinux-2026.11.01-x86_64.iso"),
    ] {
        let mut s = state();
        select_job(&mut s, id);
        assert!(key(&mut s, 'n').is_empty());
        let p = prompt(&s);
        assert_eq!(
            p.kind,
            PromptKind::Rename {
                id: JobId(id),
                file_index: None
            }
        );
        assert_eq!(p.input.text(), name, "pre-filled with the current name");
        assert_eq!(p.title, "Rename");
        replace_prompt_text(&mut s, "new.iso");
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(cmds, vec![rename_call(id, "new.iso", None)]);
        assert!(s.modal.is_none());
        assert!(status_text(&s).contains("Renaming"));
    }
}

#[test]
fn rename_of_a_completed_job_says_it_moves_the_file() {
    let mut s = state();
    select_job(&mut s, 6);
    key(&mut s, 'n');
    let p = prompt(&s);
    assert!(p.title.contains("move on disk"), "{}", p.title);
    assert!(p.label.contains("moved on disk"), "{}", p.label);
    assert_eq!(p.input.text(), "report-q3.pdf");
}

#[test]
fn rename_is_refused_for_active_transfers_with_the_reason() {
    for id in [1, 2, 3] {
        let mut s = state();
        select_job(&mut s, id);
        assert!(key(&mut s, 'n').is_empty());
        assert!(s.modal.is_none(), "job {id}");
        assert_eq!(
            status_text(&s),
            "cannot rename an active transfer — pause it first"
        );
    }
    // Failed jobs are not renamed either.
    let mut s = state();
    select_job(&mut s, 7);
    key(&mut s, 'n');
    assert!(s.modal.is_none());
    assert!(status_text(&s).contains("only waiting, paused or completed"));
}

#[test]
fn rename_from_the_palette_and_offline() {
    let mut s = state();
    select_job(&mut s, 5);
    crate::app::apply_action(&mut s, Action::Rename);
    assert!(matches!(s.modal, Some(Modal::Prompt(_))));
    let mut s = state();
    select_job(&mut s, 5);
    disconnect(&mut s);
    key(&mut s, 'n');
    assert!(s.modal.is_none());
    assert!(status_text(&s).contains("disabled"), "{}", status_text(&s));
}

#[test]
fn rename_rejects_empty_names_and_paths() {
    let mut s = state();
    select_job(&mut s, 5);
    key(&mut s, 'n');
    replace_prompt_text(&mut s, "");
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    assert!(status_text(&s).contains("empty"), "{}", status_text(&s));
    key(&mut s, 'n');
    replace_prompt_text(&mut s, "a/b");
    assert!(code(&mut s, KeyCode::Enter).is_empty());
    assert!(
        status_text(&s).contains("not a path"),
        "{}",
        status_text(&s)
    );
    // Esc cancels without a call.
    key(&mut s, 'n');
    assert!(code(&mut s, KeyCode::Esc).is_empty());
    assert!(s.modal.is_none());
}

#[test]
fn rename_in_detail_uses_the_file_under_the_cursor() {
    let mut s = state();
    open_detail(&mut s, 5); // paused, four files
    assert_eq!(detail_state(&s).tab, views::detail::Tab::Files);
    key(&mut s, 'j');
    key(&mut s, 'j');
    key(&mut s, 'n');
    let p = prompt(&s);
    assert_eq!(
        p.kind,
        PromptKind::Rename {
            id: JobId(5),
            file_index: Some(3)
        }
    );
    assert_eq!(
        p.input.text(),
        "part-3.bin",
        "the file name, not its folder"
    );
    replace_prompt_text(&mut s, "renamed.bin");
    let cmds = code(&mut s, KeyCode::Enter);
    assert_eq!(cmds, vec![rename_call(5, "renamed.bin", Some(3))]);
}

#[test]
fn rename_in_detail_off_the_files_tab() {
    // Several files, not complete: the Files tab is needed.
    let mut s = state();
    open_detail(&mut s, 5);
    key(&mut s, '2');
    key(&mut s, 'n');
    assert!(s.modal.is_none());
    assert!(status_text(&s).contains("Files tab"), "{}", status_text(&s));
    // Completed: the whole output is renamed (no file index).
    let mut s = state();
    open_detail(&mut s, 6);
    key(&mut s, '5');
    key(&mut s, 'n');
    assert_eq!(
        prompt(&s).kind,
        PromptKind::Rename {
            id: JobId(6),
            file_index: None
        }
    );
}

#[test]
fn rename_in_detail_is_refused_while_active() {
    let mut s = state();
    open_detail(&mut s, 1);
    key(&mut s, 'n');
    assert!(s.modal.is_none());
    assert_eq!(
        status_text(&s),
        "cannot rename an active transfer — pause it first"
    );
}

#[test]
fn rename_result_reaches_the_status_line_and_refreshes_the_detail() {
    let mut s = state();
    open_detail(&mut s, 5);
    let call = ApiCall::Rename {
        id: JobId(5),
        name: "x.bin".into(),
        file_index: Some(1),
    };
    let cmds = update(
        &mut s,
        Msg::ActionResult {
            call: call.clone(),
            result: Ok(ApiReply::Done),
        },
    );
    assert_eq!(status_text(&s), "Renamed #5 to 'x.bin'");
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::Detail(JobId(5)))]);
    update(
        &mut s,
        Msg::ActionResult {
            call,
            result: Err("cannot rename an active transfer — pause it first".into()),
        },
    );
    assert!(status_text(&s).starts_with("Rename failed: cannot rename"));
}

#[test]
fn snapshot_rename_prompt_80x24() {
    let mut s = state();
    select_job(&mut s, 5);
    key(&mut s, 'n');
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

#[test]
fn snapshot_rename_complete_prompt_80x24() {
    let mut s = state();
    select_job(&mut s, 6);
    key(&mut s, 'n');
    insta::assert_snapshot!(render(&mut s, 80, 24));
}

// -------------------------------------------------------------------- graph

fn history_calls(cmds: &[Cmd]) -> usize {
    cmds.iter()
        .filter(|c| matches!(c, Cmd::Api(ApiCall::StatsHistory)))
        .count()
}

fn history_reply() -> Result<ApiReply, String> {
    let h = (0..90i64)
        .map(|i| dman_proto::SpeedSample {
            t: 1_700_000_000_000 + i * 1000,
            down: (6 + 8 * u64::try_from((i * 7) % 11).unwrap_or(0) / 10) * (1 << 20),
            up: (200 + 40 * u64::try_from(i % 9).unwrap_or(0)) * 1024,
        })
        .collect();
    Ok(ApiReply::History(h))
}

fn deliver_history(s: &mut State) {
    update(
        s,
        Msg::ActionResult {
            call: ApiCall::StatsHistory,
            result: history_reply(),
        },
    );
}

#[test]
fn graph_is_hidden_by_default_and_toggles_with_w() {
    let mut s = state();
    assert!(!s.graph.visible);
    assert_eq!(s.layout().graph.height, 0);
    let cmds = key(&mut s, 'w');
    assert!(s.graph.visible);
    assert_eq!(cmds, vec![Cmd::Api(ApiCall::StatsHistory)]);
    assert!(s.layout().graph.height > 0);
    assert!(key(&mut s, 'w').is_empty());
    assert!(!s.graph.visible);
    assert_eq!(s.layout().graph.height, 0);
}

#[test]
fn graph_refreshes_every_two_seconds_only_while_visible() {
    let mut s = state(); // tick = 250 ms
    assert_eq!(history_calls(&ticks(&mut s, 40)), 0, "hidden: no polling");
    key(&mut s, 'w');
    deliver_history(&mut s);
    assert_eq!(history_calls(&ticks(&mut s, 7)), 0);
    assert_eq!(history_calls(&ticks(&mut s, 1)), 1, "after 2 s");
    // One request at a time.
    assert_eq!(history_calls(&ticks(&mut s, 20)), 0);
    deliver_history(&mut s);
    assert_eq!(history_calls(&ticks(&mut s, 8)), 1);
    // Hiding it stops the polling.
    deliver_history(&mut s);
    key(&mut s, 'w');
    assert_eq!(history_calls(&ticks(&mut s, 40)), 0);
}

#[test]
fn graph_does_not_refresh_while_another_view_covers_it() {
    let mut s = state();
    key(&mut s, 'w');
    deliver_history(&mut s);
    key(&mut s, '?');
    assert_eq!(history_calls(&ticks(&mut s, 20)), 0);
    key(&mut s, 'q');
    assert_eq!(
        history_calls(&ticks(&mut s, 1)),
        1,
        "due as soon as it is back"
    );
}

#[test]
fn graph_history_and_live_stats() {
    let mut s = state();
    let stats = |down| dman_proto::GlobalStats {
        download_speed: down,
        ..sample_stats()
    };
    // Hidden: stats events are not collected.
    update(&mut s, Msg::Event(Event::Stats { stats: stats(1) }));
    assert!(s.graph.history.is_empty());
    key(&mut s, 'w');
    deliver_history(&mut s);
    assert_eq!(s.graph.history.len(), 90);
    let last_t = s.graph.history.last().map(|h| h.t).unwrap_or_default();
    update(&mut s, Msg::Event(Event::Stats { stats: stats(42) }));
    assert_eq!(s.graph.history.len(), 91);
    let last = s
        .graph
        .history
        .last()
        .copied()
        .unwrap_or(dman_proto::SpeedSample {
            t: 0,
            down: 0,
            up: 0,
        });
    assert_eq!((last.down, last.t > last_t), (42, true));
}

#[test]
fn graph_toggle_offline_shows_the_panel_without_asking_the_daemon() {
    let mut s = state();
    disconnect(&mut s);
    assert!(key(&mut s, 'w').is_empty());
    assert!(s.graph.visible);
    assert_eq!(history_calls(&ticks(&mut s, 20)), 0);
}

#[test]
fn graph_panel_leaves_the_list_its_rows() {
    let mut s = state();
    update(&mut s, Msg::Resize(120, 40));
    let before = s.list_height();
    key(&mut s, 'w');
    let after = s.list_height();
    assert!(after < before && after >= 20, "{before} -> {after}");
    // Too small a terminal: no panel, the list keeps its room.
    update(&mut s, Msg::Resize(80, 12));
    assert_eq!(s.layout().graph.height, 0);
}

#[test]
fn snapshot_downloads_with_graph_120x40() {
    let mut s = state();
    key(&mut s, 'w');
    deliver_history(&mut s);
    insta::assert_snapshot!(render(&mut s, 120, 40));
}

// --------------------------------------------------------------------- help

#[test]
fn help_lists_popup_keys() {
    let s = state();
    let text: String = views::help::lines(&s.keymap, &s.theme)
        .iter()
        .map(|l| l.to_string() + "\n")
        .collect();
    for needle in [
        "Popups",
        "Command palette",
        "Confirmation",
        "Prompt (rename",
        "File browser",
        "Directory picker",
        "Preview",
        "fetch the file list",
        "use this dir",
        "Rename",
        "Show / hide the global speed graph",
    ] {
        assert!(text.contains(needle), "help lacks `{needle}`:\n{text}");
    }
}

// ----- notifications and schedule brief ------------------------------------

fn finished(outcome: dman_proto::method::Outcome) -> Msg {
    Msg::Event(Event::JobFinished {
        id: dman_proto::JobId(3),
        name: "movie.mkv".into(),
        path: "/dl/movie.mkv".into(),
        outcome,
    })
}

#[test]
fn job_finished_sets_status_and_notifies_only_when_enabled() {
    use dman_proto::method::Outcome;
    let mut s = state();
    assert!(!s.desktop);
    let cmds = update(&mut s, finished(Outcome::Completed));
    assert!(cmds.is_empty());
    let last = s.status.last.clone().unwrap();
    assert_eq!(last.level, dman_proto::method::Level::Info);
    assert!(last.text.contains("Download complete") && last.text.contains("movie.mkv"));

    s.desktop = true;
    let cmds = update(
        &mut s,
        finished(Outcome::Failed {
            error: "404".into(),
        }),
    );
    assert_eq!(
        cmds,
        vec![Cmd::Notify {
            title: "Download failed".into(),
            body: "movie.mkv: 404".into()
        }]
    );
    assert_eq!(
        s.status.last.as_ref().unwrap().level,
        dman_proto::method::Level::Error
    );
    let cmds = update(&mut s, finished(Outcome::SeedCompleted));
    assert!(matches!(&cmds[..], [Cmd::Notify { title, .. }] if title == "Seeding finished"));

    // Disconnected: the status line still shows it, no desktop popup.
    update(
        &mut s,
        Msg::Conn(ConnState::Disconnected {
            retry_in: std::time::Duration::from_secs(1),
            last_error: "x".into(),
        }),
    );
    assert!(update(&mut s, finished(Outcome::Completed)).is_empty());
}

#[test]
fn tui_options_follow_notify_config() {
    let mut c = dman_config::Config::default();
    assert!(crate::TuiOptions::from_config(&c, "local").desktop);
    c.notify.desktop = false;
    assert!(
        !crate::TuiOptions::from_config(&c, "local")
            .initial_state()
            .desktop
    );
}

fn brief(
    active: Option<&str>,
    paused: bool,
    next: Option<i64>,
    nw: Option<&str>,
) -> dman_proto::ScheduleBrief {
    dman_proto::ScheduleBrief {
        active: active.map(str::to_owned),
        paused,
        next_change: next,
        next_window: nw.map(str::to_owned),
    }
}

// 2030-01-07 (a Monday) 12:00 UTC.
const NOW: i64 = 1_894_017_600;

#[test]
fn schedule_brief_texts() {
    use crate::widgets::status::brief_text;
    let utc = jiff::tz::TimeZone::UTC;
    let at = |h: i64| NOW + h * 3600;
    assert_eq!(
        brief_text(&brief(Some("work"), true, Some(at(5)), None), &utc, NOW).as_deref(),
        Some("⏸ work until 17:00")
    );
    assert_eq!(
        brief_text(&brief(Some("night"), false, Some(at(19)), None), &utc, NOW).as_deref(),
        Some("night until Tue 07:00")
    );
    assert_eq!(
        brief_text(&brief(None, false, Some(at(10)), Some("night")), &utc, NOW).as_deref(),
        Some("next night 22:00")
    );
    assert_eq!(brief_text(&brief(None, false, None, None), &utc, NOW), None);
}

#[test]
fn header_shows_the_schedule_brief() {
    let mut s = state();
    s.time_zone = jiff::tz::TimeZone::UTC;
    s.now_override = Some(NOW);
    let mut stats = sample_stats();
    stats.schedule = Some(brief(Some("work"), true, Some(NOW + 5 * 3600), None));
    update(&mut s, Msg::Event(Event::Stats { stats }));
    let text = render(&mut s, 120, 40);
    assert!(
        text.lines().next().unwrap().contains("⏸ work until 17:00"),
        "{text}"
    );
    insta::assert_snapshot!(render(&mut s, 120, 10));
    let narrow = render(&mut s, 80, 24);
    assert!(
        narrow.lines().next().unwrap().contains("work until 17:00"),
        "{narrow}"
    );
}

// ------------------------------------------------------------------- search

mod search_view {
    use dman_proto::AddSource;
    use dman_proto::search::{
        ProviderInfo, ProviderSetParams, ProviderStatus, ProviderTestReport, ProvidersImported,
        ResolvedMagnet, SearchHistoryEntry, SearchResolveParams, SearchResultDto,
        SearchStartParams, SearchStarted,
    };

    use super::*;
    use crate::views::providers::TestState;
    use crate::views::search::{Focus, Prov};

    // ------------------------------------------------------------ fixtures

    fn prov(name: &str, enabled: bool, verified: bool, builtin: bool, order: i32) -> ProviderInfo {
        ProviderInfo {
            name: name.into(),
            kind: if builtin { "html" } else { "torznab" }.into(),
            enabled,
            order,
            timeout_secs: 15,
            verified,
            builtin,
            url: format!("https://{}.example/search", name.to_lowercase()),
            description: Some(format!("{name} test provider")),
            rate_limit_per_min: Some(30),
            max_results: 100,
            api_key_ref: None,
        }
    }

    fn providers() -> Vec<ProviderInfo> {
        vec![
            prov("1337x", true, true, true, 10),
            prov("nyaa", true, false, true, 20),
            prov("tpb", false, true, true, 30),
            prov("mine", false, false, false, 40),
        ]
    }

    fn all_off() -> Vec<ProviderInfo> {
        providers()
            .into_iter()
            .map(|p| ProviderInfo {
                enabled: false,
                ..p
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn res(
        title: &str,
        size: u64,
        seeders: Option<u32>,
        leechers: Option<u32>,
        date: i64,
        sources: &[&str],
    ) -> SearchResultDto {
        SearchResultDto {
            title: title.into(),
            size: Some(size),
            seeders,
            leechers,
            date: Some(date),
            sources: sources.iter().map(|s| (*s).to_owned()).collect(),
            ..SearchResultDto::default()
        }
    }

    fn magnet(hash: &str) -> Option<String> {
        Some(format!("magnet:?xt=urn:btih:{hash}"))
    }

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    fn results() -> Vec<SearchResultDto> {
        vec![
            SearchResultDto {
                magnet: magnet("aaaa"),
                info_hash: Some("aaaa".into()),
                ..res(
                    "Ubuntu 24.04.1 Desktop amd64",
                    6 * GIB,
                    Some(1204),
                    Some(31),
                    1_724_932_800,
                    &["1337x", "tpb"],
                )
            },
            SearchResultDto {
                detail_url: Some("https://nyaa.example/view/2".into()),
                ..res(
                    "Ubuntu 24.04 Server",
                    2 * GIB,
                    Some(310),
                    Some(8),
                    1_714_046_400,
                    &["nyaa"],
                )
            },
            SearchResultDto {
                torrent_url: Some("https://1337x.example/t/3.torrent".into()),
                ..res(
                    "ubuntu-24.04-live-server",
                    900 * MIB,
                    Some(12),
                    Some(1),
                    1_714_651_200,
                    &["1337x"],
                )
            },
            SearchResultDto {
                magnet: magnet("dddd"),
                ..res(
                    "Kubuntu 24.04",
                    4 * GIB,
                    Some(4),
                    None,
                    1_699_358_400,
                    &["nyaa", "1337x"],
                )
            },
        ]
    }

    fn st() -> State {
        let mut s = state();
        s.time_zone = jiff::tz::TimeZone::UTC;
        s
    }

    fn reply(s: &mut State, cmd: &Cmd, result: Result<ApiReply, String>) -> Vec<Cmd> {
        let Cmd::Api(call) = cmd else {
            panic!("not an api cmd: {cmd:?}")
        };
        update(
            s,
            Msg::ActionResult {
                call: call.clone(),
                result,
            },
        )
    }

    fn deliver_providers(s: &mut State, list: Vec<ProviderInfo>) {
        update(
            s,
            Msg::ActionResult {
                call: ApiCall::ProvidersList,
                result: Ok(ApiReply::Providers(list)),
            },
        );
    }

    /// Open the search view with `list` providers known.
    fn open_search(s: &mut State, list: Vec<ProviderInfo>) {
        let cmds = key(s, 'f');
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersList)]);
        deliver_providers(s, list);
    }

    fn started(id: u64, names: &[&str]) -> Result<ApiReply, String> {
        Ok(ApiReply::SearchStarted(SearchStarted {
            search_id: id,
            providers: names.iter().map(|n| (*n).to_owned()).collect(),
        }))
    }

    fn update_event(
        id: u64,
        provider: &str,
        status: ProviderStatus,
        r: Vec<SearchResultDto>,
    ) -> Msg {
        Msg::Event(Event::SearchUpdate {
            search_id: id,
            provider: provider.into(),
            status,
            results: r,
        })
    }

    fn done(count: u32, ms: u64) -> ProviderStatus {
        ProviderStatus::Done {
            count,
            elapsed_ms: ms,
        }
    }

    /// Type `query`, press Enter and answer `search.start` with id 7.
    fn run_search(s: &mut State, query: &str) -> SearchStartParams {
        typed(s, query);
        let cmds = code(s, KeyCode::Enter);
        let Some(Cmd::Api(ApiCall::SearchStart(p))) = cmds.first().cloned() else {
            panic!("no search.start: {cmds:?}")
        };
        reply(s, &cmds[0], started(7, &["1337x", "nyaa"]));
        p
    }

    /// A finished search with mixed provider outcomes.
    fn searched() -> State {
        let mut s = st();
        open_search(&mut s, providers());
        run_search(&mut s, "ubuntu");
        update(
            &mut s,
            update_event(7, "1337x", done(3, 340), results()[..3].to_vec()),
        );
        update(
            &mut s,
            update_event(
                7,
                "nyaa",
                ProviderStatus::Failed {
                    error: "timed out after 15s".into(),
                },
                results()[..3].to_vec(),
            ),
        );
        s
    }

    fn rows(s: &State) -> Vec<String> {
        let run = s.search.run.as_ref().expect("a search");
        s.search
            .rows
            .iter()
            .map(|i| run.results[*i].title.clone())
            .collect()
    }

    fn add_text(s: &State) -> String {
        match s.view() {
            View::Add(a) => a.sources.text(),
            v => panic!("not the add view: {v:?}"),
        }
    }

    fn submit_prompt(s: &mut State, kind: char, text: &str) {
        key(s, kind);
        assert!(
            matches!(s.modal, Some(Modal::Prompt(_))),
            "prompt for {kind}"
        );
        typed(s, text);
        code(s, KeyCode::Enter);
    }

    // ------------------------------------------------------------- opening

    #[test]
    fn f_opens_search_and_loads_providers() {
        let mut s = st();
        let cmds = key(&mut s, 'f');
        assert!(matches!(s.view(), View::Search));
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersList)]);
        assert_eq!(s.search.focus, Focus::Query);
        deliver_providers(&mut s, providers());
        assert!(
            s.modal.is_none(),
            "providers are enabled: no first-run prompt"
        );
        assert_eq!(s.providers.list.len(), 4);
    }

    #[test]
    fn palette_opens_search_and_providers() {
        let mut s = st();
        key(&mut s, ':');
        update(&mut s, Msg::Paste("search torrents".into()));
        let cmds = code(&mut s, KeyCode::Enter);
        assert!(matches!(s.view(), View::Search));
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersList)]);
        code(&mut s, KeyCode::Esc);
        assert!(matches!(s.view(), View::Downloads));
        key(&mut s, ':');
        update(&mut s, Msg::Paste("search providers".into()));
        let cmds = code(&mut s, KeyCode::Enter);
        assert!(matches!(s.view(), View::Providers));
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersList)]);
    }

    #[test]
    fn first_run_prompt_when_no_provider_is_enabled() {
        let mut s = st();
        open_search(&mut s, all_off());
        let Some(Modal::Confirm(c)) = &s.modal else {
            panic!("no first-run prompt")
        };
        assert!(c.title.contains("No search provider is enabled"));
        assert!(c.lines.iter().any(|l| l.contains("off by default")));
        // Cancel keeps the search view and is not repeated.
        key(&mut s, 'n');
        assert!(s.modal.is_none());
        assert!(matches!(s.view(), View::Search));
        let cmds = code(&mut s, KeyCode::F(2));
        assert!(matches!(s.view(), View::Providers));
        deliver_providers(&mut s, all_off());
        assert!(s.modal.is_none());
        assert_eq!(cmds.len(), 1);
        code(&mut s, KeyCode::Esc);
        // Re-opening the search later does not ask again.
        code(&mut s, KeyCode::Esc);
        key(&mut s, 'f');
        deliver_providers(&mut s, all_off());
        assert!(s.modal.is_none());
    }

    #[test]
    fn first_run_prompt_offers_the_providers_settings() {
        let mut s = st();
        open_search(&mut s, all_off());
        let cmds = key(&mut s, 'y');
        assert!(matches!(s.view(), View::Providers));
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersList)]);
    }

    #[test]
    fn first_run_prompt_only_when_search_is_on_top() {
        let mut s = st();
        key(&mut s, 'f');
        code(&mut s, KeyCode::F(1));
        deliver_providers(&mut s, all_off());
        assert!(s.modal.is_none(), "help is on top");
    }

    // ------------------------------------------------------------ starting

    #[test]
    fn enter_starts_a_search() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu 24.04");
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::SearchStart(SearchStartParams {
                query: "ubuntu 24.04".into(),
                providers: None,
                category: None,
            }))]
        );
        assert_eq!(s.search.focus, Focus::Results);
        assert!(s.search.running());
        assert_eq!(s.pending, 1, "search.start is in flight");
    }

    #[test]
    fn keys_in_the_query_are_text() {
        let mut s = st();
        open_search(&mut s, providers());
        // `q`, `f`, `p` would quit / search / open settings outside the field.
        assert!("qfp?:".chars().all(|c| key(&mut s, c).is_empty()));
        assert_eq!(s.search.query.text(), "qfp?:");
        assert!(matches!(s.view(), View::Search));
    }

    #[test]
    fn category_and_provider_selection_are_sent() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu");
        code(&mut s, KeyCode::Tab);
        assert_eq!(s.search.focus, Focus::Category);
        typed(&mut s, "software");
        code(&mut s, KeyCode::Tab);
        assert_eq!(s.search.focus, Focus::Providers);
        // Only enabled providers are offered; switch the first one off.
        key(&mut s, ' ');
        assert!(s.search.off.contains("1337x"));
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::SearchStart(SearchStartParams {
                query: "ubuntu".into(),
                providers: Some(vec!["nyaa".into()]),
                category: Some("software".into()),
            }))]
        );
    }

    #[test]
    fn empty_query_and_no_provider_selected_are_refused() {
        let mut s = st();
        open_search(&mut s, providers());
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert_eq!(status_text(&s), "Type a query first");
        typed(&mut s, "x");
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Tab);
        key(&mut s, ' ');
        code(&mut s, KeyCode::Right);
        key(&mut s, ' ');
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert_eq!(status_text(&s), "Select at least one provider");
        assert!(s.search.run.is_none());
    }

    #[test]
    fn offline_search_is_refused_and_keeps_the_old_results() {
        let mut s = searched();
        disconnect(&mut s);
        key(&mut s, 'e');
        typed(&mut s, "more");
        let cmds = code(&mut s, KeyCode::Enter);
        assert!(cmds.is_empty());
        assert!(status_text(&s).contains("disabled"), "{}", status_text(&s));
        assert_eq!(s.search.run.as_ref().map(|r| r.results.len()), Some(3));
    }

    // ------------------------------------------------------------- results

    #[test]
    fn updates_merge_results_and_provider_statuses() {
        let mut s = st();
        open_search(&mut s, providers());
        run_search(&mut s, "ubuntu");
        let run = s.search.run.as_ref().expect("run");
        assert_eq!(run.id, Some(7));
        assert!(run.providers.iter().all(|(_, p)| *p == Prov::Running));
        update(
            &mut s,
            update_event(7, "1337x", done(2, 340), results()[..2].to_vec()),
        );
        let run = s.search.run.as_ref().expect("run");
        assert_eq!(
            run.providers[0],
            (
                "1337x".into(),
                Prov::Done {
                    count: 2,
                    elapsed_ms: 340
                }
            )
        );
        assert_eq!(run.providers[1], ("nyaa".into(), Prov::Running));
        assert_eq!(s.search.rows.len(), 2);
        // The list is cumulative: the next update replaces it.
        update(&mut s, update_event(7, "nyaa", done(2, 900), results()));
        assert_eq!(s.search.rows.len(), 4);
        assert!(s.search.running());
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 7 }));
        assert!(!s.search.running());
        assert_eq!(
            status_text(&s),
            "Search 'ubuntu': 4 results from 2 providers"
        );
    }

    #[test]
    fn a_failing_provider_is_shown_and_hides_nothing() {
        let mut s = searched();
        let run = s.search.run.as_ref().expect("run");
        assert_eq!(
            run.providers[1],
            ("nyaa".into(), Prov::Failed("timed out after 15s".into()))
        );
        assert_eq!(run.failed(), 1);
        assert_eq!(s.search.rows.len(), 3, "results of the other provider stay");
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 7 }));
        assert!(
            status_text(&s).ends_with(", 1 failed"),
            "{}",
            status_text(&s)
        );
        let text = render(&mut s, 120, 24);
        assert!(
            text.contains("nyaa ✗ failed: timed out after 15s"),
            "{text}"
        );
        assert!(text.contains("1337x ✓ 3 in 340 ms"), "{text}");
    }

    #[test]
    fn events_before_the_start_reply_are_replayed() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu");
        let cmds = code(&mut s, KeyCode::Enter);
        // The daemon is faster than the reply.
        update(
            &mut s,
            update_event(7, "1337x", done(1, 20), results()[..1].to_vec()),
        );
        update(
            &mut s,
            update_event(7, "nyaa", done(0, 30), results()[..1].to_vec()),
        );
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 7 }));
        assert!(s.search.rows.is_empty(), "id not known yet");
        reply(&mut s, &cmds[0], started(7, &["1337x", "nyaa"]));
        assert_eq!(s.search.rows.len(), 1);
        assert!(!s.search.running());
        let run = s.search.run.as_ref().expect("run");
        assert!(
            run.providers
                .iter()
                .all(|(_, p)| matches!(p, Prov::Done { .. }))
        );
    }

    #[test]
    fn events_of_other_searches_are_ignored() {
        let mut s = searched();
        update(
            &mut s,
            update_event(
                99,
                "x",
                done(1, 1),
                vec![res("other", 1, Some(1), None, 0, &["x"])],
            ),
        );
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 99 }));
        assert_eq!(s.search.rows.len(), 3);
        assert!(s.search.running());
    }

    #[test]
    fn no_enabled_provider_finishes_at_once() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu");
        let cmds = code(&mut s, KeyCode::Enter);
        reply(&mut s, &cmds[0], started(8, &[]));
        assert!(!s.search.running());
        assert!(status_text(&s).contains("No provider is enabled"));
    }

    #[test]
    fn failed_start_is_reported() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu");
        let cmds = code(&mut s, KeyCode::Enter);
        reply(&mut s, &cmds[0], Err("daemon exploded".into()));
        assert!(!s.search.running());
        assert_eq!(status_text(&s), "Search failed: daemon exploded");
        let text = render(&mut s, 80, 24);
        assert!(text.contains("search failed: daemon exploded"), "{text}");
        // A new search can be started.
        assert!(!s.search.starting);
        let cmds = key(&mut s, 'r');
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::Api(ApiCall::SearchStart(_))]
        ));
    }

    #[test]
    fn disconnect_while_starting_ends_the_search() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu");
        code(&mut s, KeyCode::Enter);
        disconnect(&mut s);
        assert!(!s.search.running());
        assert!(!s.search.starting);
    }

    #[test]
    fn lagged_events_reload_a_running_search() {
        let mut s = searched();
        let cmds = update(&mut s, Msg::Lagged);
        assert_eq!(cmds[0], Cmd::Resync);
        assert_eq!(cmds[1], Cmd::Api(ApiCall::SearchResults(7)));
        let snap = dman_proto::search::SearchSnapshot {
            search_id: 7,
            query: "ubuntu".into(),
            category: None,
            finished: true,
            providers: vec![("1337x".into(), done(4, 10)), ("nyaa".into(), done(0, 10))],
            results: results(),
        };
        reply(
            &mut s,
            &cmds[1],
            Ok(ApiReply::SearchSnapshot(Box::new(snap))),
        );
        assert_eq!(s.search.rows.len(), 4);
        assert!(!s.search.running());
    }

    // --------------------------------------------------------- cancel/leave

    #[test]
    fn esc_cancels_a_running_search_then_leaves() {
        let mut s = searched();
        let cmds = code(&mut s, KeyCode::Esc);
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::SearchCancel(7))]);
        assert!(
            matches!(s.view(), View::Search),
            "Esc cancelled, did not leave"
        );
        // A second Esc while cancelling does nothing more.
        assert!(code(&mut s, KeyCode::Esc).is_empty());
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 7 }));
        let run = s.search.run.as_ref().expect("run");
        assert!(run.finished);
        assert_eq!(
            run.providers[0].1,
            Prov::Done {
                count: 3,
                elapsed_ms: 340
            }
        );
        // Idle: Esc leaves.
        assert!(code(&mut s, KeyCode::Esc).is_empty());
        assert!(matches!(s.view(), View::Downloads));
    }

    #[test]
    fn running_providers_become_cancelled() {
        let mut s = st();
        open_search(&mut s, providers());
        run_search(&mut s, "ubuntu");
        code(&mut s, KeyCode::Esc);
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 7 }));
        let run = s.search.run.as_ref().expect("run");
        assert!(run.providers.iter().all(|(_, p)| *p == Prov::Cancelled));
    }

    #[test]
    fn leaving_keeps_the_search_and_results() {
        let mut s = searched();
        assert!(key(&mut s, 'q').is_empty(), "q leaves without cancelling");
        assert!(matches!(s.view(), View::Downloads));
        assert!(s.search.running());
        // Results keep streaming in while the view is closed.
        update(&mut s, update_event(7, "nyaa", done(4, 500), results()));
        update(&mut s, Msg::Event(Event::SearchFinished { search_id: 7 }));
        assert!(key(&mut s, 'f').len() == 1);
        assert_eq!(s.search.rows.len(), 4);
        assert_eq!(s.search.focus, Focus::Results, "results have the keyboard");
        assert_eq!(s.search.query.text(), "ubuntu");
    }

    // ----------------------------------------------------- sort and filter

    #[test]
    fn default_sort_is_seeders_and_cycles() {
        let mut s = searched();
        update(&mut s, update_event(7, "nyaa", done(1, 5), results()));
        assert_eq!(
            rows(&s),
            vec![
                "Ubuntu 24.04.1 Desktop amd64",
                "Ubuntu 24.04 Server",
                "ubuntu-24.04-live-server",
                "Kubuntu 24.04"
            ]
        );
        key(&mut s, 's'); // leechers
        assert_eq!(rows(&s)[0], "Ubuntu 24.04.1 Desktop amd64");
        key(&mut s, 's'); // size
        assert_eq!(rows(&s)[3], "ubuntu-24.04-live-server");
        key(&mut s, 's'); // date
        assert_eq!(rows(&s)[0], "Ubuntu 24.04.1 Desktop amd64");
        assert_eq!(rows(&s)[3], "Kubuntu 24.04");
        key(&mut s, 's'); // title
        assert_eq!(rows(&s)[0], "Kubuntu 24.04");
        assert_eq!(status_text(&s), "Sorted by title");
        key(&mut s, 's'); // source
        assert_eq!(s.search.sort.label(), "source");
        key(&mut s, 'S');
        assert!(s.search.reverse);
        assert_eq!(status_text(&s), "Sorted by source, reversed");
        key(&mut s, 's'); // seeders again
        assert_eq!(s.search.sort.label(), "seeders");
        assert_eq!(rows(&s)[0], "Kubuntu 24.04", "reversed: fewest first");
    }

    #[test]
    fn filters_by_seeders_size_source_and_title() {
        let mut s = searched();
        update(&mut s, update_event(7, "nyaa", done(1, 5), results()));
        submit_prompt(&mut s, 'm', "10");
        assert_eq!(rows(&s).len(), 3);
        assert_eq!(status_text(&s), "Minimum seeders: 10");
        submit_prompt(&mut s, 'z', "1GiB-4GiB");
        assert_eq!(rows(&s), vec!["Ubuntu 24.04 Server"]);
        assert_eq!(status_text(&s), "Size filter: 1.0 GiB – 4.0 GiB");
        // Filters stack; clear all with Ctrl-l.
        press(&mut s, KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert!(s.search.filters.is_empty());
        assert_eq!(rows(&s).len(), 4);
        submit_prompt(&mut s, 'o', "nyaa");
        assert_eq!(rows(&s), vec!["Ubuntu 24.04 Server", "Kubuntu 24.04"]);
        submit_prompt(&mut s, '/', "kbn");
        assert_eq!(rows(&s), vec!["Kubuntu 24.04"], "fuzzy title match");
        let text = render(&mut s, 120, 24);
        assert!(text.contains("source nyaa"), "{text}");
        assert!(text.contains("1/4 shown"), "{text}");
    }

    #[test]
    fn bad_filter_input_is_rejected() {
        let mut s = searched();
        submit_prompt(&mut s, 'm', "lots");
        assert!(status_text(&s).contains("must be a number"));
        submit_prompt(&mut s, 'z', "4GiB-1GiB");
        assert!(status_text(&s).contains("bigger than the largest"));
        submit_prompt(&mut s, 'z', "huge");
        assert!(status_text(&s).contains("is not a size"));
        assert!(s.search.filters.is_empty());
    }

    #[test]
    fn filtered_out_results_cannot_be_added_by_accident() {
        let mut s = searched();
        submit_prompt(&mut s, 'o', "zzz");
        assert!(s.search.rows.is_empty());
        // Nothing under the cursor: no add.
        let text = render(&mut s, 80, 24);
        assert!(text.contains("hidden by the filters"), "{text}");
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(matches!(s.view(), View::Search));
        assert_eq!(status_text(&s), "No result under the cursor");
    }

    #[test]
    fn cursor_follows_the_result_when_the_list_changes() {
        let mut s = searched();
        key(&mut s, 'j');
        assert_eq!(
            s.search.current().map(|r| r.title.as_str()),
            Some("Ubuntu 24.04 Server")
        );
        // A new result with more seeders pushes everything down.
        let mut more = results();
        more.insert(0, res("Top hit", GIB, Some(5000), Some(1), 1, &["tpb"]));
        update(&mut s, update_event(7, "nyaa", done(4, 5), more));
        assert_eq!(
            s.search.current().map(|r| r.title.as_str()),
            Some("Ubuntu 24.04 Server")
        );
    }

    // -------------------------------------------------------------- adding

    #[test]
    fn enter_opens_the_add_view_with_the_magnet() {
        let mut s = searched();
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(matches!(s.view(), View::Add(_)));
        assert_eq!(add_text(&s).trim(), "magnet:?xt=urn:btih:aaaa");
        // Leaving the (clean?) add view returns to the search with results kept.
        assert!(s.search.run.is_some());
    }

    #[test]
    fn a_adds_marked_results_in_one_add_view() {
        let mut s = searched();
        update(&mut s, update_event(7, "nyaa", done(1, 5), results()));
        key(&mut s, ' '); // Desktop (magnet); the cursor moves on
        code(&mut s, KeyCode::Down); // past the detail-only result
        key(&mut s, ' '); // live-server (torrent url)
        assert_eq!(s.search.selected.len(), 2);
        let text = render(&mut s, 80, 24);
        assert!(text.contains("2 marked"), "{text}");
        // Marked: Desktop (magnet) and live-server (torrent url).
        key(&mut s, 'a');
        assert!(matches!(s.view(), View::Add(_)));
        let t = add_text(&s);
        assert!(t.contains("magnet:?xt=urn:btih:aaaa"), "{t}");
        assert!(t.contains("https://1337x.example/t/3.torrent"), "{t}");
        assert!(s.search.selected.is_empty(), "marks are consumed");
    }

    #[test]
    fn torrent_url_only_results_are_added_as_http_sources() {
        let mut s = searched();
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Down);
        let cmds = key(&mut s, 'A');
        let [Cmd::Api(ApiCall::Add(p))] = cmds.as_slice() else {
            panic!("no jobs.add: {cmds:?}")
        };
        assert_eq!(
            p.sources,
            vec![AddSource::Uris {
                uris: vec!["https://1337x.example/t/3.torrent".into()]
            }]
        );
        assert_eq!(p.options, dman_proto::JobOptions::default());
        assert!(!p.paused);
        assert!(matches!(s.view(), View::Search));
    }

    #[test]
    fn capital_a_adds_directly_with_defaults() {
        let mut s = searched();
        key(&mut s, ' '); // Desktop (magnet), cursor moves on
        code(&mut s, KeyCode::Down); // skip the detail-only result
        key(&mut s, ' '); // live-server (torrent url)
        let cmds = key(&mut s, 'A');
        let [Cmd::Api(ApiCall::Add(p))] = cmds.as_slice() else {
            panic!("no jobs.add: {cmds:?}")
        };
        // Two marked results are two downloads, each its own source.
        assert_eq!(p.sources.len(), 2);
        assert!(matches!(s.view(), View::Search));
    }

    #[test]
    fn magnet_less_result_is_resolved_first() {
        let mut s = searched();
        code(&mut s, KeyCode::Down); // Ubuntu 24.04 Server: detail page only
        let cmds = code(&mut s, KeyCode::Enter);
        let expected = SearchResolveParams {
            detail_url: Some("https://nyaa.example/view/2".into()),
            provider: Some("nyaa".into()),
            ..SearchResolveParams::default()
        };
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::SearchResolve(expected))]);
        assert!(matches!(s.view(), View::Search), "waits for the magnet");
        assert!(status_text(&s).starts_with("Resolving 1 magnet link"));
        assert_eq!(s.search.resolving_keys().len(), 1);
        let text = render(&mut s, 80, 24);
        assert!(text.contains("resolving 1"), "{text}");
        // A second add while resolving is refused.
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(status_text(&s).contains("Still resolving"));
        reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::Resolved(ResolvedMagnet {
                magnet: "magnet:?xt=urn:btih:bbbb".into(),
            })),
        );
        assert!(matches!(s.view(), View::Add(_)));
        assert_eq!(add_text(&s).trim(), "magnet:?xt=urn:btih:bbbb");
        assert!(s.search.adding.is_none());
    }

    #[test]
    fn resolve_failure_is_reported_and_the_view_stays() {
        let mut s = searched();
        code(&mut s, KeyCode::Down);
        let cmds = code(&mut s, KeyCode::Enter);
        reply(&mut s, &cmds[0], Err("page has no magnet link".into()));
        assert!(matches!(s.view(), View::Search));
        assert!(
            status_text(&s).contains("Cannot add"),
            "{}",
            status_text(&s)
        );
        assert!(status_text(&s).contains("page has no magnet link"));
        assert!(s.search.adding.is_none());
    }

    #[test]
    fn resolved_and_failed_mix_adds_what_it_can() {
        let mut s = searched();
        update(&mut s, update_event(7, "nyaa", done(1, 5), results()));
        // Mark the detail-only result and the magnet result.
        key(&mut s, ' '); // Desktop (magnet)
        key(&mut s, ' '); // Server (detail)
        let cmds = key(&mut s, 'a');
        assert_eq!(cmds.len(), 1);
        reply(&mut s, &cmds[0], Err("blocked".into()));
        assert!(
            matches!(s.view(), View::Add(_)),
            "the magnet is still added"
        );
        assert_eq!(add_text(&s).trim(), "magnet:?xt=urn:btih:aaaa");
        assert!(
            status_text(&s).starts_with("1 of 2 could not be resolved"),
            "{}",
            status_text(&s)
        );
    }

    #[test]
    fn direct_add_waits_for_resolution() {
        let mut s = searched();
        code(&mut s, KeyCode::Down);
        let cmds = key(&mut s, 'A');
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::Api(ApiCall::SearchResolve(_))]
        ));
        let next = reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::Resolved(ResolvedMagnet {
                magnet: "magnet:?xt=urn:btih:bbbb".into(),
            })),
        );
        let [Cmd::Api(ApiCall::Add(p))] = next.as_slice() else {
            panic!("no jobs.add: {next:?}")
        };
        assert_eq!(
            p.sources,
            vec![AddSource::Uris {
                uris: vec!["magnet:?xt=urn:btih:bbbb".into()]
            }]
        );
    }

    #[test]
    fn result_without_any_link_cannot_be_added() {
        let mut s = st();
        open_search(&mut s, providers());
        run_search(&mut s, "x");
        update(
            &mut s,
            update_event(
                7,
                "1337x",
                done(1, 1),
                vec![res("bare", 1, Some(1), None, 0, &["1337x"])],
            ),
        );
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert!(
            status_text(&s).contains("no magnet link or page"),
            "{}",
            status_text(&s)
        );
        assert!(matches!(s.view(), View::Search));
    }

    #[test]
    fn disconnect_drops_a_pending_resolution() {
        let mut s = searched();
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Enter);
        assert!(s.search.adding.is_some());
        disconnect(&mut s);
        assert!(s.search.adding.is_none());
        assert!(matches!(
            s.search.run.as_ref().map(|r| r.finished),
            Some(false)
        ));
    }

    // ------------------------------------------------------------- history

    fn history() -> Vec<SearchHistoryEntry> {
        vec![
            SearchHistoryEntry {
                query: "debian 12".into(),
                providers: vec!["nyaa".into()],
                category: Some("software".into()),
                at: 1_724_932_800,
                result_count: 42,
            },
            SearchHistoryEntry {
                query: "ubuntu".into(),
                providers: vec![],
                category: None,
                at: 1_724_846_400,
                result_count: 7,
            },
        ]
    }

    fn open_history(s: &mut State) {
        let cmds = code(s, KeyCode::F(3));
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::SearchHistory)]);
        reply(s, &cmds[0], Ok(ApiReply::SearchHistory(history())));
    }

    #[test]
    fn history_lists_and_reruns() {
        let mut s = searched();
        open_history(&mut s);
        assert!(s.search.history_open);
        let text = render(&mut s, 80, 24);
        assert!(text.contains("debian 12"), "{text}");
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(
            cmds[1..],
            [Cmd::Api(ApiCall::SearchStart(SearchStartParams {
                query: "debian 12".into(),
                providers: Some(vec!["nyaa".into()]),
                category: Some("software".into()),
            }))],
            "the running search is cancelled first"
        );
        assert_eq!(cmds[0], Cmd::Api(ApiCall::SearchCancel(7)));
        assert!(!s.search.history_open);
        assert_eq!(s.search.query.text(), "debian 12");
        assert_eq!(s.search.category.text(), "software");
        assert_eq!(s.search.focus, Focus::Results);
    }

    #[test]
    fn history_all_providers_entry_resets_the_selection() {
        let mut s = st();
        open_search(&mut s, providers());
        s.search.off.insert("nyaa".into());
        open_history(&mut s);
        code(&mut s, KeyCode::Down);
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::SearchStart(SearchStartParams {
                query: "ubuntu".into(),
                providers: None,
                category: None,
            }))]
        );
    }

    #[test]
    fn x_clears_the_history_after_confirmation() {
        let mut s = searched();
        open_history(&mut s);
        assert!(key(&mut s, 'x').is_empty());
        let Some(Modal::Confirm(c)) = &s.modal else {
            panic!("no confirmation")
        };
        assert!(c.title.starts_with("Clear the search history"));
        key(&mut s, 'n');
        assert!(s.modal.is_none());
        assert_eq!(s.search.history.len(), 2, "nothing was cleared");
        key(&mut s, 'x');
        let cmds = key(&mut s, 'y');
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::SearchHistoryClear)]);
        reply(&mut s, &cmds[0], Ok(ApiReply::Done));
        assert!(s.search.history.is_empty());
        assert_eq!(status_text(&s), "Search history cleared");
    }

    #[test]
    fn esc_and_h_close_the_history_first() {
        let mut s = searched();
        open_history(&mut s);
        assert!(code(&mut s, KeyCode::Esc).is_empty());
        assert!(!s.search.history_open);
        assert!(matches!(s.view(), View::Search));
        open_history(&mut s);
        key(&mut s, 'h');
        assert!(!s.search.history_open);
    }

    // ----------------------------------------------------------- providers

    fn open_providers() -> State {
        let mut s = st();
        open_search(&mut s, providers());
        let cmds = code(&mut s, KeyCode::F(2));
        assert!(matches!(s.view(), View::Providers));
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersList)]);
        deliver_providers(&mut s, providers());
        s
    }

    #[test]
    fn space_toggles_and_warns_about_unverified_providers() {
        let mut s = open_providers();
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Down); // mine: custom, disabled, unverified
        let cmds = key(&mut s, ' ');
        let expected = ProviderSetParams {
            name: "mine".into(),
            enabled: Some(true),
            ..ProviderSetParams::default()
        };
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersSet(expected))]);
        reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::Provider(Box::new(ProviderInfo {
                enabled: true,
                ..prov("mine", false, false, false, 40)
            }))),
        );
        assert!(s.providers.list[3].enabled);
        assert_eq!(
            status_text(&s),
            "mine enabled — definition not verified against the live site"
        );
        // Verified providers get no warning.
        code(&mut s, KeyCode::Up);
        let cmds = code(&mut s, KeyCode::Enter);
        reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::Provider(Box::new(ProviderInfo {
                enabled: true,
                ..prov("tpb", false, true, true, 30)
            }))),
        );
        assert_eq!(status_text(&s), "tpb enabled");
        // The search selector now offers it.
        code(&mut s, KeyCode::Esc);
        assert_eq!(
            crate::views::providers::enabled(&s)
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["1337x", "nyaa", "tpb", "mine"]
        );
    }

    #[test]
    fn toggle_failure_reloads_the_list() {
        let mut s = open_providers();
        let cmds = key(&mut s, ' ');
        let next = reply(&mut s, &cmds[0], Err("config is read-only".into()));
        assert_eq!(next, vec![Cmd::Api(ApiCall::ProvidersList)]);
        assert!(status_text(&s).contains("config is read-only"));
    }

    fn order_set(name: &str, order: i32) -> Cmd {
        Cmd::Api(ApiCall::ProvidersSet(ProviderSetParams {
            name: name.into(),
            order: Some(order),
            ..ProviderSetParams::default()
        }))
    }

    #[test]
    fn reorder_swaps_the_orders_of_neighbours() {
        let mut s = open_providers();
        let cmds = key(&mut s, 'J');
        assert_eq!(cmds, vec![order_set("nyaa", 10), order_set("1337x", 20)]);
        let names: Vec<&str> = s.providers.list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["nyaa", "1337x", "tpb", "mine"]);
        assert_eq!(
            s.providers.current().map(|p| p.name.as_str()),
            Some("1337x")
        );
        // `-` and K move up again; `+` moves down.
        let cmds = key(&mut s, 'K');
        assert_eq!(cmds, vec![order_set("1337x", 10), order_set("nyaa", 20)]);
        assert_eq!(s.providers.cursor, 0);
        assert_eq!(key(&mut s, '+').len(), 2);
        assert_eq!(key(&mut s, '-').len(), 2);
        // Already first.
        assert!(key(&mut s, 'K').is_empty());
        assert_eq!(status_text(&s), "Already at the end of the list");
    }

    #[test]
    fn reorder_renumbers_when_orders_are_tied() {
        let mut s = st();
        open_search(&mut s, providers());
        code(&mut s, KeyCode::F(2));
        let tied = vec![
            prov("a", true, true, true, 0),
            prov("b", true, true, true, 0),
            prov("c", true, true, true, 5),
        ];
        deliver_providers(&mut s, tied);
        let cmds = key(&mut s, 'J'); // a moves behind b
        let names: Vec<&str> = s.providers.list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        // `b` keeps order 0; only the changed ones are sent.
        assert_eq!(cmds, vec![order_set("a", 1), order_set("c", 2)]);
    }

    #[test]
    fn reorder_needs_a_connection() {
        let mut s = open_providers();
        disconnect(&mut s);
        assert!(key(&mut s, 'J').is_empty());
        assert_eq!(s.providers.list[0].name, "1337x", "list unchanged");
        assert!(status_text(&s).contains("disabled"));
    }

    fn report(ok: bool) -> ProviderTestReport {
        ProviderTestReport {
            name: "1337x".into(),
            ok,
            error: (!ok).then(|| "HTTP 503 from the site".into()),
            results: if ok { 25 } else { 0 },
            sample: ok.then(|| {
                res(
                    "Ubuntu 24.04 sample",
                    6 * GIB,
                    Some(99),
                    Some(3),
                    0,
                    &["1337x"],
                )
            }),
            elapsed_ms: 812,
        }
    }

    #[test]
    fn test_runs_in_the_background_and_shows_the_report() {
        let mut s = open_providers();
        let cmds = key(&mut s, 't');
        assert_eq!(cmds, vec![Cmd::Api(ApiCall::ProvidersTest("1337x".into()))]);
        assert_eq!(s.providers.tests.get("1337x"), Some(&TestState::Running));
        // Not blocking: the list still navigates, a second test is refused.
        code(&mut s, KeyCode::Down);
        code(&mut s, KeyCode::Up);
        assert!(key(&mut s, 't').is_empty());
        assert!(status_text(&s).contains("already being tested"));
        // The spinner animates while it runs.
        s.dirty = false;
        update(&mut s, Msg::Tick);
        assert!(s.dirty);
        reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::ProviderTest(Box::new(report(true)))),
        );
        assert!(matches!(
            s.providers.tests.get("1337x"),
            Some(TestState::Done(_))
        ));
        assert_eq!(status_text(&s), "1337x: ok, 25 results");
        let text = render(&mut s, 80, 24);
        assert!(text.contains("ok: 25 results in 812 ms"), "{text}");
        assert!(text.contains("sample: Ubuntu 24.04 sample"), "{text}");
    }

    #[test]
    fn failing_test_shows_the_error() {
        let mut s = open_providers();
        let cmds = key(&mut s, 't');
        reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::ProviderTest(Box::new(report(false)))),
        );
        assert_eq!(
            status_text(&s),
            "1337x: test failed: HTTP 503 from the site"
        );
        let text = render(&mut s, 80, 24);
        assert!(
            text.contains("failed after 812 ms: HTTP 503 from the site"),
            "{text}"
        );
        // An RPC error is shown too.
        let cmds = key(&mut s, 't');
        reply(&mut s, &cmds[0], Err("timed out".into()));
        assert_eq!(
            s.providers.tests.get("1337x"),
            Some(&TestState::Failed("timed out".into()))
        );
    }

    #[test]
    fn disconnect_fails_running_tests() {
        let mut s = open_providers();
        key(&mut s, 't');
        disconnect(&mut s);
        assert!(matches!(
            s.providers.tests.get("1337x"),
            Some(TestState::Failed(_))
        ));
    }

    #[test]
    fn import_reads_the_file_then_sends_it() {
        let mut s = open_providers();
        key(&mut s, 'i');
        typed(&mut s, "~/mine.toml");
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(cmds, vec![Cmd::ReadProviderFile("~/mine.toml".into())]);
        let cmds = update(
            &mut s,
            Msg::ProviderFile(Ok((
                "~/mine.toml".into(),
                "[[provider]]\nname = 'x'\n".into(),
            ))),
        );
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::ProvidersImport(
                "[[provider]]\nname = 'x'\n".into()
            ))]
        );
        let next = reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::Imported(ProvidersImported {
                names: vec!["x".into(), "y".into()],
            })),
        );
        assert_eq!(next, vec![Cmd::Api(ApiCall::ProvidersList)]);
        assert_eq!(status_text(&s), "Imported 2 providers: x, y");
    }

    #[test]
    fn import_problems_reach_the_status_line() {
        let mut s = open_providers();
        key(&mut s, 'i');
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        assert_eq!(status_text(&s), "Enter a file path");
        update(
            &mut s,
            Msg::ProviderFile(Err("nope.toml: No such file or directory".into())),
        );
        assert!(status_text(&s).starts_with("Import failed: nope.toml"));
        let cmds = update(&mut s, Msg::ProviderFile(Ok(("a".into(), "junk".into()))));
        reply(
            &mut s,
            &cmds[0],
            Err("invalid providers file: expected a table".into()),
        );
        assert!(status_text(&s).contains("invalid providers file"));
    }

    #[test]
    fn export_writes_what_the_daemon_returns() {
        let mut s = open_providers();
        key(&mut s, 'e');
        // Pre-filled with a file name; Enter accepts it.
        let cmds = code(&mut s, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::ProvidersExport {
                path: "dman-providers.toml".into()
            })]
        );
        let next = reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::Exported("[[provider]]\n".into())),
        );
        assert_eq!(
            next,
            vec![Cmd::WriteProviderFile {
                path: "dman-providers.toml".into(),
                content: "[[provider]]\n".into()
            }]
        );
        update(
            &mut s,
            Msg::ProviderFileWritten(Ok("dman-providers.toml".into())),
        );
        assert_eq!(status_text(&s), "Exported providers to dman-providers.toml");
        update(
            &mut s,
            Msg::ProviderFileWritten(Err("dman-providers.toml already exists".into())),
        );
        assert!(status_text(&s).starts_with("Export failed"));
    }

    #[test]
    fn only_custom_providers_can_be_removed_after_confirmation() {
        let mut s = open_providers();
        assert!(key(&mut s, 'D').is_empty());
        assert!(s.modal.is_none());
        assert!(status_text(&s).contains("built in"), "{}", status_text(&s));
        key(&mut s, 'G');
        assert_eq!(s.providers.current().map(|p| p.name.as_str()), Some("mine"));
        key(&mut s, 'D');
        let Some(Modal::Confirm(c)) = &s.modal else {
            panic!("no confirmation")
        };
        assert!(c.title.contains("mine"));
        key(&mut s, 'n');
        assert!(s.modal.is_none());
        key(&mut s, 'D');
        let cmds = key(&mut s, 'y');
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::ProvidersRemove("mine".into()))]
        );
        let next = reply(&mut s, &cmds[0], Ok(ApiReply::Done));
        assert_eq!(next, vec![Cmd::Api(ApiCall::ProvidersList)]);
        assert_eq!(status_text(&s), "Removed provider mine");
    }

    #[test]
    fn providers_view_goes_back_to_the_search() {
        let mut s = open_providers();
        code(&mut s, KeyCode::Esc);
        assert!(matches!(s.view(), View::Search));
        code(&mut s, KeyCode::F(2));
        key(&mut s, 'q');
        assert!(matches!(s.view(), View::Search));
    }

    // ----------------------------------------------------------- snapshots

    fn mixed_state() -> State {
        // 1337x done, nyaa failed, tpb still running.
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu 24.04");
        let cmds = code(&mut s, KeyCode::Enter);
        reply(&mut s, &cmds[0], started(7, &["1337x", "nyaa", "tpb"]));
        update(
            &mut s,
            update_event(7, "1337x", done(3, 340), results()[..3].to_vec()),
        );
        update(
            &mut s,
            update_event(
                7,
                "nyaa",
                ProviderStatus::Failed {
                    error: "timed out after 15s".into(),
                },
                results(),
            ),
        );
        key(&mut s, ' ');
        s
    }

    #[test]
    fn snapshot_search_results_80x24() {
        let mut s = mixed_state();
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_search_results_120x40() {
        let mut s = mixed_state();
        code(&mut s, KeyCode::Down);
        submit_prompt(&mut s, 'm', "5");
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }

    #[test]
    fn snapshot_search_form_80x24() {
        let mut s = st();
        open_search(&mut s, providers());
        typed(&mut s, "ubuntu");
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Tab);
        key(&mut s, ' ');
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_search_history_80x24() {
        let mut s = searched();
        open_history(&mut s);
        code(&mut s, KeyCode::Down);
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_providers_80x24() {
        let mut s = open_providers();
        key(&mut s, 't');
        code(&mut s, KeyCode::Down);
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_providers_120x40() {
        let mut s = open_providers();
        let cmds = key(&mut s, 't');
        reply(
            &mut s,
            &cmds[0],
            Ok(ApiReply::ProviderTest(Box::new(report(true)))),
        );
        key(&mut s, 'G');
        update(
            &mut s,
            Msg::ActionResult {
                call: ApiCall::ProvidersSet(ProviderSetParams::default()),
                result: Ok(ApiReply::Provider(Box::new(ProviderInfo {
                    enabled: true,
                    ..prov("mine", false, false, false, 40)
                }))),
            },
        );
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }

    #[test]
    fn snapshot_first_run_prompt_80x24() {
        let mut s = st();
        open_search(&mut s, all_off());
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_first_run_prompt_120x40() {
        let mut s = st();
        open_search(&mut s, all_off());
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }
}

// -------------------------------------------------------------------- video

mod video_view {
    use super::*;
    use crate::views::add::options::Field;
    use crate::views::add::preview::Preview;
    use crate::views::add::source::Kind;
    use crate::views::add::video::{ProbeFetch, Section};
    use crate::views::add::{AddState, Focus, Overlay};
    use dman_proto::method::EngineStatus;
    use dman_proto::video::{
        PlaylistEntry, VideoFormat, VideoPlan, VideoPlanKind, VideoStreamKind,
    };
    use dman_proto::{
        AddOutcome, AddParams, AddSource, SourceKind, VideoOptions, VideoProbe, VideoProbeParams,
    };

    const YT: &str = "https://www.youtube.com/watch?v=abc123";
    const OTHER: &str = "https://media.example.org/clip/42";

    fn engine(available: bool, ffmpeg: bool, reason: Option<&str>) -> Msg {
        Msg::Event(Event::Engine {
            status: EngineStatus {
                engine: Engine::YtDlp,
                available,
                version: available.then(|| "2026.10.01".to_owned()),
                features: if ffmpeg {
                    vec!["ffmpeg 7.1".into(), "merge".into()]
                } else {
                    vec![]
                },
                reason: reason.map(str::to_owned),
            },
        })
    }

    fn open(engine_msg: Option<Msg>) -> State {
        let mut s = state();
        if let Some(m) = engine_msg {
            update(&mut s, m);
        }
        key(&mut s, 'a');
        s
    }

    fn add(s: &State) -> &AddState {
        match s.view() {
            View::Add(a) => a,
            v => panic!("not the add view: {v:?}"),
        }
    }

    fn ctrl(s: &mut State, c: char) -> Vec<Cmd> {
        press(s, KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn lines(s: &mut State, lines: &[&str]) {
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                code(s, KeyCode::Enter);
            }
            typed(s, l);
        }
    }

    fn kinds(s: &State) -> Vec<Kind> {
        add(s).all_tokens().iter().map(|t| t.kind).collect()
    }

    fn focus(s: &mut State, f: Field) {
        if let Some(View::Add(a)) = s.views.last_mut() {
            a.focus = Focus::Field(f);
        }
    }

    fn set(s: &mut State, f: Field, text: &str) {
        if let Some(View::Add(a)) = s.views.last_mut() {
            a.form.set_text(f, text);
        }
    }

    fn preview(s: &State) -> &Preview {
        match &add(s).overlay {
            Some(Overlay::Preview(p)) => p,
            other => panic!("no preview: {other:?}"),
        }
    }

    fn add_call(cmds: &[Cmd]) -> AddParams {
        match cmds {
            [Cmd::Api(ApiCall::Add(p))] => (**p).clone(),
            other => panic!("expected an add call, got {other:?}"),
        }
    }

    fn fmt(id: &str, h: Option<u32>, kind: VideoStreamKind) -> VideoFormat {
        VideoFormat {
            id: id.into(),
            ext: if kind == VideoStreamKind::AudioOnly {
                "m4a"
            } else {
                "mp4"
            }
            .into(),
            vcodec: (kind != VideoStreamKind::AudioOnly).then(|| "avc1.640028".to_owned()),
            acodec: (kind != VideoStreamKind::VideoOnly).then(|| "mp4a.40.2".to_owned()),
            width: h.map(|h| h * 16 / 9),
            height: h,
            fps: h.map(|_| 30.0),
            size: Some(u64::from(h.unwrap_or(1)) * 100_000),
            note: None,
            kind,
        }
    }

    fn plan(preset: &str, kind: VideoPlanKind, why: &str) -> VideoPlan {
        VideoPlan {
            preset: preset.into(),
            kind,
            selector: "bv*+ba/b".into(),
            explanation: why.into(),
            alternative: None,
        }
    }

    fn probe() -> VideoProbe {
        let mut p1080 = plan(
            "1080p",
            VideoPlanKind::Single,
            "ffmpeg missing: best single-file format 720p; separate 1080p streams available with consent",
        );
        p1080.alternative = Some(Box::new(plan(
            "1080p",
            VideoPlanKind::Separate,
            "1080p video and audio as two files",
        )));
        VideoProbe {
            url: YT.into(),
            id: Some("abc123".into()),
            title: Some("Big Buck Bunny".into()),
            duration: Some(596.0),
            uploader: Some("Blender Foundation".into()),
            formats: vec![
                fmt("18", Some(360), VideoStreamKind::Combined),
                fmt("137", Some(1080), VideoStreamKind::VideoOnly),
                fmt("140", None, VideoStreamKind::AudioOnly),
                fmt("sb0", Some(90), VideoStreamKind::Other),
            ],
            subtitles: vec!["en".into(), "de".into()],
            ffmpeg: false,
            plans: vec![
                plan("best", VideoPlanKind::Single, "best single-file format"),
                p1080,
                plan("audio:m4a", VideoPlanKind::Audio, "audio only (m4a)"),
            ],
            ..VideoProbe::default()
        }
    }

    fn playlist_probe() -> VideoProbe {
        VideoProbe {
            url: "https://www.youtube.com/playlist?list=PL1".into(),
            title: Some("Conference talks".into()),
            playlist: true,
            ffmpeg: true,
            entries: (1..=5)
                .map(|i| PlaylistEntry {
                    index: i,
                    title: Some(format!("Talk number {i}")),
                    duration: Some(f64::from(i) * 600.0),
                })
                .collect(),
            ..VideoProbe::default()
        }
    }

    fn deliver(s: &mut State, url: &str, playlist: bool, result: Result<VideoProbe, String>) {
        update(
            s,
            Msg::ActionResult {
                call: ApiCall::VideoProbe(VideoProbeParams {
                    url: url.into(),
                    playlist,
                    cookies_from_browser: None,
                }),
                result: result.map(|p| ApiReply::VideoProbe(Box::new(p))),
            },
        );
    }

    fn probed() -> State {
        let mut s = open(Some(engine(true, false, Some("ffmpeg not found in PATH"))));
        lines(&mut s, &[YT]);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        deliver(&mut s, YT, false, Ok(probe()));
        s
    }

    #[test]
    fn detection_follows_the_engine() {
        // yt-dlp present: known sites are video, others stay http.
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT, OTHER, "https://example.org/a.iso"]);
        assert_eq!(kinds(&s), [Kind::Video, Kind::Https, Kind::Https]);
        // No engine information yet: nothing is classified.
        let mut s = open(None);
        lines(&mut s, &[YT]);
        assert_eq!(kinds(&s), [Kind::Https]);
        // yt-dlp unavailable: a hint, and the plan keeps plain http.
        let mut s = open(Some(engine(false, false, Some("yt-dlp not found in PATH"))));
        lines(&mut s, &[YT]);
        assert_eq!(kinds(&s), [Kind::VideoUnavailable]);
        assert_eq!(
            Kind::VideoUnavailable.label(),
            "video (yt-dlp not installed — will download the page)"
        );
        let hint = add(&s).engine_hint().expect("hint").0;
        assert!(hint.contains("yt-dlp not found in PATH"), "{hint}");
        ctrl(&mut s, 's');
        let p = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(
            p.sources,
            vec![AddSource::Uris {
                uris: vec![YT.into()]
            }]
        );
    }

    #[test]
    fn engine_event_updates_an_open_form() {
        let mut s = open(None);
        lines(&mut s, &[YT]);
        assert_eq!(kinds(&s), [Kind::Https]);
        update(&mut s, engine(true, true, None));
        assert_eq!(kinds(&s), [Kind::Video]);
    }

    #[test]
    fn try_as_video_toggles_per_line() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT, OTHER]);
        // Cursor is on the second (unknown) line.
        ctrl(&mut s, 'g');
        assert_eq!(kinds(&s), [Kind::Video, Kind::Video]);
        ctrl(&mut s, 'g');
        assert_eq!(kinds(&s), [Kind::Video, Kind::Https]);
        // A detected line can be turned back into plain http.
        code(&mut s, KeyCode::Up);
        ctrl(&mut s, 'g');
        assert_eq!(kinds(&s), [Kind::Https, Kind::Https]);
        // Unavailable engine: refused with the reason.
        let mut s = open(Some(engine(false, false, Some("yt-dlp not found"))));
        lines(&mut s, &[OTHER]);
        ctrl(&mut s, 'g');
        assert!(status_text(&s).contains("yt-dlp not found"));
        assert_eq!(kinds(&s), [Kind::Https]);
    }

    #[test]
    fn video_section_appears_only_with_a_video_line() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &["https://example.org/a.iso"]);
        assert!(!add(&s).visible_fields().contains(&Field::VQuality));
        let mut s2 = open(Some(engine(true, true, None)));
        lines(&mut s2, &[YT]);
        assert!(add(&s2).visible_fields().contains(&Field::VQuality));
        // Tab walks into the video section and wraps back to the sources.
        for _ in 0..Field::FULL.len() {
            code(&mut s2, KeyCode::Tab);
        }
        assert_eq!(add(&s2).focus, Focus::Field(Field::VSeparate));
        code(&mut s2, KeyCode::Tab);
        assert_eq!(add(&s2).focus, Focus::Sources);
    }

    #[test]
    fn video_options_become_the_video_source() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT, "https://example.org/a.iso"]);
        for (f, t) in [
            (Field::VQuality, "1080p"),
            (Field::VContainer, "mkv"),
            (Field::VSubs, "en, de"),
            (Field::VSponsor, "sponsor selfpromo"),
            (Field::VItems, "1-3, 7"),
            (Field::VTemplate, "%(title)s.%(ext)s"),
            (Field::VCookies, "firefox"),
        ] {
            set(&mut s, f, t);
        }
        for f in [
            Field::VSubsAuto,
            Field::VThumb,
            Field::VMeta,
            Field::VPlaylist,
            Field::VSeparate,
        ] {
            focus(&mut s, f);
            key(&mut s, ' ');
        }
        ctrl(&mut s, 's');
        let p = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(p.sources.len(), 2);
        let want = VideoOptions {
            quality: Some("1080p".into()),
            container: Some("mkv".into()),
            subtitles: Some(dman_proto::VideoSubtitles {
                langs: vec!["en".into(), "de".into()],
                embed: true,
                auto: true,
            }),
            embed_thumbnail: Some(true),
            embed_metadata: Some(true),
            sponsorblock_remove: Some(vec!["sponsor".into(), "selfpromo".into()]),
            playlist: true,
            playlist_items: Some("1-3,7".into()),
            output_template: Some("%(title)s.%(ext)s".into()),
            cookies_from_browser: Some("firefox".into()),
            allow_separate_streams: true,
            ..VideoOptions::default()
        };
        assert_eq!(
            p.sources[0],
            AddSource::Video {
                url: YT.into(),
                video: Box::new(want)
            }
        );
        assert!(matches!(&p.sources[1], AddSource::Uris { .. }));
    }

    #[test]
    fn invalid_video_options_block_only_with_a_video_line() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT]);
        set(&mut s, Field::VQuality, "8k");
        set(&mut s, Field::VContainer, "avi");
        assert!(ctrl(&mut s, 's').is_empty());
        assert!(status_text(&s).contains("Quality"), "{}", status_text(&s));
        assert_eq!(add(&s).focus, Focus::Field(Field::VQuality));
        // The same values are ignored when no source is a video.
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &["https://example.org/a.iso"]);
        set(&mut s, Field::VQuality, "8k");
        ctrl(&mut s, 's');
        assert!(matches!(add(&s).overlay, Some(Overlay::Preview(_))));
    }

    #[test]
    fn probe_then_choose_a_plan_then_add() {
        let mut s = open(Some(engine(true, false, Some("ffmpeg not found in PATH"))));
        lines(&mut s, &[YT]);
        ctrl(&mut s, 's');
        let cmds = key(&mut s, 'f');
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::VideoProbe(VideoProbeParams {
                url: YT.into(),
                playlist: false,
                cookies_from_browser: None,
            }))]
        );
        assert!(preview(&s).probing());
        // The UI stays responsive: ticks animate, other keys are ignored.
        assert!(
            !update(&mut s, Msg::Tick)
                .iter()
                .any(|c| matches!(c, Cmd::Quit))
        );
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        deliver(&mut s, YT, false, Ok(probe()));
        let ProbeFetch::Done(ui) = &preview(&s).probe else {
            panic!("not probed")
        };
        assert_eq!(ui.plans.len(), 4, "the alternative is a row of its own");
        assert_eq!(ui.formats.len(), 3, "storyboards are left out");
        // Choose the alternative (separate streams) of 1080p: rows best, 1080p, 1080p alt.
        key(&mut s, 'j');
        key(&mut s, 'j');
        key(&mut s, ' ');
        let p = add_call(&code(&mut s, KeyCode::Enter));
        let AddSource::Video { url, video } = &p.sources[0] else {
            panic!("{:?}", p.sources)
        };
        assert_eq!(url, YT);
        assert_eq!(video.quality.as_deref(), Some("1080p"));
        assert!(video.allow_separate_streams);
        assert_eq!(video.format_id, None);
    }

    #[test]
    fn choosing_a_format_sets_format_id() {
        let mut s = probed();
        // Plans -> Formats, second row (137).
        code(&mut s, KeyCode::Tab);
        key(&mut s, 'j');
        key(&mut s, ' ');
        let p = add_call(&code(&mut s, KeyCode::Enter));
        let AddSource::Video { video, .. } = &p.sources[0] else {
            panic!()
        };
        assert_eq!(video.format_id.as_deref(), Some("137"));
        assert_eq!(video.quality, None);
        // Choosing nothing keeps the form's options.
        let mut s = probed();
        let p = add_call(&code(&mut s, KeyCode::Enter));
        assert_eq!(
            p.sources[0],
            AddSource::Video {
                url: YT.into(),
                video: Box::default()
            }
        );
    }

    #[test]
    fn playlist_items_follow_the_checkboxes() {
        let url = "https://www.youtube.com/playlist?list=PL1";
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[url]);
        focus(&mut s, Field::VPlaylist);
        key(&mut s, ' ');
        ctrl(&mut s, 's');
        let cmds = key(&mut s, 'f');
        assert_eq!(
            cmds,
            vec![Cmd::Api(ApiCall::VideoProbe(VideoProbeParams {
                url: url.into(),
                playlist: true,
                cookies_from_browser: None,
            }))]
        );
        deliver(&mut s, url, true, Ok(playlist_probe()));
        let ProbeFetch::Done(ui) = &preview(&s).probe else {
            panic!()
        };
        assert_eq!(ui.section, Section::Entries);
        assert_eq!(ui.chosen_entries(), [1, 2, 3, 4, 5]);
        // Uncheck 2 (the cursor then moves on to 3), then 4.
        key(&mut s, 'j');
        key(&mut s, ' ');
        key(&mut s, 'j');
        key(&mut s, ' ');
        let p = add_call(&code(&mut s, KeyCode::Enter));
        let AddSource::Video { video, .. } = &p.sources[0] else {
            panic!()
        };
        assert!(video.playlist);
        assert_eq!(video.playlist_items.as_deref(), Some("1,3,5"));
        // None selected blocks adding; all selected means the whole playlist.
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[url]);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        deliver(&mut s, url, false, Ok(playlist_probe()));
        key(&mut s, 'a');
        assert_eq!(
            preview(&s).blocker(),
            Some("select at least one playlist entry")
        );
        assert!(code(&mut s, KeyCode::Enter).is_empty());
        key(&mut s, 'a');
        let p = add_call(&code(&mut s, KeyCode::Enter));
        let AddSource::Video { video, .. } = &p.sources[0] else {
            panic!()
        };
        assert!(video.playlist && video.playlist_items.is_none());
    }

    #[test]
    fn esc_cancels_the_probe_and_drops_its_answer() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT]);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        assert!(code(&mut s, KeyCode::Esc).is_empty());
        assert_eq!(preview(&s).probe, ProbeFetch::Idle);
        deliver(&mut s, YT, false, Ok(probe()));
        assert_eq!(preview(&s).probe, ProbeFetch::Idle);
        // Probing again works; a failure is shown and can be retried.
        key(&mut s, 'f');
        deliver(
            &mut s,
            YT,
            false,
            Err("Video probe failed: unsupported URL".into()),
        );
        assert!(matches!(&preview(&s).probe, ProbeFetch::Failed(e) if e.contains("unsupported")));
        assert!(!key(&mut s, 'f').is_empty());
        // Losing the connection ends a running probe.
        disconnect(&mut s);
        assert!(matches!(preview(&s).probe, ProbeFetch::Failed(_)));
    }

    #[test]
    fn probe_is_only_offered_for_a_single_video() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT, "https://www.youtube.com/watch?v=zzz"]);
        ctrl(&mut s, 's');
        assert!(!preview(&s).probeable());
        assert!(key(&mut s, 'f').is_empty());
    }

    #[test]
    fn add_result_removes_added_video_lines() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT]);
        ctrl(&mut s, 's');
        code(&mut s, KeyCode::Enter);
        update(
            &mut s,
            Msg::ActionResult {
                call: ApiCall::Add(Box::default_params()),
                result: Ok(ApiReply::Added(vec![AddOutcome::Added {
                    id: JobId(9),
                    extra_ids: vec![],
                }])),
            },
        );
        assert!(matches!(s.view(), View::Downloads));
    }

    trait DefaultParams {
        fn default_params() -> Self;
    }
    impl DefaultParams for Box<AddParams> {
        fn default_params() -> Self {
            Box::new(AddParams {
                sources: vec![],
                options: dman_proto::JobOptions::default(),
                paused: false,
            })
        }
    }

    #[test]
    fn video_jobs_are_marked_and_http_jobs_hide_torrent_tabs() {
        let mut s = state();
        let screen = render(&mut s, 80, 24);
        assert!(screen.contains("▶ Big Buck Bunny"), "{screen}");
        // HTTP job: no Peers/Trackers/Pieces.
        open_detail(&mut s, 2);
        let screen = render(&mut s, 80, 24);
        assert!(
            !screen.contains("Peers") && !screen.contains("Pieces"),
            "{screen}"
        );
        assert!(screen.contains("5 Speed"));
        key(&mut s, '2');
        assert!(status_text(&s).contains("only shown for torrents"));
        // Tab skips the hidden ones: Files -> Speed -> Options -> Files.
        code(&mut s, KeyCode::Tab);
        assert_eq!(top_tab(&s), crate::views::detail::Tab::Speed);
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Tab);
        assert_eq!(top_tab(&s), crate::views::detail::Tab::Files);
        // A video job hides them too; a torrent keeps them.
        let mut s = state();
        open_detail(&mut s, 4);
        assert!(!render(&mut s, 80, 24).contains("Peers"));
        let mut s = state();
        open_detail(&mut s, 1);
        assert!(render(&mut s, 80, 24).contains("2 Peers"));
        assert_eq!(s.jobs[&JobId(4)].kind, SourceKind::Video);
    }

    fn top_tab(s: &State) -> crate::views::detail::Tab {
        match s.view() {
            View::Detail(d) => d.tab,
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn daemon_refusals_of_rename_and_options_reach_the_status_line() {
        let mut s = state();
        open_detail(&mut s, 4);
        let call = ApiCall::Rename {
            id: JobId(4),
            name: "x.mp4".into(),
            file_index: None,
        };
        update(
            &mut s,
            Msg::ActionResult {
                call,
                result: Err("video jobs cannot be renamed while downloading".into()),
            },
        );
        assert_eq!(
            status_text(&s),
            "Rename failed: video jobs cannot be renamed while downloading"
        );
        update(
            &mut s,
            Msg::ActionResult {
                call: ApiCall::SetOptions {
                    id: JobId(4),
                    options: Box::default(),
                },
                result: Err("option split does not apply to video jobs".into()),
            },
        );
        assert!(status_text(&s).contains("does not apply to video jobs"));
    }

    // ---------------------------------------------------------- snapshots

    fn snap_form(w: u16, h: u16, f: Field) -> String {
        let mut s = open(Some(engine(true, false, Some("ffmpeg not found in PATH"))));
        lines(&mut s, &[YT, OTHER, "https://example.org/a.iso"]);
        ctrl(&mut s, 'g');
        set(&mut s, Field::VQuality, "1080p");
        set(&mut s, Field::VSubs, "en,de");
        focus(&mut s, f);
        render(&mut s, w, h)
    }

    #[test]
    fn snapshot_add_video_80x24() {
        insta::assert_snapshot!(snap_form(80, 24, Field::VQuality));
    }

    #[test]
    fn snapshot_add_video_120x40() {
        insta::assert_snapshot!(snap_form(120, 40, Field::VQuality));
    }

    #[test]
    fn snapshot_add_video_unavailable_80x24() {
        let mut s = open(Some(engine(false, false, Some("yt-dlp not found in PATH"))));
        lines(&mut s, &[YT]);
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_probe_running_80x24() {
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[YT]);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_probe_plans_and_formats_80x24() {
        let mut s = probed();
        key(&mut s, 'j');
        key(&mut s, ' ');
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }

    #[test]
    fn snapshot_probe_formats_120x40() {
        let mut s = probed();
        code(&mut s, KeyCode::Tab);
        key(&mut s, 'j');
        key(&mut s, ' ');
        insta::assert_snapshot!(render(&mut s, 120, 40));
    }

    #[test]
    fn snapshot_probe_playlist_80x24() {
        let url = "https://www.youtube.com/playlist?list=PL1";
        let mut s = open(Some(engine(true, true, None)));
        lines(&mut s, &[url]);
        ctrl(&mut s, 's');
        key(&mut s, 'f');
        deliver(&mut s, url, false, Ok(playlist_probe()));
        key(&mut s, 'j');
        key(&mut s, ' ');
        insta::assert_snapshot!(render(&mut s, 80, 24));
    }
}
