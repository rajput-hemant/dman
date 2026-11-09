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
    key(&mut s, '2');
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
