//! The two-pane Add form, speed windows and custom theme files.

use dman_proto::SpeedSample;

use super::add_view::mixed;
use super::*;
use crate::theme::{ColorDepth, ThemeOptions};
use crate::views::add::{AddState, Focus, Section};
use crate::widgets::graph::{self, Window};

fn add(s: &State) -> &AddState {
    match s.view() {
        View::Add(a) => a,
        v => panic!("not the add view: {v:?}"),
    }
}

fn open_add(w: u16, h: u16) -> State {
    let mut s = state();
    update(&mut s, Msg::Resize(w, h));
    key(&mut s, 'a');
    s
}

fn text_of(s: &mut State, w: u16, h: u16) -> String {
    render(s, w, h)
}

// ------------------------------------------------------------ two-pane add

#[test]
fn two_pane_starts_at_110_columns() {
    assert!(!crate::views::add::two_pane(109));
    assert!(crate::views::add::two_pane(110));
    let s = open_add(110, 30);
    assert!(add(&s).two_pane);
    let s = open_add(109, 30);
    assert!(!add(&s).two_pane);
}

#[test]
fn wide_add_shows_sources_left_and_sections_right() {
    let mut s = mixed();
    let text = text_of(&mut s, 120, 40);
    let line = text
        .lines()
        .find(|l| l.contains("Sources") && l.contains("Options"))
        .expect("both boxes share a row");
    assert!(line.find("Sources") < line.find("Options"), "{line}");
    for want in [
        "▾ Common",
        "▸ Torrent",
        "▸ HTTP",
        "▸ Advanced",
        "[magnet] ✓",
        "✗ not found",
    ] {
        assert!(text.contains(want), "missing {want}:\n{text}");
    }
    assert!(!text.contains("Advanced options"), "stacked row is gone");
}

#[test]
fn narrow_add_keeps_the_stacked_layout() {
    let mut s = mixed();
    let text = text_of(&mut s, 100, 40);
    assert!(text.contains("Advanced options"), "{text}");
    assert!(!text.contains("▸ Torrent"), "{text}");
    assert!(!text.contains("[magnet] ✓"), "{text}");
}

#[test]
fn the_action_line_sits_under_the_sources() {
    let mut s = open_add(120, 40);
    typed(&mut s, "https://example.org/a.iso");
    let text = text_of(&mut s, 120, 40);
    let rows: Vec<&str> = text.lines().collect();
    let bottom = rows.iter().position(|l| l.contains('╰')).unwrap();
    assert!(rows[bottom + 1].contains("Add 1 download"), "{text}");
}

#[test]
fn tab_walks_section_headers_and_space_opens_them() {
    let mut s = open_add(120, 40);
    // Sources, Dir, Paused, DownLimit, then the Torrent header.
    for _ in 0..4 {
        code(&mut s, KeyCode::Tab);
    }
    assert_eq!(add(&s).focus, Focus::Section(Section::Torrent));
    key(&mut s, ' ');
    assert!(add(&s).section_open(Section::Torrent));
    assert!(!add(&s).section_open(Section::Http));
    code(&mut s, KeyCode::Tab);
    assert_eq!(
        add(&s).focus,
        Focus::Field(crate::views::add::options::Field::Seed)
    );
    let text = text_of(&mut s, 120, 40);
    assert!(
        text.contains("▾ Torrent") && text.contains("Seed until ratio"),
        "{text}"
    );
    assert!(text.contains("▸ HTTP"), "{text}");
    // Space on the header again closes it; Left/Right work too.
    press(&mut s, KeyCode::BackTab, KeyModifiers::SHIFT);
    key(&mut s, ' ');
    assert!(!add(&s).section_open(Section::Torrent));
    code(&mut s, KeyCode::Right);
    assert!(add(&s).section_open(Section::Torrent));
    code(&mut s, KeyCode::Left);
    assert!(!add(&s).section_open(Section::Torrent));
}

#[test]
fn f2_opens_and_closes_every_section() {
    let mut s = open_add(120, 40);
    code(&mut s, KeyCode::F(2));
    for sec in Section::ALL {
        assert!(add(&s).section_open(sec), "{sec:?}");
    }
    assert!(s.add_advanced);
    // One section can still be closed while the others stay open.
    if let View::Add(a) = &mut s.views[1] {
        a.toggle_section(Section::Http);
    }
    assert!(!add(&s).section_open(Section::Http));
    assert!(add(&s).section_open(Section::Torrent));
    code(&mut s, KeyCode::F(2));
    for sec in Section::ALL {
        assert!(!add(&s).section_open(sec), "{sec:?}");
    }
    assert!(!s.add_advanced);
}

