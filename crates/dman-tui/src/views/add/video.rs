//! Video support of the add view: the `video.probe` overlay state (plans,
//! formats, playlist entries) and its drawing.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent};
use dman_proto::video::{VideoFormat, VideoPlan, VideoPlanKind, VideoStreamKind};
use dman_proto::{VideoOptions, VideoProbe, VideoProbeParams};
use ratatui::text::{Line, Span};

use crate::format::{plural, size, truncate};
use crate::theme::Theme;

/// Spinner frames of the running probe.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Progress of `video.probe` for a single video source.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProbeFetch {
    /// Not requested.
    #[default]
    Idle,
    /// Waiting for the daemon.
    Running {
        /// The URL being probed (answers for another URL are dropped).
        url: String,
        /// Ticks since the start (spinner frame).
        ticks: usize,
        /// Time since the start.
        elapsed: Duration,
    },
    /// The probe arrived.
    Done(Box<ProbeUi>),
    /// The probe failed (or was interrupted).
    Failed(String),
}

/// The list that has the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Quality plans.
    Plans,
    /// Raw formats.
    Formats,
    /// Playlist entries.
    Entries,
}

/// One row of the plan list (an alternative is a row of its own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// The plan.
    pub plan: VideoPlan,
    /// A better option offered next to its primary plan.
    pub alternative: bool,
}

/// What a key did to the probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKey {
    /// Handled (cursor, toggle, section).
    Stay,
    /// Not a probe key.
    Unhandled,
}

/// A probe result and the user's choice from it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeUi {
    /// The daemon's reply.
    pub probe: VideoProbe,
    /// Plans, alternatives flattened in.
    pub plans: Vec<PlanRow>,
    /// Formats worth choosing (storyboards left out).
    pub formats: Vec<VideoFormat>,
    /// Which list has the cursor.
    pub section: Section,
    /// Cursor per list: plans, formats, entries.
    pub cursor: [usize; 3],
    /// Chosen plan row.
    pub plan: Option<usize>,
    /// Chosen format row.
    pub format: Option<usize>,
    /// Playlist entry checkboxes.
    pub checked: Vec<bool>,
}

impl ProbeUi {
    /// Build from the daemon's reply.
    pub fn new(probe: VideoProbe) -> Self {
        let mut plans = Vec::new();
        for p in &probe.plans {
            plans.push(PlanRow {
                plan: p.clone(),
                alternative: false,
            });
            if let Some(alt) = &p.alternative {
                plans.push(PlanRow {
                    plan: (**alt).clone(),
                    alternative: true,
                });
            }
        }
        let formats = probe
            .formats
            .iter()
            .filter(|f| f.kind != VideoStreamKind::Other)
            .cloned()
            .collect();
        let checked = vec![true; probe.entries.len()];
        let section = if probe.playlist && !probe.entries.is_empty() {
            Section::Entries
        } else if plans.is_empty() {
            Section::Formats
        } else {
            Section::Plans
        };
        Self {
            probe,
            plans,
            formats,
            section,
            cursor: [0; 3],
            plan: None,
            format: None,
            checked,
        }
    }

    /// Whether the entries list applies.
    pub fn is_playlist(&self) -> bool {
        self.probe.playlist && !self.probe.entries.is_empty()
    }

    fn sections(&self) -> Vec<Section> {
        let mut s = Vec::new();
        if self.is_playlist() {
            s.push(Section::Entries);
        }
        if !self.plans.is_empty() {
            s.push(Section::Plans);
        }
        if !self.formats.is_empty() {
            s.push(Section::Formats);
        }
        s
    }

    fn len(&self, s: Section) -> usize {
        match s {
            Section::Plans => self.plans.len(),
            Section::Formats => self.formats.len(),
            Section::Entries => self.probe.entries.len(),
        }
    }

    fn slot(s: Section) -> usize {
        match s {
            Section::Plans => 0,
            Section::Formats => 1,
            Section::Entries => 2,
        }
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.len(self.section).saturating_sub(1);
        let c = &mut self.cursor[Self::slot(self.section)];
        *c = c.saturating_add_signed(delta).min(last);
    }

