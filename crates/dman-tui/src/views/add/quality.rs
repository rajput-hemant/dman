//! The video quality picker: the flow that runs when a video source is
//! confirmed (preferences → probe → picker → add) and the picker popup.
//!
//! The flow is documented in `dman_proto::video`. Several video sources are
//! decided one after the other (each gets its own probe and picker, titled
//! "n of m"); ticking "never ask again" for one skips the pickers of the
//! remaining ones. The request is sent once, with every decision applied.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent};
use dman_proto::video::{QualityChoice, QualityChoiceKind};
use dman_proto::{AddParams, AddSource, VideoOptions, VideoPreferencesUpdate, VideoProbe};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::preview::Preview;
use super::video::{self, ProbeFetch, ProbeUi};
use super::{Overlay, add_any};
use crate::api::{ApiCall, ApiReply, NEVER_ASK_HINT, PrefsIntent};
use crate::app::{Cmd, State};
use crate::format::{self, size, truncate};
use crate::theme::Theme;

/// Presets of the quick row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Highest quality.
    Best,
    /// At most 1080p.
    P1080,
    /// At most 720p.
    P720,
    /// At most 480p.
    P480,
    /// No video.
    AudioOnly,
}

impl Preset {
    /// Row order.
    pub const ALL: [Preset; 5] = [
        Preset::Best,
        Preset::P1080,
        Preset::P720,
        Preset::P480,
        Preset::AudioOnly,
    ];

    /// Label.
    pub const fn label(self) -> &'static str {
        match self {
            Preset::Best => "Best",
            Preset::P1080 => "1080p",
            Preset::P720 => "720p",
            Preset::P480 => "480p",
            Preset::AudioOnly => "Audio only",
        }
    }

    const fn max_height(self) -> Option<u32> {
        match self {
            Preset::P1080 => Some(1080),
            Preset::P720 => Some(720),
            Preset::P480 => Some(480),
            Preset::Best | Preset::AudioOnly => None,
        }
    }
}

/// The part of the picker that has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Video list.
    Video,
    /// Audio list.
    Audio,
    /// Quick presets.
    Presets,
    /// "Use as my default" checkbox.
    UseDefault,
    /// "Never ask again" checkbox.
    NeverAsk,
}

const FOCUS_ORDER: [Focus; 5] = [
    Focus::Video,
    Focus::Audio,
    Focus::Presets,
    Focus::UseDefault,
    Focus::NeverAsk,
];

/// What a key did to the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKey {
    /// Handled (or ignored).
    Stay,
    /// Esc: cancel the add.
    Cancel,
    /// Enter with a valid selection.
    Download,
}

/// What the current selection downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// `VideoOptions::format_id`.
    pub format_id: String,
    /// Height of the chosen video (none for audio only).
    pub height: Option<u32>,
    /// Codec of the audio that is part of the download, when it was chosen.
    pub audio_codec: Option<String>,
    /// Audio only.
    pub audio_only: bool,
}

/// `AAC` → `m4a`, `Opus` → `opus`, `MP3` → `mp3`.
pub fn audio_family(codec: Option<&str>) -> Option<&'static str> {
    let c = codec?.to_ascii_lowercase();
    if c.contains("aac") || c.contains("mp4a") || c.contains("m4a") {
        Some("m4a")
    } else if c.contains("opus") {
        Some("opus")
    } else if c.contains("mp3") {
        Some("mp3")
    } else {
        None
    }
}

/// `1080` → `1080p` for the presets the daemon knows, otherwise `best`.
pub fn preset_for_height(height: Option<u32>) -> String {
    match height {
        Some(h @ (2160 | 1440 | 1080 | 720 | 480)) => format!("{h}p"),
        _ => "best".to_owned(),
    }
}

/// The picker popup state.
#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    /// The probe the lists come from.
    pub probe: VideoProbe,
    /// Title shown in the border.
    pub title: String,
    /// `(n, of)` when several videos are decided in a row.
    pub position: (usize, usize),
    /// Part with the keyboard.
    pub focus: Focus,
    /// Cursor (= selection) in the video list; `video_choices.len()` is the
    /// "audio only" row.
    pub video: usize,
    /// Cursor (= selection) in the audio list.
    pub audio: usize,
    /// Cursor of the presets row.
    pub preset: usize,
    /// The audio list was used by hand (the default audio of video-only
    /// choices no longer follows the video cursor).
    pub audio_touched: bool,
    /// "Use as my default".
    pub use_default: bool,
    /// "Never ask again".
    pub never_ask: bool,
    /// Why Enter did nothing.
    pub message: Option<String>,
}