#[test]
fn enter_still_reviews_from_a_field_and_ctrl_j_adds_lines() {
    let mut s = open_add(120, 40);
    typed(&mut s, "https://example.org/a.iso");
    press(&mut s, KeyCode::Char('j'), KeyModifiers::CONTROL);
    typed(&mut s, "https://example.org/b.iso");
    assert_eq!(add(&s).sources.lines().len(), 2);
    code(&mut s, KeyCode::Tab);
    code(&mut s, KeyCode::Enter);
    assert!(matches!(
        add(&s).overlay,
        Some(crate::views::add::Overlay::Preview(_))
    ));
}

#[test]
fn errors_reveal_hidden_fields() {
    let mut s = open_add(120, 40);
    typed(&mut s, "https://example.org/a.iso");
    // A bad value in a closed section opens it when Enter is pressed.
    code(&mut s, KeyCode::F(2));
    code(&mut s, KeyCode::F(2));
    if let View::Add(a) = &mut s.views[1] {
        a.form
            .editor_mut(crate::views::add::options::Field::Split)
            .unwrap()
            .insert_str("abc");
    }
    code(&mut s, KeyCode::Enter);
    assert_eq!(
        add(&s).focus,
        Focus::Field(crate::views::add::options::Field::Split)
    );
    assert!(add(&s).section_open(Section::Http));
}

#[test]
fn resizing_across_the_breakpoint_keeps_focus_valid() {
    let mut s = open_add(120, 40);
    for _ in 0..4 {
        code(&mut s, KeyCode::Tab);
    }
    assert!(matches!(add(&s).focus, Focus::Section(_)));
    update(&mut s, Msg::Resize(90, 30));
    assert_eq!(add(&s).focus, Focus::Advanced);
    assert!(!add(&s).two_pane);
    update(&mut s, Msg::Resize(130, 30));
    assert_eq!(add(&s).focus, Focus::Section(Section::Advanced));
    // Sections opened on a wide screen show as "advanced" on a narrow one.
    key(&mut s, ' ');
    update(&mut s, Msg::Resize(90, 30));
    assert!(add(&s).advanced);
}

#[test]
fn snapshot_add_two_pane_empty_120x40() {
    let mut s = open_add(120, 40);
    insta::assert_snapshot!(render(&mut s, 120, 40));
}

#[test]
fn snapshot_add_two_pane_open_sections_120x40() {
    let mut s = mixed();
    code(&mut s, KeyCode::F(2));
    for _ in 0..5 {
        code(&mut s, KeyCode::Tab);
    }
    insta::assert_snapshot!(render(&mut s, 120, 40));
}

// ------------------------------------------------------------ speed windows

fn samples(n: i64) -> Vec<SpeedSample> {
    (0..n)
        .map(|i| SpeedSample {
            t: 1_000_000 + i * 1000,
            down: 1000 + (i as u64 % 7) * 500,
            up: 100,
        })
        .collect()
}

#[test]
fn windows_cut_history_and_report_what_is_available() {
    let h = samples(600);
    let (w1, avail) = graph::windowed(&h, Window::Min1);
    assert_eq!(w1.len(), 61);
    assert_eq!(avail, 599);
    assert_eq!(graph::windowed(&h, Window::Min5).0.len(), 301);
    assert_eq!(graph::windowed(&h, Window::Min15).0.len(), 600);
    assert_eq!(graph::windowed(&[], Window::Min5), (&[][..], 0));
    assert_eq!(graph::describe(Window::Min5, 599), "last 5 min");
    assert_eq!(
        graph::describe(Window::Min15, 599),
        "last 15 min (10 min available)"
    );
    assert_eq!(
        graph::describe(Window::Min5, 45),
        "last 5 min (45 s available)"
    );
    assert_eq!(
        graph::describe(Window::Min5, 0),
        "last 5 min (no history yet)"
    );
}

