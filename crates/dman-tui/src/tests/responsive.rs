//! Every view and overlay rendered at a sweep of terminal sizes: nothing may
//! panic, and the main views keep small-size snapshots.

use super::add_view::{browser_state, mixed, preview_state};
use super::quality_picker::picker_state;
use super::search_view::{
    all_off, mixed_state, open_history, open_providers, open_search, searched,
};
use super::usability::empty_state;
use super::video_view::probed;
use super::*;

/// A named way to reach a screen.
type Scenario = (&'static str, fn() -> State);

fn detail(tab: char) -> State {
    let mut s = state();
    open_detail(&mut s, 1);
    key(&mut s, tab);
    s
}

/// A list whose names are wide (CJK) characters, under a popup.
fn wide_names() -> State {
    let mut s = state();
    let mut jobs = crate::fixtures::sample_jobs();
    for (i, j) in jobs.iter_mut().enumerate() {
        j.name = format!("日本語のファイル名・テスト{i}.mkv");
    }
    update(
        &mut s,
        Msg::Event(Event::Snapshot {
            jobs,
            stats: sample_stats(),
            limits: sample_limits(),
        }),
    );
    s
}

fn scenarios() -> Vec<Scenario> {
    vec![
        ("wide_names_palette", || {
            let mut s = wide_names();
            key(&mut s, ':');
            s
        }),
        ("wide_names_confirm", || {
            let mut s = wide_names();
            key(&mut s, 'D');
            s
        }),
        ("wide_names_toast_whichkey", || {
            let mut s = wide_names();
            super::redesign::removed(&mut s, vec![JobId(6)], false);
            key(&mut s, 's');
            s
        }),
        ("downloads", || {
            let mut s = state();
            key(&mut s, 'j');
            key(&mut s, ' ');
            s
        }),
        ("downloads_graph", || {
            let mut s = state();
            key(&mut s, 'w');
            s
        }),
        ("downloads_empty", || empty_state(false, None).0),
        ("welcome", || empty_state(true, None).0),
        ("disconnected", || {
            let mut s = state();
            disconnect(&mut s);
            s
        }),
        ("toasts_whichkey", || {
            let mut s = state();
            super::redesign::removed(&mut s, vec![JobId(6)], false);
            key(&mut s, 's');
            s
        }),
        ("filter", || {
            let mut s = state();
            key(&mut s, '/');
            typed(&mut s, "iso");
            s
        }),
        ("palette", || {
            let mut s = state();
            key(&mut s, ':');
            s
        }),
        ("palette_query", || {
            let mut s = state();
            key(&mut s, ':');
            typed(&mut s, "pause");
            s
        }),
        ("palette_group", || {
            let mut s = state();
            key(&mut s, ':');
            code(&mut s, KeyCode::Tab);
            code(&mut s, KeyCode::Tab);
            s
        }),
        ("delete_confirm", || {
            let mut s = state();
            key(&mut s, 'D');
            s
        }),
        ("remove_many_confirm", || {
            let mut s = state();
            press(&mut s, KeyCode::Char('a'), KeyModifiers::CONTROL);
            key(&mut s, 'd');
            s
        }),
        ("rename_prompt", || {
            let mut s = state();
            s.list.cursor = Some(JobId(5));
            key(&mut s, 'n');
            s
        }),
        ("shutdown_confirm", || {
            let mut s = state();
            key(&mut s, 'Q');
            s
        }),
        ("help", || {
            let mut s = state();
            key(&mut s, '?');
            s
        }),
        ("help_search", || {
            let mut s = state();
            key(&mut s, '?');
            key(&mut s, '/');
            typed(&mut s, "pause");
            s
        }),
        ("detail_files", || detail('1')),
        ("detail_peers", || detail('2')),
        ("detail_trackers", || detail('3')),
        ("detail_pieces", || detail('4')),
        ("detail_speed", || detail('5')),
        ("detail_options", || detail('6')),
        ("detail_info", || detail('7')),
        ("detail_http", || {
            let mut s = state();
            open_detail(&mut s, 2);
            s
        }),
        ("add_mixed", mixed),
        ("add_empty", || {
            let mut s = state();
            key(&mut s, 'a');
            s
        }),
        ("add_review", preview_state),
        ("add_file_browser", browser_state),
        ("add_folder_picker", || {
            use dman_proto::method::{FsEntry, FsListParams, FsListing};
            let mut s = state();
            key(&mut s, 'a');
            press(&mut s, KeyCode::Char('f'), KeyModifiers::CONTROL);
            let entries = (0..30)
                .map(|i| FsEntry {
                    name: format!("folder-{i:02}-with-a-fairly-long-name"),
                    is_dir: true,
                    size: None,
                })
                .collect();
            update(
                &mut s,
                Msg::ActionResult {
                    call: ApiCall::FsList(FsListParams::default()),
                    result: Ok(ApiReply::Listing(FsListing {
                        path: "/srv/dl/very/deep/path/that/keeps/going/and/going".into(),
                        parent: Some("/srv".into()),
                        entries,
                        truncated: false,
                    })),
                },
            );
            s
        }),
        ("add_probe", probed),
        ("add_quality_picker", picker_state),
        ("search_results", mixed_state),
        ("search_done", searched),
        ("search_history", || {
            let mut s = searched();
            open_history(&mut s);
            s
        }),
        ("search_first_run", || {
            let mut s = state();
            open_search(&mut s, all_off());
            s
        }),
        ("providers", open_providers),
        ("palette_over_detail", || {
            let mut s = detail('1');
            key(&mut s, ':');
            s
        }),
    ]
}

fn render_buffer(s: &mut State, w: u16, h: u16) -> ratatui::buffer::Buffer {
    update(s, Msg::Resize(w, h));
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    // The frame as drawn (not what the terminal shows after the diff, where
    // a hidden cell is already lost).
    terminal
        .draw(|f| views::render(s, f))
        .expect("draw succeeds")
        .buffer
        .clone()
}

/// A wide character covers the next cell on a real terminal: that cell must
/// not hold anything else (e.g. a popup border drawn over half a character).
fn assert_no_split_wide_chars(buf: &ratatui::buffer::Buffer, name: &str, w: u16, h: u16) {
    for y in 0..h {
        for x in 0..w.saturating_sub(1) {
            if crate::format::width(buf[(x, y)].symbol()) > 1 {
                let next = buf[(x + 1, y)].symbol();
                assert!(
                    next.is_empty() || next == " ",
                    "{name} at {w}×{h}: '{}' at ({x}, {y}) hides '{next}'",
                    buf[(x, y)].symbol()
                );
            }
        }
    }
}

/// Widths 20..=220 step 7 and heights 6..=60 step 5, plus a few named
/// extremes (very wide and short, narrow and tall, 1×1).
fn sizes() -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for w in (20..=220).step_by(7) {
        for h in (6..=60).step_by(5) {
            out.push((w, h));
        }
    }
    out.extend([(200, 12), (40, 60), (1, 1), (0, 0), (2, 40), (300, 3)]);
    out
}