    fn step_section(&mut self, forward: bool) {
        let all = self.sections();
        if all.len() < 2 {
            return;
        }
        let i = all.iter().position(|s| *s == self.section).unwrap_or(0);
        let n = all.len();
        self.section = all[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
    }

    /// Checked playlist entries (1-based indexes).
    pub fn chosen_entries(&self) -> Vec<u32> {
        self.probe
            .entries
            .iter()
            .zip(&self.checked)
            .filter(|(_, c)| **c)
            .map(|(e, _)| e.index)
            .collect()
    }

    /// Why confirming is blocked.
    pub fn blocker(&self) -> Option<&'static str> {
        (self.is_playlist() && self.chosen_entries().is_empty())
            .then_some("select at least one playlist entry")
    }

    /// Handle a navigation or choice key.
    pub fn key(&mut self, key: KeyEvent) -> ProbeKey {
        match key.code {
            KeyCode::Tab => self.step_section(true),
            KeyCode::BackTab => self.step_section(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => self.move_by(1_000_000),
            KeyCode::Char(' ') => self.choose(),
            KeyCode::Char('a') if self.section == Section::Entries => {
                let all = self.checked.iter().all(|c| *c);
                self.checked.iter_mut().for_each(|c| *c = !all);
            }
            _ => return ProbeKey::Unhandled,
        }
        ProbeKey::Stay
    }

    /// Space: pick the row under the cursor (radio for plans and formats,
    /// checkbox for entries).
    fn choose(&mut self) {
        let c = self.cursor[Self::slot(self.section)];
        match self.section {
            Section::Plans => {
                self.plan = if self.plan == Some(c) { None } else { Some(c) };
                self.format = None;
            }
            Section::Formats => {
                self.format = if self.format == Some(c) {
                    None
                } else {
                    Some(c)
                };
                self.plan = None;
            }
            Section::Entries => {
                if let Some(x) = self.checked.get_mut(c) {
                    *x = !*x;
                }
                self.move_by(1);
            }
        }
    }

    /// Apply the choices to `v`.
    pub fn apply(&self, v: &mut VideoOptions) {
        if let Some(r) = self.plan.and_then(|i| self.plans.get(i)) {
            v.quality = Some(r.plan.preset.clone());
            v.format_id = None;
            v.allow_separate_streams = r.plan.kind == VideoPlanKind::Separate;
        } else if let Some(f) = self.format.and_then(|i| self.formats.get(i)) {
            v.format_id = Some(f.id.clone());
            v.quality = None;
        }
        if self.is_playlist() {
            v.playlist = true;
            let chosen = self.chosen_entries();
            v.playlist_items = if chosen.len() == self.probe.entries.len() {
                None
            } else {
                Some(compress(&chosen))
            };
        }
    }

    /// Summary of the current choice.
    fn choice(&self) -> String {
        if let Some(r) = self.plan.and_then(|i| self.plans.get(i)) {
            format!("plan {} ({})", r.plan.preset, kind_label(r.plan.kind))
        } else if let Some(f) = self.format.and_then(|i| self.formats.get(i)) {
            format!("format {} ({} {})", f.id, f.ext, resolution(f))
        } else {
            "nothing (form / daemon defaults apply)".into()
        }
    }

    /// Footer hints.
    pub fn hints(&self) -> Vec<(String, String)> {
        let mut h = vec![("Space".to_owned(), "choose".to_owned())];
        if self.sections().len() > 1 {
            h.push(("Tab".into(), "next list".into()));
        }
        if self.section == Section::Entries {
            h.push(("a".into(), "all/none".into()));
        }
        h
    }

    /// Lines of the result and the cursor line.
    pub fn lines(&self, theme: &Theme, width: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let w = width.saturating_sub(5).max(10);
        let ind = "     ";
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut cursor_line = None;
        let p = &self.probe;
        let mut meta = Vec::new();
        if let Some(u) = &p.uploader {
            meta.push(format!("by {u}"));
        }
        if let Some(d) = p.duration {
            meta.push(duration(d));
        }
        meta.push(if p.ffmpeg {
            "ffmpeg available".into()
        } else {
            "ffmpeg missing".into()
        });
        out.push(Line::styled(
            truncate(&format!("{ind}{}", meta.join(" · ")), width),
            theme.dim,
        ));
        if !p.subtitles.is_empty() || !p.automatic_captions.is_empty() {
            out.push(Line::styled(
                truncate(
                    &format!(
                        "{ind}subtitles: {} · automatic: {}",
                        lang_list(&p.subtitles),
                        lang_list(&p.automatic_captions)
                    ),
                    width,
                ),
                theme.dim,
            ));
        }
        out.push(Line::from(vec![
            Span::raw(ind),
            Span::styled("Selected: ", theme.dim),
            Span::raw(truncate(&self.choice(), w.saturating_sub(10))),
        ]));
        let active = |s: Section| self.section == s;
        let heading = |text: String, on: bool| {
            Line::styled(
                format!("   {text}"),
                if on { theme.accent } else { theme.key },
            )
        };
        if self.is_playlist() {
            out.push(Line::default());
            out.push(heading(
                format!(
                    "Playlist · {} of {} selected",
                    self.chosen_entries().len(),
                    plural(p.entries.len(), "entry").replace("entrys", "entries")
                ),
                active(Section::Entries),
            ));
            for (n, e) in p.entries.iter().enumerate() {
                let on = active(Section::Entries) && n == self.cursor[2];
                let text = truncate(
                    &format!(
                        " {} [{}] {:>3}  {}  {}",
                        if on { '▸' } else { ' ' },
                        if self.checked[n] { 'x' } else { ' ' },
                        e.index,
                        e.title.as_deref().unwrap_or("(untitled)"),
                        e.duration.map(duration).unwrap_or_default()
                    ),
                    width.saturating_sub(2),
                );
                if on {
                    cursor_line = Some(out.len());
                }
                out.push(row(text, on, theme, ind));
            }
        }
        if !self.plans.is_empty() {
            out.push(Line::default());
            out.push(heading("Quality plans".into(), active(Section::Plans)));
            for (n, r) in self.plans.iter().enumerate() {
                let on = active(Section::Plans) && n == self.cursor[0];
                let text = truncate(
                    &format!(
                        " {} ({}) {:<11} {:<9} {}",
                        if on { '▸' } else { ' ' },
                        if self.plan == Some(n) { '•' } else { ' ' },
                        format!("{}{}", if r.alternative { "↳ " } else { "" }, r.plan.preset),
                        kind_label(r.plan.kind),
                        r.plan.explanation
                    ),
                    width.saturating_sub(2),
                );
                if on {
                    cursor_line = Some(out.len());
                }
                out.push(row(text, on, theme, ind));
            }
        }
        if !self.formats.is_empty() {
            out.push(Line::default());
            out.push(heading(
                format!("Formats · {}", self.formats.len()),
                active(Section::Formats),
            ));
            out.push(Line::styled(
                truncate(
                    &format!(
                        "{ind}     {:<8} {:<5} {:<10} {:<4} {:<17} {:>10} {}",
                        "id", "ext", "size", "fps", "vcodec/acodec", "bytes", "kind"
                    ),
                    width,
                ),
                theme.dim,
            ));
            for (n, f) in self.formats.iter().enumerate() {
                let on = active(Section::Formats) && n == self.cursor[1];
                let text = truncate(
                    &format!(
                        " {} ({}) {:<8} {:<5} {:<10} {:<4} {:<17} {:>10} {}",
                        if on { '▸' } else { ' ' },
                        if self.format == Some(n) { '•' } else { ' ' },
                        truncate(&f.id, 8),
                        truncate(&f.ext, 5),
                        resolution(f),
                        f.fps.map_or_else(|| "-".into(), |v| format!("{v:.0}")),
                        truncate(&codecs(f), 17),
                        f.size.map_or_else(|| "-".into(), size),
                        stream_kind(f.kind),
                    ),
                    width.saturating_sub(2),
                );
                if on {
                    cursor_line = Some(out.len());
                }
                out.push(row(text, on, theme, ind));
            }
        }
        (out, cursor_line)
    }
}

fn row(text: String, on: bool, theme: &Theme, ind: &str) -> Line<'static> {
    let text = format!("{}{text}", &ind[..2]);
    if on {
        Line::styled(text, theme.accent)
    } else {
        Line::raw(text)
    }
}

