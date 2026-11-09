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
use crate::fixtures::{job, sample_jobs, sample_limits, sample_stats};
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
    assert!(matches!(s.view(), View::Add(a) if a.text.starts_with("magnet:")));
    code(&mut s, KeyCode::Esc);
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