#[test]
fn resampling_keeps_the_length_and_the_mean() {
    let v: Vec<u64> = (0..100).map(|_| 10).collect();
    let r = graph::resample(&v, 20);
    assert_eq!(r.len(), 20);
    assert!(r.iter().all(|x| *x == 10));
    assert_eq!(graph::resample(&v[..5], 20), v[..5].to_vec());
}

#[test]
fn window_keys_in_the_speed_tab() {
    let mut s = state();
    open_detail(&mut s, 1);
    key(&mut s, '5');
    assert_eq!(s.graph.window, Window::Min1);
    key(&mut s, ']');
    assert_eq!(s.graph.window, Window::Min5);
    key(&mut s, ']');
    assert_eq!(s.graph.window, Window::Min15);
    key(&mut s, ']');
    assert_eq!(s.graph.window, Window::Min15, "stops at the longest");
    key(&mut s, '[');
    assert_eq!(s.graph.window, Window::Min5);
    // 1-3 pick a window here instead of switching tabs.
    key(&mut s, '1');
    assert_eq!(s.graph.window, Window::Min1);
    key(&mut s, '3');
    assert_eq!(s.graph.window, Window::Min15);
    key(&mut s, '2');
    assert_eq!(s.graph.window, Window::Min5);
    assert!(matches!(
        s.view(),
        View::Detail(d) if d.tab == views::detail::Tab::Speed
    ));
    // Other tabs keep 1-3 as tab keys and ignore the brackets.
    key(&mut s, '6');
    key(&mut s, ']');
    assert_eq!(s.graph.window, Window::Min5);
    assert!(matches!(
        s.view(),
        View::Detail(d) if d.tab != views::detail::Tab::Speed
    ));
}

#[test]
fn window_keys_in_the_downloads_list() {
    let mut s = state();
    key(&mut s, '}');
    assert_eq!(s.graph.window, Window::Min5);
    key(&mut s, '}');
    key(&mut s, '{');
    assert_eq!(s.graph.window, Window::Min5);
}

#[test]
fn clicking_a_window_label_selects_it() {
    let mut s = state_with(true);
    update(&mut s, Msg::Resize(120, 40));
    update(
        &mut s,
        Msg::Conn(ConnState::Connected {
            daemon_version: "x".into(),
        }),
    );
    s.graph.history = samples(300);
    render(&mut s, 120, 40);
    let (rect, _) = s
        .hits
        .borrow()
        .iter()
        .find(|(_, h)| *h == crate::layout::Hit::SpeedWindow(Window::Min15))
        .cloned()
        .expect("card has window labels");
    update(
        &mut s,
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        }),
    );
    assert_eq!(s.graph.window, Window::Min15);
}

#[test]
fn the_speed_tab_labels_its_window_honestly() {
    let mut s = state();
    open_detail(&mut s, 1);
    key(&mut s, '5');
    key(&mut s, ']');
    // 60 fixture samples = about a minute of data, a 5 min window.
    let text = render(&mut s, 120, 40);
    assert!(text.contains("last 5 min (59 s available)"), "{text}");
    let hits = s
        .hits
        .borrow()
        .iter()
        .filter(|(_, h)| matches!(h, crate::layout::Hit::SpeedWindow(_)))
        .count();
    assert_eq!(hits, 3);
}

#[test]
fn the_throughput_card_labels_its_window() {
    let mut s = state();
    s.graph.history = samples(600);
    key(&mut s, '}');
    key(&mut s, '}');
    let text = render(&mut s, 160, 45);
    assert!(text.contains("15 min"), "{text}");
    assert!(text.contains("last 15 min (10 min available)"), "{text}");
    let text = render(&mut s, 100, 45);
    assert!(text.contains("Throughput"), "{text}");
}

// ------------------------------------------------------------ theme files

fn themed(dir: &std::path::Path, name: &str, file: Option<&str>) -> (Theme, Vec<String>) {
    if let Some(text) = file {
        std::fs::write(dir.join(format!("{name}.toml")), text).unwrap();
    }
    Theme::build_with_warnings(&ThemeOptions {
        name: name.into(),
        themes_dir: Some(dir.to_path_buf()),
        ..ThemeOptions::default()
    })
}