fn lang_list(l: &[String]) -> String {
    match l.len() {
        0 => "none".into(),
        1..=6 => l.join(" "),
        n => format!("{} … ({n})", l[..6].join(" ")),
    }
}

/// `h:mm:ss` or `m:ss`.
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn resolution(f: &VideoFormat) -> String {
    match (f.width, f.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        (None, Some(h)) => format!("{h}p"),
        _ if f.kind == VideoStreamKind::AudioOnly => "audio".into(),
        _ => "-".into(),
    }
}

fn codecs(f: &VideoFormat) -> String {
    let c = |o: &Option<String>| match o.as_deref() {
        None | Some("none") => "-".to_owned(),
        Some(s) => s.split('.').next().unwrap_or(s).to_owned(),
    };
    format!("{}/{}", c(&f.vcodec), c(&f.acodec))
}

fn stream_kind(k: VideoStreamKind) -> &'static str {
    match k {
        VideoStreamKind::Combined => "video+audio",
        VideoStreamKind::VideoOnly => "video",
        VideoStreamKind::AudioOnly => "audio",
        VideoStreamKind::Other => "other",
    }
}

/// Label of a plan kind.
pub fn kind_label(k: VideoPlanKind) -> &'static str {
    match k {
        VideoPlanKind::Merged => "merged",
        VideoPlanKind::Single => "single",
        VideoPlanKind::Separate => "separate",
        VideoPlanKind::Audio => "audio",
    }
}