#[test]
fn every_screen_renders_at_every_size() {
    let sizes = sizes();
    // One thread per scenario: thousands of debug-build frames otherwise
    // take minutes.
    std::thread::scope(|scope| {
        for (name, build) in scenarios() {
            let sizes = &sizes;
            scope.spawn(move || {
                let base = build();
                for &(w, h) in sizes {
                    let mut s = base.clone();
                    let buf = render_buffer(&mut s, w, h);
                    assert_no_split_wide_chars(&buf, name, w, h);
                }
            });
        }
    });
}

/// The main views at small sizes, as snapshots.
#[test]
fn small_size_snapshots() {
    const MAIN: &[&str] = &[
        "downloads",
        "downloads_empty",
        "welcome",
        "palette",
        "palette_query",
        "delete_confirm",
        "help",
        "detail_files",
        "detail_options",
        "add_mixed",
        "add_quality_picker",
        "search_results",
        "providers",
    ];
    for (name, build) in scenarios() {
        if !MAIN.contains(&name) {
            continue;
        }
        for (w, h) in [(60, 18), (40, 12)] {
            let mut s = build();
            insta::assert_snapshot!(format!("{name}_{w}x{h}"), render(&mut s, w, h));
        }
    }
}

/// Print screens for manual inspection:
/// `DMAN_DUMP=palette@60x18,help@40x12 cargo test -p dman-tui dump -- --ignored --nocapture`.
#[test]
#[ignore = "manual inspection helper"]
fn dump() {
    let spec = std::env::var("DMAN_DUMP").unwrap_or_default();
    let all = scenarios();
    for item in spec.split(',').filter(|s| !s.is_empty()) {
        let (name, size) = item.split_once('@').unwrap_or((item, "80x24"));
        let (w, h) = size.split_once('x').unwrap_or(("80", "24"));
        let (w, h): (u16, u16) = (w.parse().unwrap_or(80), h.parse().unwrap_or(24));
        for (n, build) in &all {
            if *n == name || name == "all" {
                let mut s = build();
                let text = render(&mut s, w, h);
                println!("===== {n} @ {w}x{h}");
                for line in text.lines() {
                    println!("{line:<width$}|", width = usize::from(w));
                }
            }
        }
    }
}

/// Shrinking then growing a live state (not a fresh clone) keeps working:
/// cursors and scroll offsets computed at one size stay valid at the next.
#[test]
fn resizing_a_live_state_keeps_working() {
    for (name, build) in scenarios() {
        let mut s = build();
        for &(w, h) in &[(120, 40), (60, 18), (30, 10), (200, 12), (40, 60), (80, 24)] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render(&mut s, w, h);
                code(&mut s, KeyCode::Down);
                code(&mut s, KeyCode::End);
                render(&mut s, w, h);
            }));
            assert!(result.is_ok(), "{name} panicked at {w}×{h}");
        }
    }
}