impl Picker {
    /// Build from the probe, preselecting `probe.suggested`.
    pub fn new(probe: VideoProbe, position: (usize, usize)) -> Self {
        let title = probe.title.clone().unwrap_or_else(|| probe.url.clone());
        let mut p = Self {
            probe,
            title,
            position,
            focus: Focus::Video,
            video: 0,
            audio: 0,
            preset: 0,
            audio_touched: false,
            use_default: false,
            never_ask: false,
            message: None,
        };
        let s = p.probe.suggested.clone();
        let video = s
            .video
            .as_deref()
            .and_then(|id| p.probe.video_choices.iter().position(|c| c.id == id));
        p.video = match video {
            Some(i) => i,
            None if s.audio.is_some() && p.can_audio_only() => p.probe.video_choices.len(),
            None => p
                .probe
                .video_choices
                .iter()
                .position(|c| c.available)
                .unwrap_or(0),
        };
        p.audio = s
            .audio
            .as_deref()
            .and_then(|id| p.probe.audio_choices.iter().position(|c| c.id == id))
            .or_else(|| p.default_audio_for(p.video))
            .unwrap_or(0);
        p
    }

    fn can_audio_only(&self) -> bool {
        !self.probe.audio_choices.is_empty()
    }

    /// Rows of the video list (choices and the audio-only row).
    pub fn video_rows(&self) -> usize {
        self.probe.video_choices.len() + usize::from(self.can_audio_only())
    }

    fn video_choice(&self) -> Option<&QualityChoice> {
        self.probe.video_choices.get(self.video)
    }

    /// The audio list applies to the selected video row.
    pub fn audio_applies(&self) -> bool {
        match self.video_choice() {
            Some(c) => c.kind == QualityChoiceKind::VideoOnly && self.can_audio_only(),
            None => self.can_audio_only(),
        }
    }

    /// The audio that comes with video choice `vidx` by itself (`VID+AUD`),
    /// else the suggested one, else the best.
    fn default_audio_for(&self, vidx: usize) -> Option<usize> {
        let by_id = |id: &str| self.probe.audio_choices.iter().position(|a| a.id == id);
        self.probe
            .video_choices
            .get(vidx)
            .and_then(|c| c.id.split_once('+'))
            .and_then(|(_, a)| by_id(a))
            .or_else(|| self.probe.suggested.audio.as_deref().and_then(by_id))
            .or((!self.probe.audio_choices.is_empty()).then_some(0))
    }

    fn after_video_moved(&mut self) {
        if !self.audio_touched
            && let Some(a) = self.default_audio_for(self.video)
        {
            self.audio = a;
        }
        self.message = None;
    }

    /// Move the preset cursor to what the selection currently is (if any).
    fn select_video(&mut self, idx: usize) {
        self.video = idx.min(self.video_rows().saturating_sub(1));
        self.after_video_moved();
    }

    /// Apply a quick preset to the lists.
    pub fn apply_preset(&mut self, preset: Preset) {
        if let Some(i) = Preset::ALL.iter().position(|p| *p == preset) {
            self.preset = i;
        }
        let choices = &self.probe.video_choices;
        let idx = match preset {
            Preset::AudioOnly if self.can_audio_only() => Some(choices.len()),
            Preset::AudioOnly => None,
            _ => {
                let limit = preset.max_height();
                // Best first: the first available choice within the limit,
                // else the lowest available one.
                choices
                    .iter()
                    .position(|c| c.available && limit.is_none_or(|l| c.height.unwrap_or(0) <= l))
                    .or_else(|| choices.iter().rposition(|c| c.available))
                    .or_else(|| self.can_audio_only().then_some(choices.len()))
            }
        };
        if let Some(i) = idx {
            self.select_video(i);
        }
    }