#[test]
fn a_theme_file_inherits_and_overrides() {
    use ratatui::style::Color;
    let dir = tempfile::tempdir().unwrap();
    let (t, w) = themed(
        dir.path(),
        "mine",
        Some("inherits = \"tokyo-night\"\naccent = \"#010203\"\n"),
    );
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(t.accent.fg, Some(Color::Rgb(1, 2, 3)));
    // Everything else comes from the parent.
    let parent = Theme::resolve("tokyo-night", false);
    assert_eq!(t.palette.success, parent.palette.success);
    assert_eq!(t.palette.surface, parent.palette.surface);
}

#[test]
fn ui_colors_win_over_the_theme_file() {
    use ratatui::style::Color;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mine.toml"), "accent = \"#010203\"\n").unwrap();
    let (t, w) = Theme::build_with_warnings(&ThemeOptions {
        name: "mine".into(),
        colors: vec![("accent".into(), "#0a0b0c".into())],
        themes_dir: Some(dir.path().to_path_buf()),
        ..ThemeOptions::default()
    });
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(t.accent.fg, Some(Color::Rgb(10, 11, 12)));
}

#[test]
fn bad_theme_files_warn_and_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let default = Theme::default_theme();
    let (t, w) = themed(dir.path(), "broken", Some("accent = \n"));
    assert_eq!(t.palette, default.palette);
    assert!(w.iter().any(|m| m.contains("broken.toml")), "{w:?}");
    // Unknown names, with and without a directory.
    let (t, w) = themed(dir.path(), "missing", None);
    assert_eq!(t.palette, default.palette);
    assert!(
        w.iter().any(|m| m.contains("unknown theme 'missing'")),
        "{w:?}"
    );
    let (_, w) = Theme::build_with_warnings(&ThemeOptions {
        name: "missing".into(),
        ..ThemeOptions::default()
    });
    assert_eq!(w.len(), 1);
    // A name that tries to leave the directory is never read.
    std::fs::write(dir.path().join("x.toml"), "accent = \"#ffffff\"\n").unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let (t, w) = themed(&sub, "../x", None);
    assert_eq!(t.palette, default.palette);
    assert_eq!(w.len(), 1, "{w:?}");
    // Unknown tokens and colours in a good file are warnings, the rest applies.
    let (t, w) = themed(
        dir.path(),
        "half",
        Some("nope = \"#000000\"\ninfo = \"bogus\"\nwarning = \"#112233\"\n"),
    );
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w.iter().all(|m| m.contains("theme 'half'")), "{w:?}");
    assert_eq!(
        t.palette.warning,
        crate::theme::Tok(crate::theme::Rgb(0x11, 0x22, 0x33), t.palette.warning.1)
    );
}

#[test]
fn theme_files_are_not_read_without_colour() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mine.toml"), "accent = \"#010203\"\n").unwrap();
    let t = Theme::build(&ThemeOptions {
        name: "mine".into(),
        depth: ColorDepth::Mono,
        themes_dir: Some(dir.path().to_path_buf()),
        ..ThemeOptions::default()
    });
    assert_eq!(t.name, "mono");
}

#[test]
fn a_theme_file_name_in_the_options_reaches_the_state() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mine.toml"),
        "inherits = \"high-contrast\"\n",
    )
    .unwrap();
    let opts = crate::TuiOptions {
        theme: "mine".into(),
        color: "truecolor".into(),
        themes_dir: Some(dir.path().to_path_buf()),
        ..crate::TuiOptions::default()
    };
    let s = opts.initial_state_with_env(&|_| None);
    assert_eq!(s.theme.name, "high-contrast");
    let opts = crate::TuiOptions {
        theme: "nope".into(),
        color: "truecolor".into(),
        themes_dir: Some(dir.path().to_path_buf()),
        ..crate::TuiOptions::default()
    };
    let s = opts.initial_state_with_env(&|_| None);
    assert_eq!(s.theme.name, "dman-dark");
}

#[test]
fn detail_summary_mentions_pending_cleanup() {
    let mut s = state();
    open_detail(&mut s, 1);
    if let View::Detail(d) = &mut s.views[1]
        && let Some(det) = d.detail.as_mut()
    {
        det.cleanup_pending = Some(dman_proto::CleanupInfo {
            count: 2,
            bytes: 2048,
        });
    }
    let text = render(&mut s, 120, 40);
    assert!(
        text.contains("2 unselected partial files (2.0 KiB)"),
        "{text}"
    );
    assert!(text.contains("C in the list cleans up"), "{text}");
}
