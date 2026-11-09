//! Quality picker rules, the same as the TUI and the CLI implement them (see the
//! module docs of `dman_proto::video`): what is preselected, what a preset
//! selects, which `format_id` a selection becomes and what "use as my default"
//! saves. Pure functions over a probe, so the picker works without JavaScript:
//! the browser only sends row numbers, and the server decides the format.

use dman_proto::video::{QualityChoice, QualityChoiceKind};
use dman_proto::{VideoPreferencesUpdate, VideoProbe};

/// Preset buttons: `(form value, label)`.
pub const PRESETS: [(&str, &str); 5] = [
    ("best", "Best"),
    ("1080p", "1080p"),
    ("720p", "720p"),
    ("480p", "480p"),
    ("audio", "Audio only"),
];

/// `AAC` -> `m4a`, `Opus` -> `opus`, `MP3` -> `mp3`.
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

/// `1080` -> `1080p` for the presets the daemon knows, otherwise `best`.
pub fn preset_for_height(height: Option<u32>) -> String {
    match height {
        Some(h @ (2160 | 1440 | 1080 | 720 | 480)) => format!("{h}p"),
        _ => "best".to_owned(),
    }
}

/// The selected rows: `video` indexes `video_choices` (`video_choices.len()` is
/// the "audio only" row), `audio` indexes `audio_choices`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub video: usize,
    pub audio: usize,
}

/// An "audio only" row exists when there are audio choices.
pub fn can_audio_only(p: &VideoProbe) -> bool {
    !p.audio_choices.is_empty()
}

/// The audio that comes with video choice `vidx` by itself (`VID+AUD`), else
/// the suggested one, else the best.
pub fn default_audio_for(p: &VideoProbe, vidx: usize) -> Option<usize> {
    let by_id = |id: &str| p.audio_choices.iter().position(|a| a.id == id);
    p.video_choices
        .get(vidx)
        .and_then(|c| c.id.split_once('+'))
        .and_then(|(_, a)| by_id(a))
        .or_else(|| p.suggested.audio.as_deref().and_then(by_id))
        .or((!p.audio_choices.is_empty()).then_some(0))
}

/// What is preselected: `probe.suggested`, else the best available video.
pub fn initial(p: &VideoProbe) -> Selection {
    let video = p
        .suggested
        .video
        .as_deref()
        .and_then(|id| p.video_choices.iter().position(|c| c.id == id));
    let video = match video {
        Some(i) => i,
        None if p.suggested.audio.is_some() && can_audio_only(p) => p.video_choices.len(),
        None => p
            .video_choices
            .iter()
            .position(|c| c.available)
            .unwrap_or(0),
    };
    let audio = p
        .suggested
        .audio
        .as_deref()
        .and_then(|id| p.audio_choices.iter().position(|c| c.id == id))
        .or_else(|| default_audio_for(p, video))
        .unwrap_or(0);
    Selection { video, audio }
}

/// A preset's video row: the first available choice within the height limit
/// (best first), else the lowest available one, else the audio-only row.
/// `None` = the preset does not apply (unknown name, or no audio-only row).
pub fn preset_row(p: &VideoProbe, preset: &str) -> Option<usize> {
    let choices = &p.video_choices;
    let limit = match preset {
        "best" => None,
        "1080p" => Some(1080),
        "720p" => Some(720),
        "480p" => Some(480),
        "audio" => return can_audio_only(p).then_some(choices.len()),
        _ => return None,
    };
    choices
        .iter()
        .position(|c| c.available && limit.is_none_or(|l| c.height.unwrap_or(0) <= l))
        .or_else(|| choices.iter().rposition(|c| c.available))
        .or_else(|| can_audio_only(p).then_some(choices.len()))
}

/// Applies a preset: its video row, and the audio that goes with it.
pub fn apply_preset(p: &VideoProbe, preset: &str, sel: Selection) -> Selection {
    match preset_row(p, preset) {
        Some(video) => Selection {
            video,
            audio: default_audio_for(p, video).unwrap_or(sel.audio),
        },
        None => sel,
    }
}

/// The audio list applies to the selected video row (video-only choices and
/// the audio-only row).
pub fn audio_applies(p: &VideoProbe, sel: Selection) -> bool {
    match p.video_choices.get(sel.video) {
        Some(c) => c.kind == QualityChoiceKind::VideoOnly && can_audio_only(p),
        None => can_audio_only(p),
    }
}

/// What the selection downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// `VideoOptions::format_id`.
    pub format_id: String,
    /// Height of the video (none for audio only).
    pub height: Option<u32>,
    /// Codec of the audio that is part of the download, when it was chosen.
    pub audio_codec: Option<String>,
    pub audio_only: bool,
}

fn unavailable(c: &QualityChoice) -> String {
    format!(
        "{} is not available: {}",
        c.label,
        c.unavailable_reason.as_deref().unwrap_or("needs ffmpeg")
    )
}

/// The `format_id` of the selection.
///
/// # Errors
/// The reason the selection cannot be downloaded (nothing there, or it needs
/// ffmpeg and ffmpeg is missing).
pub fn chosen(p: &VideoProbe, sel: Selection) -> Result<Chosen, String> {
    if let Some(c) = p.video_choices.get(sel.video) {
        if !c.available {
            return Err(unavailable(c));
        }
        let audio = audio_applies(p, sel)
            .then(|| p.audio_choices.get(sel.audio))
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
    let a = p
        .audio_choices
        .get(sel.audio)
        .filter(|_| sel.video == p.video_choices.len())
        .ok_or_else(|| "Choose something to download.".to_owned())?;
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

/// The `video.preferences.set` payload for the ticked checkboxes (none = nothing ticked).
pub fn prefs_update(
    c: &Chosen,
    use_default: bool,
    never_ask: bool,
) -> Option<VideoPreferencesUpdate> {
    let mut u = VideoPreferencesUpdate::default();
    if never_ask {
        u.ask_quality = Some(false);
    }
    if use_default {
        let family = audio_family(c.audio_codec.as_deref());
        if c.audio_only {
            u.default_quality = Some(format!("audio:{}", family.unwrap_or("m4a")));
            u.default_audio = Some(family.unwrap_or("best").to_owned());
        } else {
            u.default_quality = Some(preset_for_height(c.height));
            if c.audio_codec.is_some() {
                u.default_audio = Some(family.unwrap_or("best").to_owned());
            }
        }
    }
    (u != VideoPreferencesUpdate::default()).then_some(u)
}