    fn move_list(&mut self, delta: isize) {
        match self.focus {
            Focus::Video => {
                let last = self.video_rows().saturating_sub(1);
                self.video = self.video.saturating_add_signed(delta).min(last);
                self.after_video_moved();
            }
            Focus::Audio => {
                let last = self.probe.audio_choices.len().saturating_sub(1);
                self.audio = self.audio.saturating_add_signed(delta).min(last);
                self.audio_touched = true;
                self.message = None;
            }
            _ => {}
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let i = FOCUS_ORDER
            .iter()
            .position(|f| *f == self.focus)
            .unwrap_or(0);
        let n = FOCUS_ORDER.len();
        self.focus = FOCUS_ORDER[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
    }

    fn step_preset(&mut self, delta: isize) {
        let n = Preset::ALL.len();
        let i = self.preset.saturating_add_signed(delta).min(n - 1);
        self.apply_preset(Preset::ALL[i]);
    }

    /// What Enter would download, or why it cannot.
    ///
    /// # Errors
    /// The reason the selection cannot be downloaded.
    pub fn chosen(&self) -> Result<Chosen, String> {
        let unavailable = |c: &QualityChoice| {
            format!(
                "{} is not available: {}",
                c.label,
                c.unavailable_reason.as_deref().unwrap_or("needs ffmpeg")
            )
        };
        if let Some(c) = self.video_choice() {
            if !c.available {
                return Err(unavailable(c));
            }
            let audio = self
                .audio_applies()
                .then(|| self.probe.audio_choices.get(self.audio))
                .flatten();
            let format_id = match audio {
                Some(a) if !a.available => return Err(unavailable(a)),
                Some(a) if c.id.split_once('+').is_some_and(|(_, id)| id == a.id) => c.id.clone(),
                Some(a) => format!("{}+{}", c.stream_id, a.id),
                None => c.id.clone(),
            };
            return Ok(Chosen {
                format_id,
                height: c.height,
                audio_codec: audio.and_then(|a| a.codec.clone()),
                audio_only: false,
            });
        }
        let a = self
            .probe
            .audio_choices
            .get(self.audio)
            .ok_or_else(|| "nothing to download here".to_owned())?;
        if !a.available {
            return Err(unavailable(a));
        }
        Ok(Chosen {
            format_id: a.id.clone(),
            height: None,
            audio_codec: a.codec.clone(),
            audio_only: true,
        })
    }

    /// The `video.preferences.set` payload for the ticked checkboxes.
    pub fn prefs_update(&self, chosen: &Chosen) -> Option<VideoPreferencesUpdate> {
        let mut u = VideoPreferencesUpdate::default();
        if self.never_ask {
            u.ask_quality = Some(false);
        }
        if self.use_default {
            let family = audio_family(chosen.audio_codec.as_deref());
            if chosen.audio_only {
                u.default_quality = Some(format!("audio:{}", family.unwrap_or("m4a")));
                u.default_audio = Some(family.unwrap_or("best").to_owned());
            } else {
                u.default_quality = Some(preset_for_height(chosen.height));
                if chosen.audio_codec.is_some() {
                    u.default_audio = Some(family.unwrap_or("best").to_owned());
                }
            }
        }
        (u != VideoPreferencesUpdate::default()).then_some(u)
    }

    /// Handle a key.
    pub fn key(&mut self, key: KeyEvent) -> PickerKey {
        match key.code {
            KeyCode::Esc => return PickerKey::Cancel,
            KeyCode::Enter => {
                return match self.chosen() {
                    Ok(_) => PickerKey::Download,
                    Err(e) => {
                        self.message = Some(e);
                        PickerKey::Stay
                    }
                };
            }
            KeyCode::Tab => self.step_focus(true),
            KeyCode::BackTab => self.step_focus(false),
            KeyCode::Char('u') => self.use_default = !self.use_default,
            KeyCode::Char('n') => self.never_ask = !self.never_ask,
            KeyCode::Char(c @ '1'..='5') => {
                let i = usize::from(c as u8 - b'1');
                self.apply_preset(Preset::ALL[i]);
            }
            KeyCode::Char(' ') => match self.focus {
                Focus::UseDefault => self.use_default = !self.use_default,
                Focus::NeverAsk => self.never_ask = !self.never_ask,
                Focus::Presets => self.apply_preset(Preset::ALL[self.preset]),
                Focus::Video | Focus::Audio => {}
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Presets => self.step_preset(1),
                _ => self.move_list(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Presets => self.step_preset(-1),
                _ => self.move_list(-1),
            },
            KeyCode::Right | KeyCode::Char('l') => match self.focus {
                Focus::Video => self.focus = Focus::Audio,
                Focus::Presets => self.step_preset(1),
                _ => {}
            },
            KeyCode::Left | KeyCode::Char('h') => match self.focus {
                Focus::Audio => self.focus = Focus::Video,
                Focus::Presets => self.step_preset(-1),
                _ => {}
            },
            KeyCode::PageDown => self.move_list(8),
            KeyCode::PageUp => self.move_list(-8),
            KeyCode::Home | KeyCode::Char('g') => self.move_list(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => self.move_list(1_000_000),
            _ => {}
        }
        PickerKey::Stay
    }

    /// Footer hints.
    pub fn hints() -> Vec<(String, String)> {
        [
            ("Enter", "download"),
            ("Esc", "cancel"),
            ("Tab", "next list"),
            ("↑↓", "move"),
            ("1-5", "preset"),
            ("u", "my default"),
            ("n", "never ask"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
    }
}

fn badge(c: &QualityChoice) -> &'static str {
    if c.requires_ffmpeg {
        "  needs ffmpeg"
    } else {
        ""
    }
}

/// One row of a choice list.
fn choice_row(
    c: &QualityChoice,
    on: bool,
    active: bool,
    theme: &Theme,
    width: usize,
) -> Line<'static> {
    let mut label = c.label.clone();
    if !label.contains('~')
        && let Some(s) = c.size_estimate
    {
        label.push_str(&format!(" · ~{}", size(s)));
    }
    let mut tail = badge(c).to_owned();
    if !c.available {
        let why = c.unavailable_reason.as_deref().unwrap_or("not available");
        tail = format!("  ({why})");
    }
    let marker = if on { "▸ " } else { "  " };
    let room = width.saturating_sub(2 + tail.chars().count());
    let text = format!("{marker}{}{tail}", truncate(&label, room));
    let style = if !c.available {
        theme.dim
    } else if on && active {
        theme.accent
    } else if on {
        theme.key
    } else {
        theme.text
    };
    Line::styled(truncate(&text, width), style)
}

/// Rows `top..` that keep `cursor` visible in `height` rows.
fn window(cursor: usize, height: usize) -> usize {
    if height == 0 || cursor < height {
        0
    } else {
        cursor + 1 - height
    }
}

impl Picker {
    fn render_video(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let active = self.focus == Focus::Video;
        let head_style = if active { theme.accent } else { theme.key };
        let h = usize::from(area.height).saturating_sub(1);
        let w = usize::from(area.width);
        let mut lines = vec![Line::styled(
            format!(" Video · {}", self.probe.video_choices.len()),
            head_style,
        )];
        let rows = self.video_rows();
        let top = window(self.video, h);
        for i in (top..rows).take(h) {
            lines.push(match self.probe.video_choices.get(i) {
                Some(c) => choice_row(c, i == self.video, active, theme, w),
                None => {
                    let on = i == self.video;
                    let text = format!("{}Audio only (no video)", if on { "▸ " } else { "  " });
                    Line::styled(
                        text,
                        if on && active {
                            theme.accent
                        } else if on {
                            theme.key
                        } else {
                            theme.text
                        },
                    )
                }
            });
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_audio(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let active = self.focus == Focus::Audio;
        let applies = self.audio_applies();
        let head_style = if active { theme.accent } else { theme.key };
        let h = usize::from(area.height).saturating_sub(1);
        let w = usize::from(area.width);
        let note = if applies {
            String::new()
        } else if self.probe.audio_choices.is_empty() {
            " · none".to_owned()
        } else {
            " · included in the video".to_owned()
        };
        let mut lines = vec![Line::styled(
            format!(" Audio · {}{note}", self.probe.audio_choices.len()),
            head_style,
        )];
        let top = window(self.audio, h);
        for (i, c) in self
            .probe
            .audio_choices
            .iter()
            .enumerate()
            .skip(top)
            .take(h)
        {
            if applies {
                lines.push(choice_row(c, i == self.audio, active, theme, w));
            } else {
                lines.push(Line::styled(
                    truncate(&format!("  {}", c.label), w),
                    theme.dim,
                ));
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    /// Draw the picker inside `inner`.
    fn render_body(&self, frame: &mut Frame<'_>, inner: Rect, theme: &Theme) {
        // On short terminals the lists keep their rows: the details line,
        // the checkboxes (u / n still work) and the presets (1-5) give way.
        let h = inner.height;
        let one = |show: bool| u16::from(show);
        let (meta_h, boxes_h, presets_h) = (one(h >= 14), one(h >= 12), one(h >= 9));
        let mut y = inner.y;
        let mut take = |height: u16| {
            let r = Rect {
                y,
                height: height.min(inner.bottom().saturating_sub(y)),
                ..inner
            };
            y = r.bottom();
            r
        };
        let meta = take(meta_h);
        let presets = take(presets_h);
        let lists = take(h.saturating_sub(meta_h + presets_h + boxes_h + 2));
        let boxes = take(boxes_h);
        let summary = take(1);
        let hints = take(1);
        let w = usize::from(inner.width);
        let p = &self.probe;
        let mut m = Vec::new();
        if let Some(u) = &p.uploader {
            m.push(format!("by {u}"));
        }
        if let Some(d) = p.duration {
            m.push(video::duration(d));
        }
        m.push(if p.ffmpeg {
            "ffmpeg available".into()
        } else {
            "ffmpeg missing".into()
        });
        frame.render_widget(
            Paragraph::new(Line::styled(
                truncate(&format!(" {}", m.join(" · ")), w),
                theme.dim,
            )),
            meta,
        );

        let mut spans = vec![Span::styled(
            " Presets ",
            if self.focus == Focus::Presets {
                theme.accent
            } else {
                theme.key
            },
        )];
        for (i, pr) in Preset::ALL.iter().enumerate() {
            let on = self.focus == Focus::Presets && i == self.preset;
            spans.push(Span::styled(
                format!(" {} {} ", i + 1, pr.label()),
                if on { theme.cursor } else { theme.text },
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), presets);

        if inner.width >= 90 {
            let [l, r] =
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(lists);
            self.render_video(frame, l, theme);
            self.render_audio(frame, r, theme);
        } else {
            // The video list takes what it needs, keeping a few rows for audio.
            let need = u16::try_from(1 + self.video_rows()).unwrap_or(u16::MAX);
            let audio_min = u16::try_from(1 + self.probe.audio_choices.len())
                .unwrap_or(u16::MAX)
                .min(4);
            let top = need.min(lists.height.saturating_sub(audio_min));
            let [t, b] =
                Layout::vertical([Constraint::Length(top), Constraint::Fill(1)]).areas(lists);
            self.render_video(frame, t, theme);
            self.render_audio(frame, b, theme);
        }

        let check = |on: bool, focus: Focus, key: char, text: &str| {
            let style = if self.focus == focus {
                theme.accent
            } else {
                theme.text
            };
            vec![
                Span::styled(format!(" [{}] {text} ", if on { 'x' } else { ' ' }), style),
                Span::styled(format!("({key})  "), theme.dim),
            ]
        };
        let mut spans = check(
            self.use_default,
            Focus::UseDefault,
            'u',
            "Use as my default",
        );
        spans.extend(check(
            self.never_ask,
            Focus::NeverAsk,
            'n',
            "Never ask again",
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), boxes);

        let line = match (self.message.clone(), self.chosen()) {
            (Some(msg), _) | (None, Err(msg)) => {
                Line::styled(truncate(&format!(" {msg}"), w), theme.error)
            }
            (None, Ok(c)) => Line::from(vec![
                Span::styled(" Download ", theme.dim),
                Span::raw(truncate(&self.summary(&c), w.saturating_sub(10))),
            ]),
        };
        frame.render_widget(Paragraph::new(line), summary);

        let mut h = Vec::new();
        let mut used = 0;
        for (k, v) in Self::hints() {
            let item_w = format::width(&k) + format::width(&v) + 2;
            if used + item_w > w {
                break;
            }
            used += item_w;
            h.push(Span::styled(format!(" {k}"), theme.key));
            h.push(Span::raw(format!(" {v}")));
        }
        frame.render_widget(Paragraph::new(Line::from(h)), hints);
    }

    fn summary(&self, c: &Chosen) -> String {
        if c.audio_only {
            let a = self.probe.audio_choices.get(self.audio);
            return format!("audio only: {}", a.map_or("", |a| a.label.as_str()));
        }
        let v = self.video_choice().map_or("", |v| v.label.as_str());
        match (
            self.audio_applies(),
            self.probe.audio_choices.get(self.audio),
        ) {
            (true, Some(a)) => format!("{v} + {}", a.label),
            _ => v.to_owned(),
        }
    }

    /// Draw the popup.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let width = area.width.saturating_sub(4).min(104);
        let (v, a) = (self.video_rows(), self.probe.audio_choices.len());
        // Borders, five fixed rows, and the list headings and rows.
        let lists = if width.saturating_sub(2) >= 90 {
            1 + v.max(a)
        } else {
            2 + v + a
        };
        let want = u16::try_from(7 + lists).unwrap_or(u16::MAX);
        let rect = crate::widgets::popup_rect(area, width, want);
        frame.render_widget(Clear, rect);
        let of = if self.position.1 > 1 {
            format!(" ({} of {})", self.position.0, self.position.1)
        } else {
            String::new()
        };
        let title = truncate(
            &format!(" Choose quality · {}{of} ", self.title),
            usize::from(rect.width).saturating_sub(4),
        );
        let inner = crate::widgets::modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(title, theme.accent.patch(theme.surface_bg()))),
            theme.border_focus,
        );
        self.render_body(frame, inner, theme);
    }
}

/// Where the flow is.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Waiting for the daemon: the preferences (`url` unset) or a probe.
    Waiting {
        /// The URL being probed.
        url: Option<String>,
        /// Ticks since the start (spinner frame).
        ticks: usize,
        /// Time since the start.
        elapsed: Duration,
    },
    /// The picker is open.
    Picker(Box<Picker>),
    /// Something failed.
    Failed {
        /// What.
        message: String,
        /// It was the preferences that failed (not a probe).
        at_prefs: bool,
    },
}

/// The quality flow of a confirmed review.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityFlow {
    /// The review the user confirmed (back to it for playlists / offline).
    pub preview: Box<Preview>,
    /// The request, with the decisions applied as they are made.
    pub params: AddParams,
    /// Indexes into `params.sources` of the videos to decide.
    pub targets: Vec<usize>,
    /// Next target.
    pub pos: usize,
    /// Where it is.
    pub stage: Stage,
    /// "Never ask again" was ticked: no more pickers for this request.
    pub skip_picker: bool,
}

fn undecided(v: &VideoOptions) -> bool {
    v.quality.is_none()
        && v.format_id.is_none()
        && v.audio_only.is_none()
        && !v.playlist
        && v.playlist_items.is_none()
}

impl QualityFlow {
    /// The flow for a confirmed review, or `None` when nothing needs
    /// deciding (no video source, a manual probe was done, or the form already
    /// names a quality / format / playlist).
    pub fn new(p: &Preview) -> Option<Self> {
        if !matches!(p.probe, ProbeFetch::Idle | ProbeFetch::Failed(_)) {
            return None;
        }
        let params = p.confirm_params();
        let targets: Vec<usize> = params
            .sources
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, AddSource::Video { video, .. } if undecided(video)))
            .map(|(i, _)| i)
            .collect();
        (!targets.is_empty()).then(|| Self {
            preview: Box::new(p.clone()),
            params,
            targets,
            pos: 0,
            stage: Stage::Waiting {
                url: None,
                ticks: 0,
                elapsed: Duration::ZERO,
            },
            skip_picker: false,
        })
    }

    fn target_source(&mut self) -> Option<(&str, &mut VideoOptions)> {
        let i = *self.targets.get(self.pos)?;
        match self.params.sources.get_mut(i)? {
            AddSource::Video { url, video } => Some((url.as_str(), &mut **video)),
            _ => None,
        }
    }

    fn waiting(url: Option<String>) -> Stage {
        Stage::Waiting {
            url,
            ticks: 0,
            elapsed: Duration::ZERO,
        }
    }

    /// Footer hints.
    pub fn hints(&self) -> Vec<(String, String)> {
        match &self.stage {
            Stage::Waiting { .. } => vec![("Esc".into(), "cancel".into())],
            Stage::Picker(_) => Picker::hints(),
            Stage::Failed { .. } => vec![
                ("Enter".into(), "add with defaults".into()),
                ("r".into(), "retry".into()),
                ("Esc".into(), "back".into()),
            ],
        }
    }

    /// A tick: advance the spinner. Returns whether it did.
    pub fn on_tick(&mut self, tick: Duration) -> bool {
        if let Stage::Waiting { ticks, elapsed, .. } = &mut self.stage {
            *ticks += 1;
            *elapsed += tick;
            return true;
        }
        false
    }

    /// The connection dropped.
    pub fn on_disconnect(&mut self) {
        if let Stage::Waiting { url, .. } = &self.stage {
            self.stage = Stage::Failed {
                message: "lost the connection to the daemon".into(),
                at_prefs: url.is_none(),
            };
        }
    }

    /// Draw the popup.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let (title, lines) = match &self.stage {
            Stage::Picker(p) => return p.render(frame, area, theme),
            Stage::Waiting {
                url,
                ticks,
                elapsed,
            } => {
                let what = if url.is_some() {
                    "checking the available qualities"
                } else {
                    "reading your video preferences"
                };
                (
                    " Video quality ",
                    vec![
                        Line::from(vec![
                            Span::styled(
                                video::SPINNER[ticks % video::SPINNER.len()],
                                theme.accent,
                            ),
                            Span::raw(format!(" {what}… {}s", elapsed.as_secs())),
                        ]),
                        Line::default(),
                        Line::from(vec![Span::styled("Esc", theme.key), Span::raw(" cancel")]),
                    ],
                )
            }
            Stage::Failed { message, at_prefs } => (
                " Video quality ",
                vec![
                    Line::styled(
                        format!(
                            "{} failed: {message}",
                            if *at_prefs {
                                "Reading preferences"
                            } else {
                                "Probe"
                            }
                        ),
                        theme.warn,
                    ),
                    Line::default(),
                    Line::from(vec![
                        Span::styled("Enter", theme.action),
                        Span::raw(" add with your defaults   "),
                        Span::styled("r", theme.key),
                        Span::raw(" retry   "),
                        Span::styled("Esc", theme.key),
                        Span::raw(" back"),
                    ]),
                ],
            ),
        };
        let rect = crate::widgets::popup_rect(area, 64, 7);
        frame.render_widget(Clear, rect);
        let inner = crate::widgets::modal_frame(
            frame,
            rect,
            theme,
            Line::from(Span::styled(title, theme.accent.patch(theme.surface_bg()))),
            theme.border_focus,
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

fn put_back(state: &mut State, q: Box<QualityFlow>) {
    if let Some(a) = add_any(state) {
        a.overlay = Some(Overlay::Quality(q));
    }
}

/// Start the flow: ask for the preferences. Empty = offline (nothing changed;
/// the caller keeps its review open).
pub fn start(state: &mut State, q: QualityFlow) -> Vec<Cmd> {
    let cmds = state.call_quiet(ApiCall::VideoPrefsGet(PrefsIntent::Flow));
    if !cmds.is_empty() {
        put_back(state, Box::new(q));
    }
    cmds
}

/// Send the request with every decision applied.
fn finish(state: &mut State, q: Box<QualityFlow>, mut cmds: Vec<Cmd>) -> Vec<Cmd> {
    let Some(a) = add_any(state) else {
        return cmds;
    };
    a.submitting = Some(q.preview.origins.clone());
    let add = state.call(ApiCall::Add(Box::new(q.params.clone())));
    if add.is_empty() {
        // Offline: back to the review.
        if let Some(a) = add_any(state) {
            a.submitting = None;
            a.overlay = Some(Overlay::Preview(q.preview));
        }
        return cmds;
    }
    cmds.extend(add);
    cmds
}

/// Probe the next undecided video, or send the request when none is left.
fn advance(state: &mut State, mut q: Box<QualityFlow>, mut cmds: Vec<Cmd>) -> Vec<Cmd> {
    if q.skip_picker || q.pos >= q.targets.len() {
        return finish(state, q, cmds);
    }
    let Some((url, video)) = q.target_source() else {
        q.pos += 1;
        return advance(state, q, cmds);
    };
    let params = video::probe_params(url, video);
    q.stage = QualityFlow::waiting(Some(params.url.clone()));
    let call = state.call_quiet(ApiCall::VideoProbe(params));
    if call.is_empty() {
        q.stage = Stage::Failed {
            message: state
                .offline_reason()
                .unwrap_or_else(|| "not connected".into()),
            at_prefs: false,
        };
    }
    cmds.extend(call);
    put_back(state, q);
    cmds
}

/// A key while the flow is open.
pub fn on_key(state: &mut State, mut q: Box<QualityFlow>, key: KeyEvent) -> Vec<Cmd> {
    match &mut q.stage {
        Stage::Waiting { .. } => {
            if key.code != KeyCode::Esc {
                put_back(state, q);
            }
            // Esc drops the flow; the late answer finds no flow and is ignored.
            Vec::new()
        }
        Stage::Failed { at_prefs, .. } => {
            let at_prefs = *at_prefs;
            match key.code {
                KeyCode::Esc | KeyCode::Char('n' | 'q') => Vec::new(),
                KeyCode::Char('r') if at_prefs => {
                    q.stage = QualityFlow::waiting(None);
                    let cmds = state.call_quiet(ApiCall::VideoPrefsGet(PrefsIntent::Flow));
                    put_back(state, q);
                    cmds
                }
                KeyCode::Char('r') => advance(state, q, Vec::new()),
                KeyCode::Enter | KeyCode::Char('y') => {
                    if at_prefs {
                        q.skip_picker = true;
                    } else {
                        q.pos += 1;
                    }
                    advance(state, q, Vec::new())
                }
                _ => {
                    put_back(state, q);
                    Vec::new()
                }
            }
        }
        Stage::Picker(p) => match p.key(key) {
            PickerKey::Cancel => Vec::new(),
            PickerKey::Stay => {
                put_back(state, q);
                Vec::new()
            }
            PickerKey::Download => accept(state, q),
        },
    }
}

/// Enter in the picker: apply the choice, save the preferences, go on.
fn accept(state: &mut State, mut q: Box<QualityFlow>) -> Vec<Cmd> {
    let Stage::Picker(p) = &q.stage else {
        return Vec::new();
    };
    let Ok(chosen) = p.chosen() else {
        return Vec::new();
    };
    let update = p.prefs_update(&chosen);
    if let Some((_, video)) = q.target_source() {
        video.format_id = Some(chosen.format_id.clone());
        video.quality = None;
        video.audio_only = None;
    }
    q.pos += 1;
    let mut cmds = Vec::new();
    if let Some(u) = update {
        if u.ask_quality == Some(false) {
            q.skip_picker = true;
            state.video_notice = Some(NEVER_ASK_HINT.to_owned());
        }
        cmds.extend(state.call_quiet(ApiCall::VideoPrefsSet(u)));
    }
    advance(state, q, cmds)
}

/// Take the flow of the add view that waits for `want`.
fn take_waiting(
    state: &mut State,
    want: impl Fn(&Option<String>) -> bool,
) -> Option<Box<QualityFlow>> {
    let a = add_any(state)?;
    match &a.overlay {
        Some(Overlay::Quality(q)) if matches!(&q.stage, Stage::Waiting { url, .. } if want(url)) => {
            match a.overlay.take() {
                Some(Overlay::Quality(q)) => Some(q),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `video.preferences.get` answered. `Flow` continues the add; the palette
/// intents toggle or edit.
pub fn on_prefs(
    state: &mut State,
    intent: PrefsIntent,
    result: &Result<ApiReply, String>,
) -> Vec<Cmd> {
    let prefs = match result {
        Ok(ApiReply::VideoPreferences(p)) => Some((**p).clone()),
        _ => None,
    };
    if let Some(p) = &prefs {
        state.video_prefs = Some(p.clone());
    }
    match intent {
        PrefsIntent::Cache => Vec::new(),
        PrefsIntent::Flow => {
            let Some(mut q) = take_waiting(state, Option::is_none) else {
                return Vec::new();
            };
            match prefs {
                Some(p) if !p.ask_quality => {
                    q.skip_picker = true;
                    finish(state, q, Vec::new())
                }
                Some(_) => advance(state, q, Vec::new()),
                None => {
                    q.stage = Stage::Failed {
                        message: result
                            .as_ref()
                            .err()
                            .cloned()
                            .unwrap_or_else(|| "unexpected reply".into()),
                        at_prefs: true,
                    };
                    put_back(state, q);
                    Vec::new()
                }
            }
        }
        PrefsIntent::ToggleAsk => match prefs {
            Some(p) => state.call_quiet(ApiCall::VideoPrefsSet(VideoPreferencesUpdate {
                ask_quality: Some(!p.ask_quality),
                ..Default::default()
            })),
            None => {
                fail(state, result);
                Vec::new()
            }
        },
        PrefsIntent::EditQuality | PrefsIntent::EditAudio => match prefs {
            Some(p) => {
                state.modal = Some(crate::widgets::Modal::Prompt(prefs_prompt(intent, &p)));
                Vec::new()
            }
            None => {
                fail(state, result);
                Vec::new()
            }
        },
    }
}

fn fail(state: &mut State, result: &Result<ApiReply, String>) {
    let why = result
        .as_ref()
        .err()
        .map_or("unexpected reply", String::as_str);
    state
        .status
        .error(format!("Video preferences failed: {why}"));
}

/// Allowed `default_quality` values.
pub const QUALITIES: [&str; 9] = [
    "best",
    "2160p",
    "1440p",
    "1080p",
    "720p",
    "480p",
    "audio:m4a",
    "audio:mp3",
    "audio:opus",
];
/// Allowed `default_audio` values.
pub const AUDIOS: [&str; 4] = ["best", "m4a", "opus", "mp3"];

fn prefs_prompt(
    intent: PrefsIntent,
    p: &dman_proto::VideoPreferences,
) -> crate::widgets::prompt::PromptState {
    use crate::widgets::prompt::{PromptKind, PromptState};
    if intent == PrefsIntent::EditQuality {
        PromptState::new(
            PromptKind::VideoQuality,
            "Default video quality",
            &format!("One of: {}", QUALITIES.join(", ")),
            &p.default_quality,
        )
    } else {
        PromptState::new(
            PromptKind::VideoAudio,
            "Default video audio",
            &format!("One of: {}", AUDIOS.join(", ")),
            &p.default_audio,
        )
    }
}

/// A submitted default-quality / default-audio prompt.
pub fn submit_default(state: &mut State, audio: bool, value: &str) -> Vec<Cmd> {
    let v = value.trim().to_ascii_lowercase();
    let (allowed, what): (&[&str], &str) = if audio {
        (&AUDIOS, "audio")
    } else {
        (&QUALITIES, "quality")
    };
    if !allowed.contains(&v.as_str()) {
        state.status.error(format!(
            "Default {what} must be one of: {}",
            allowed.join(", ")
        ));
        return Vec::new();
    }
    let mut u = VideoPreferencesUpdate::default();
    if audio {
        u.default_audio = Some(v);
    } else {
        u.default_quality = Some(v);
    }
    state.call_quiet(ApiCall::VideoPrefsSet(u))
}

/// The action that toggles asking.
pub fn toggle_ask(state: &mut State) -> Vec<Cmd> {
    state.call_quiet(ApiCall::VideoPrefsGet(PrefsIntent::ToggleAsk))
}

/// Palette: edit the default quality or audio.
pub fn edit_default(state: &mut State, audio: bool) -> Vec<Cmd> {
    state.call_quiet(ApiCall::VideoPrefsGet(if audio {
        PrefsIntent::EditAudio
    } else {
        PrefsIntent::EditQuality
    }))
}

/// `video.probe` answered for the flow. Returns `None` when no flow waits for
/// this URL (a probe of the manual review).
pub fn on_probe(
    state: &mut State,
    url: &str,
    result: &Result<ApiReply, String>,
) -> Option<Vec<Cmd>> {
    let mut q = take_waiting(state, |u| u.as_deref() == Some(url))?;
    match result {
        Ok(ApiReply::VideoProbe(v)) if v.playlist && q.params.sources.len() == 1 => {
            // Playlist: the existing item picker of the review (manual probe).
            let mut preview = q.preview;
            preview.probe = ProbeFetch::Done(Box::new(ProbeUi::new((**v).clone())));
            preview.scroll = 0;
            if let Some(a) = add_any(state) {
                a.overlay = Some(Overlay::Preview(preview));
            }
            state
                .status
                .info("Playlist: choose the entries, then Enter to add");
            Some(Vec::new())
        }
        Ok(ApiReply::VideoProbe(v)) if v.playlist || !v.needs_choice => {
            q.pos += 1;
            Some(advance(state, q, Vec::new()))
        }
        Ok(ApiReply::VideoProbe(v)) => {
            q.stage = Stage::Picker(Box::new(Picker::new(
                (**v).clone(),
                (q.pos + 1, q.targets.len()),
            )));
            put_back(state, q);
            Some(Vec::new())
        }
        other => {
            q.stage = Stage::Failed {
                message: other
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "unexpected reply".into()),
                at_prefs: false,
            };
            put_back(state, q);
            Some(Vec::new())
        }
    }
}