/// `[1,2,3,7]` → `1-3,7`.
pub fn compress(items: &[u32]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let mut j = i;
        while j + 1 < items.len() && items[j + 1] == items[j] + 1 {
            j += 1;
        }
        out.push(if j > i {
            format!("{}-{}", items[i], items[j])
        } else {
            items[i].to_string()
        });
        i = j + 1;
    }
    out.join(",")
}

/// The `video.probe` request for a single video source.
pub fn probe_params(url: &str, v: &VideoOptions) -> VideoProbeParams {
    VideoProbeParams {
        url: url.to_owned(),
        playlist: v.playlist || v.playlist_items.is_some(),
        cookies_from_browser: v.cookies_from_browser.clone(),
    }
}

/// One-line summary of the options that apply to a video.
pub fn summary(v: &VideoOptions) -> String {
    let mut p = Vec::new();
    if let Some(q) = &v.quality {
        p.push(format!("quality {q}"));
    }
    if let Some(c) = &v.container {
        p.push(format!("container {c}"));
    }
    if let Some(s) = &v.subtitles {
        p.push(format!("subtitles {}", s.langs.join(",")));
    }
    if v.playlist || v.playlist_items.is_some() {
        p.push(match &v.playlist_items {
            Some(i) => format!("playlist items {i}"),
            None => "whole playlist".into(),
        });
    }
    if v.allow_separate_streams {
        p.push("separate streams allowed".into());
    }
    if p.is_empty() {
        "options: daemon defaults".into()
    } else {
        format!("options: {}", p.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_ranges() {
        assert_eq!(compress(&[1, 2, 3, 7]), "1-3,7");
        assert_eq!(compress(&[2]), "2");
        assert_eq!(compress(&[1, 3, 5]), "1,3,5");
        assert_eq!(compress(&[]), "");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(65.0), "1:05");
        assert_eq!(duration(3725.0), "1:02:05");
    }
}
