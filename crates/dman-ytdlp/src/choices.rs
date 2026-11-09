//! Human-friendly quality choices built from a probed format list: what a
//! quality picker shows before a video download starts.
//!
//! Rules:
//! * Storyboards and other non-media formats are ignored.
//! * **Video choices** are one per (height, fps class, codec family). Within
//!   such a group the best format wins (largest size, then bitrate). A
//!   video-only format is combined with the preferred audio stream, so its
//!   `id` is `VID+AUD` (yt-dlp `-f` syntax) and it needs ffmpeg to merge. A
//!   pre-merged ("muxed") format of the same group is dropped when ffmpeg is
//!   present (the merged choice is at least as good) and kept next to the
//!   unavailable merged choice otherwise.
//! * **Audio choices** are one per (codec family, bitrate); the best-sized
//!   format wins.
//! * `needs_choice` is true when there is more than one distinct (height,
//!   fps class) or more than one audio choice; a single-format source (a
//!   direct file) needs no picker.

use std::cmp::Ordering;

use crate::format::{AudioPref, QualityPreset};
use crate::probe::{Format, StreamKind, VideoInfo};
use crate::tools::Tools;

/// What a choice downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    /// One pre-merged stream (video and audio, or codecs unknown).
    Muxed,
    /// A video stream, combined with the preferred audio stream when one exists.
    VideoOnly,
    AudioOnly,
}

/// One picker entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// A yt-dlp format id, or `VID+AUD` for a video combined with audio.
    pub id: String,
    /// The format id of the video (or audio) stream alone; pair it with
    /// another audio choice as `{stream_id}+{audio.id}`.
    pub stream_id: String,
    /// "1080p60 · VP9 · ~1.2 GiB"
    pub label: String,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Friendly codec family: H.264, VP9, AV1, AAC, Opus, ...
    pub codec: Option<String>,
    /// Bytes (video plus audio for a combined choice).
    pub size_estimate: Option<u64>,
    pub kind: ChoiceKind,
    pub requires_ffmpeg: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

/// The defaults the suggestion is computed from.
#[derive(Debug, Clone, Copy)]
pub struct ChoiceDefaults {
    pub preset: QualityPreset,
    pub audio: AudioPref,
}

/// What the defaults would pick, as ids of the lists in [`Choices`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggested {
    /// `video[].id`; unset for audio-only defaults or sources without video.
    pub video: Option<String>,
    /// `audio[].id`: the audio part of a combined video choice, or the audio
    /// track of an audio-only default; unset for a pre-merged video choice.
    pub audio: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choices {
    pub video: Vec<Choice>,
    pub audio: Vec<Choice>,
    pub needs_choice: bool,
    pub suggested: Suggested,
}

const NEEDS_FFMPEG: &str = "ffmpeg is needed to merge video and audio";

/// fps above ~30 distinguishes "60 fps" choices; 24/25/30 are one class.
fn fps_class(fps: Option<f64>) -> u32 {
    match fps {
        Some(f) if f > 30.5 => f.round() as u32,
        _ => 0,
    }
}

fn video_family(codec: Option<&str>) -> Option<String> {
    let c = codec?.to_ascii_lowercase();
    if c == "none" {
        return None;
    }
    let head = c.split('.').next().unwrap_or(&c);
    Some(
        match head {
            "avc1" | "avc3" | "h264" => "H.264",
            "vp9" | "vp09" => "VP9",
            "vp8" => "VP8",
            "av01" | "av1" => "AV1",
            "hev1" | "hvc1" | "h265" | "hevc" => "H.265",
            other => return Some(other.to_ascii_uppercase()),
        }
        .to_owned(),
    )
}

fn audio_family(codec: Option<&str>) -> Option<String> {
    let c = codec?.to_ascii_lowercase();
    if c == "none" {
        return None;
    }
    let head = c.split('.').next().unwrap_or(&c);
    Some(
        match head {
            "mp4a" | "aac" => "AAC",
            "opus" => "Opus",
            "vorbis" => "Vorbis",
            "mp3" => "MP3",
            "ac-3" | "ac3" => "AC-3",
            "ec-3" | "eac3" => "E-AC-3",
            "flac" => "FLAC",
            other => return Some(other.to_ascii_uppercase()),
        }
        .to_owned(),
    )
}

/// Preference order of codecs at equal resolution: widest compatibility first.
fn codec_rank(family: Option<&str>) -> u8 {
    match family {
        Some("H.264") => 0,
        Some("VP9") => 1,
        Some("AV1") => 2,
        Some("H.265") => 3,
        _ => 4,
    }
}

/// Bytes: exact, approximate, or bitrate times duration.
fn estimate(f: &Format, duration: Option<f64>) -> Option<u64> {
    f.size().or_else(|| match (f.tbr, duration) {
        (Some(tbr), Some(d)) if tbr > 0.0 && d > 0.0 => Some((tbr * 1000.0 / 8.0 * d) as u64),
        _ => None,
    })
}

fn human_size(b: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn resolution(height: Option<u32>, class: u32) -> Option<String> {
    height.map(|h| {
        if class > 0 {
            format!("{h}p{class}")
        } else {
            format!("{h}p")
        }
    })
}

fn label(parts: &[Option<String>], size: Option<u64>) -> String {
    let mut v: Vec<String> = parts.iter().flatten().cloned().collect();
    if let Some(s) = size {
        v.push(format!("~{}", human_size(s)));
    }
    v.join(" · ")
}

/// Larger size first, then bitrate; used to pick the best format of a group.
fn better(a: &Format, b: &Format, duration: Option<f64>) -> Ordering {
    estimate(a, duration)
        .cmp(&estimate(b, duration))
        .then_with(|| {
            a.tbr
                .unwrap_or(0.0)
                .partial_cmp(&b.tbr.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal)
        })
}

fn abr(f: &Format) -> u32 {
    f.tbr.map_or(0, |t| t.round() as u32)
}

/// Best audio-only format for `pref`: highest bitrate among those matching,
/// else among all.
fn pick_audio<'a>(audio: &[&'a Format], pref: AudioPref) -> Option<&'a Format> {
    let matches = |f: &Format| {
        let c = f.acodec.as_deref().unwrap_or("").to_ascii_lowercase();
        match pref {
            AudioPref::Best => true,
            AudioPref::M4a => f.ext.eq_ignore_ascii_case("m4a"),
            AudioPref::Opus => c.starts_with("opus"),
            AudioPref::Mp3 => c.starts_with("mp3"),
        }
    };
    let best =
        |it: &mut dyn Iterator<Item = &&'a Format>| it.max_by(|a, b| abr(a).cmp(&abr(b))).copied();
    best(&mut audio.iter().filter(|f| matches(f))).or_else(|| best(&mut audio.iter()))
}

fn audio_choice(f: &Format, duration: Option<f64>) -> Choice {
    let codec = audio_family(f.acodec.as_deref());
    let size = estimate(f, duration);
    let rate = (abr(f) > 0).then(|| format!("{} kbps", abr(f)));
    Choice {
        id: f.format_id.clone(),
        stream_id: f.format_id.clone(),
        label: label(
            &[codec.clone().or_else(|| Some(f.ext.to_uppercase())), rate],
            size,
        ),
        height: None,
        fps: None,
        codec,
        size_estimate: size,
        kind: ChoiceKind::AudioOnly,
        requires_ffmpeg: false,
        available: true,
        unavailable_reason: None,
    }
}

/// Builds the picker lists for `info` with the available `tools`, and what
/// `defaults` would pick.
pub fn build_choices(info: &VideoInfo, tools: &Tools, defaults: ChoiceDefaults) -> Choices {
    let merge = tools.merge_supported();
    let duration = info.duration;

    // ---- audio ----
    let audio_formats: Vec<&Format> = info
        .formats
        .iter()
        .filter(|f| f.kind() == StreamKind::AudioOnly)
        .collect();
    let mut audio: Vec<&Format> = Vec::new();
    for f in &audio_formats {
        let key = (audio_family(f.acodec.as_deref()), abr(f));
        let same = |g: &&Format| (audio_family(g.acodec.as_deref()), abr(g)) == key;
        match audio.iter().position(same) {
            Some(i) if better(f, audio[i], duration) == Ordering::Greater => audio[i] = f,
            Some(_) => {}
            None => audio.push(f),
        }
    }
    audio.sort_by(|a, b| {
        abr(b)
            .cmp(&abr(a))
            .then_with(|| audio_family(a.acodec.as_deref()).cmp(&audio_family(b.acodec.as_deref())))
    });
    let audio_choices: Vec<Choice> = audio.iter().map(|f| audio_choice(f, duration)).collect();

    // ---- video ----
    type Key = (Option<u32>, u32, Option<String>);
    let key_of = |f: &Format| -> Key {
        (
            f.height,
            fps_class(f.fps),
            video_family(f.vcodec.as_deref()),
        )
    };
    let mut muxed: Vec<(Key, &Format)> = Vec::new();
    let mut only: Vec<(Key, &Format)> = Vec::new();
    for f in &info.formats {
        let list = match f.kind() {
            StreamKind::Combined => &mut muxed,
            StreamKind::VideoOnly => &mut only,
            _ => continue,
        };
        let key = key_of(f);
        match list.iter().position(|(k, _)| *k == key) {
            Some(i) if better(f, list[i].1, duration) == Ordering::Greater => list[i].1 = f,
            Some(_) => {}
            None => list.push((key, f)),
        }
    }
    let combine_with = pick_audio(&audio, defaults.audio);
    let mut video_choices: Vec<Choice> = Vec::new();
    for (key, f) in &only {
        let (height, class, family) = key;
        let (id, requires, size) = match combine_with {
            Some(a) => (
                format!("{}+{}", f.format_id, a.format_id),
                true,
                match (estimate(f, duration), estimate(a, duration)) {
                    (Some(v), Some(a)) => Some(v + a),
                    (v, _) => v,
                },
            ),
            None => (f.format_id.clone(), false, estimate(f, duration)),
        };
        let available = !requires || merge;
        video_choices.push(Choice {
            id,
            stream_id: f.format_id.clone(),
            label: label(&[resolution(*height, *class), family.clone()], size),
            height: *height,
            fps: f.fps,
            codec: family.clone(),
            size_estimate: size,
            kind: ChoiceKind::VideoOnly,
            requires_ffmpeg: requires,
            available,
            unavailable_reason: (!available).then(|| NEEDS_FFMPEG.to_owned()),
        });
    }
    for (key, f) in &muxed {
        if merge && only.iter().any(|(k, _)| k == key) {
            continue;
        }
        let (height, class, family) = key;
        let size = estimate(f, duration);
        let first = resolution(*height, *class).or_else(|| {
            f.note
                .clone()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| Some(f.ext.to_uppercase()))
        });
        video_choices.push(Choice {
            id: f.format_id.clone(),
            stream_id: f.format_id.clone(),
            label: label(&[first, family.clone()], size),
            height: *height,
            fps: f.fps,
            codec: family.clone(),
            size_estimate: size,
            kind: ChoiceKind::Muxed,
            requires_ffmpeg: false,
            available: true,
            unavailable_reason: None,
        });
    }
    video_choices.sort_by(|a, b| {
        b.height
            .cmp(&a.height)
            .then_with(|| fps_class(b.fps).cmp(&fps_class(a.fps)))
            .then_with(|| codec_rank(a.codec.as_deref()).cmp(&codec_rank(b.codec.as_deref())))
            .then_with(|| (a.kind as u8).cmp(&(b.kind as u8)))
    });

    // ---- needs_choice ----
    let mut qualities: Vec<(Option<u32>, u32)> = video_choices
        .iter()
        .map(|c| (c.height, fps_class(c.fps)))
        .collect();
    qualities.sort_unstable();
    qualities.dedup();
    let needs_choice = qualities.len() > 1 || audio_choices.len() > 1;

    // ---- suggestion ----
    let suggested = suggest(&video_choices, &audio_choices, &audio, defaults);

    Choices {
        video: video_choices,
        audio: audio_choices,
        needs_choice,
        suggested,
    }
}

fn suggest(
    video: &[Choice],
    audio_choices: &[Choice],
    audio: &[&Format],
    defaults: ChoiceDefaults,
) -> Suggested {
    if let QualityPreset::AudioOnly { codec } = defaults.preset {
        let pref = match codec {
            crate::format::AudioCodec::M4a => AudioPref::M4a,
            crate::format::AudioCodec::Opus => AudioPref::Opus,
            crate::format::AudioCodec::Mp3 => AudioPref::Mp3,
        };
        return Suggested {
            video: None,
            audio: pick_audio(audio, pref).map(|f| f.format_id.clone()),
        };
    }
    let usable: Vec<&Choice> = {
        let ok: Vec<&Choice> = video.iter().filter(|c| c.available).collect();
        if ok.is_empty() {
            video.iter().collect()
        } else {
            ok
        }
    };
    // `usable` is sorted best first; honour the height cap, else take the lowest.
    let pick = match defaults.preset.max_height() {
        Some(cap) => usable
            .iter()
            .find(|c| c.height.is_none_or(|h| h <= cap))
            .or_else(|| usable.last()),
        None => usable.first(),
    };
    let Some(pick) = pick else {
        return Suggested::default();
    };
    let audio_id = (pick.kind == ChoiceKind::VideoOnly)
        .then(|| pick.id.strip_prefix(&format!("{}+", pick.stream_id)))
        .flatten()
        .filter(|a| audio_choices.iter().any(|c| c.id == *a))
        .map(str::to_owned);
    Suggested {
        video: Some(pick.id.clone()),
        audio: audio_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{ProbeResult, parse_probe_json};
    use crate::tools::ToolInfo;

    /// A YouTube-like probe: muxed 360p, video-only AVC/VP9/AV1 at several
    /// heights (including 60 fps), three audio streams, and a storyboard.
    const YOUTUBE: &str = r#"{
      "id": "abc", "title": "Demo", "duration": 100.0,
      "formats": [
        {"format_id":"sb0","ext":"mhtml","vcodec":"none","acodec":"none","protocol":"mhtml","height":90},
        {"format_id":"139","ext":"m4a","vcodec":"none","acodec":"mp4a.40.5","tbr":49.6,"filesize":600000},
        {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","tbr":129.5,"filesize":1600000},
        {"format_id":"140-dup","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","tbr":129.5,"filesize":1500000},
        {"format_id":"251","ext":"webm","vcodec":"none","acodec":"opus","tbr":135.0,"filesize":1700000},
        {"format_id":"18","ext":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","height":360,"width":640,"fps":30,"tbr":500.0,"filesize":6000000},
        {"format_id":"134","ext":"mp4","vcodec":"avc1.4d401e","acodec":"none","height":360,"fps":30,"tbr":300.0,"filesize":3500000},
        {"format_id":"135","ext":"mp4","vcodec":"avc1.4d401f","acodec":"none","height":480,"fps":30,"tbr":700.0,"filesize":8000000},
        {"format_id":"136","ext":"mp4","vcodec":"avc1.4d401f","acodec":"none","height":720,"fps":30,"tbr":1500.0,"filesize":18000000},
        {"format_id":"137","ext":"mp4","vcodec":"avc1.640028","acodec":"none","height":1080,"fps":30,"tbr":4000.0,"filesize":50000000},
        {"format_id":"248","ext":"webm","vcodec":"vp9","acodec":"none","height":1080,"fps":30,"tbr":2500.0,"filesize":31000000},
        {"format_id":"299","ext":"mp4","vcodec":"avc1.64002a","acodec":"none","height":1080,"fps":60,"tbr":7000.0,"filesize":85000000},
        {"format_id":"303","ext":"webm","vcodec":"vp9","acodec":"none","height":1080,"fps":60,"tbr":3500.0,"filesize":42000000},
        {"format_id":"399","ext":"mp4","vcodec":"av01.0.08M.08","acodec":"none","height":1080,"fps":30,"tbr":1500.0,"filesize":19000000},
        {"format_id":"313","ext":"webm","vcodec":"vp9","acodec":"none","height":2160,"fps":30,"tbr":15000.0,"filesize":180000000}
      ]
    }"#;

    /// A direct file: the generic extractor, one format, codecs unknown.
    const DIRECT: &str = r#"{
      "id": "file", "title": "file",
      "formats": [{"format_id":"0","ext":"mp4","protocol":"https","filesize":1048576}]
    }"#;

    fn info(json: &str) -> VideoInfo {
        match parse_probe_json(json.as_bytes()).unwrap() {
            ProbeResult::Video(v) => v,
            other => panic!("{other:?}"),
        }
    }

    fn tools(ffmpeg: bool) -> Tools {
        Tools {
            ytdlp: None,
            ffmpeg: ffmpeg.then(|| ToolInfo {
                path: "ffmpeg".into(),
                version: "7".into(),
            }),
        }
    }

    fn defaults(preset: QualityPreset, audio: AudioPref) -> ChoiceDefaults {
        ChoiceDefaults { preset, audio }
    }

    fn build(json: &str, ffmpeg: bool, preset: QualityPreset, audio: AudioPref) -> Choices {
        build_choices(&info(json), &tools(ffmpeg), defaults(preset, audio))
    }

    #[test]
    fn youtube_like_video_choices() {
        let c = build(YOUTUBE, true, QualityPreset::Best, AudioPref::Best);
        let ids: Vec<&str> = c.video.iter().map(|c| c.id.as_str()).collect();
        // Highest first; at equal height 60 fps before 30; at equal fps
        // H.264, VP9, AV1; the muxed 360p is dropped (merged 360p exists).
        // Best audio is Opus 135 kbps (251).
        assert_eq!(
            ids,
            [
                "313+251", "299+251", "303+251", "137+251", "248+251", "399+251", "136+251",
                "135+251", "134+251"
            ]
        );
        assert!(c.needs_choice);
        let top = &c.video[1];
        assert_eq!(top.label, "1080p60 · H.264 · ~82.7 MiB");
        assert_eq!(top.stream_id, "299");
        assert_eq!(top.kind, ChoiceKind::VideoOnly);
        assert!(top.requires_ffmpeg && top.available);
        assert_eq!(top.size_estimate, Some(85_000_000 + 1_700_000));
        assert_eq!(top.codec.as_deref(), Some("H.264"));
        assert_eq!(c.video[0].label, "2160p · VP9 · ~173 MiB");
    }

    #[test]
    fn youtube_like_audio_choices_deduplicate() {
        let c = build(YOUTUBE, true, QualityPreset::Best, AudioPref::Best);
        let ids: Vec<&str> = c.audio.iter().map(|c| c.id.as_str()).collect();
        // 251 (Opus 135), 140 (AAC 130, larger duplicate wins), 139 (AAC 50)
        assert_eq!(ids, ["251", "140", "139"]);
        assert_eq!(c.audio[0].label, "Opus · 135 kbps · ~1.6 MiB");
        assert_eq!(c.audio[1].label, "AAC · 130 kbps · ~1.5 MiB");
        assert!(c.audio.iter().all(|a| a.kind == ChoiceKind::AudioOnly
            && !a.requires_ffmpeg
            && a.available
            && a.height.is_none()));
    }

    #[test]
    fn audio_preference_picks_the_combined_track() {
        let c = build(YOUTUBE, true, QualityPreset::P1080, AudioPref::M4a);
        assert!(
            c.video.iter().all(|v| v.id.ends_with("+140")),
            "{:?}",
            c.video
        );
        let c = build(YOUTUBE, true, QualityPreset::P1080, AudioPref::Opus);
        assert!(c.video.iter().all(|v| v.id.ends_with("+251")));
        // No mp3 stream: falls back to the best audio.
        let c = build(YOUTUBE, true, QualityPreset::P1080, AudioPref::Mp3);
        assert!(c.video.iter().all(|v| v.id.ends_with("+251")));
    }

    #[test]
    fn suggestion_follows_defaults() {
        let s = |p, a| build(YOUTUBE, true, p, a).suggested;
        assert_eq!(
            s(QualityPreset::Best, AudioPref::Best),
            Suggested {
                video: Some("313+251".into()),
                audio: Some("251".into())
            }
        );
        // Capped: best 1080p, 60 fps H.264, with the preferred m4a track.
        assert_eq!(
            s(QualityPreset::P1080, AudioPref::M4a),
            Suggested {
                video: Some("299+140".into()),
                audio: Some("140".into())
            }
        );
        assert_eq!(
            s(QualityPreset::P720, AudioPref::Best).video.as_deref(),
            Some("136+251")
        );
        // Cap below everything: the lowest available.
        let tiny = build(YOUTUBE, true, QualityPreset::P480, AudioPref::Best).suggested;
        assert_eq!(tiny.video.as_deref(), Some("135+251"));
        let a = QualityPreset::AudioOnly {
            codec: crate::format::AudioCodec::M4a,
        };
        assert_eq!(
            s(a, AudioPref::Best),
            Suggested {
                video: None,
                audio: Some("140".into())
            }
        );
    }

    #[test]
    fn without_ffmpeg_merged_choices_are_unavailable() {
        let c = build(YOUTUBE, false, QualityPreset::Best, AudioPref::Best);
        let top = &c.video[0];
        assert!(top.requires_ffmpeg && !top.available);
        assert_eq!(
            top.unavailable_reason.as_deref(),
            Some("ffmpeg is needed to merge video and audio")
        );
        // The muxed 360p stays, next to the unavailable merged 360p.
        let muxed = c.video.iter().find(|v| v.id == "18").expect("muxed kept");
        assert!(muxed.available && !muxed.requires_ffmpeg);
        assert_eq!(muxed.kind, ChoiceKind::Muxed);
        assert!(c.video.iter().any(|v| v.id == "134+251" && !v.available));
        // The suggestion is the best available choice.
        assert_eq!(c.suggested.video.as_deref(), Some("18"));
        assert_eq!(c.suggested.audio, None);
        // Audio-only choices never need ffmpeg.
        assert!(c.audio.iter().all(|a| a.available));
    }

    #[test]
    fn single_format_source_needs_no_choice() {
        for ffmpeg in [true, false] {
            let c = build(DIRECT, ffmpeg, QualityPreset::Best, AudioPref::Best);
            assert!(!c.needs_choice);
            assert_eq!(c.video.len(), 1);
            assert!(c.audio.is_empty());
            assert_eq!(c.video[0].id, "0");
            assert_eq!(c.video[0].kind, ChoiceKind::Muxed);
            assert_eq!(c.video[0].label, "MP4 · ~1.0 MiB");
            assert_eq!(c.suggested.video.as_deref(), Some("0"));
        }
    }

    #[test]
    fn needs_choice_rules() {
        // One video quality but two audio streams: choose.
        let one_video_two_audio = r#"{"id":"x","title":"t","formats":[
          {"format_id":"v","ext":"mp4","vcodec":"avc1","acodec":"none","height":720},
          {"format_id":"a1","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","tbr":128},
          {"format_id":"a2","ext":"webm","vcodec":"none","acodec":"opus","tbr":128}]}"#;
        assert!(
            build(
                one_video_two_audio,
                true,
                QualityPreset::Best,
                AudioPref::Best
            )
            .needs_choice
        );
        // Same height in different codecs is not a different quality.
        let codecs_only = r#"{"id":"x","title":"t","formats":[
          {"format_id":"v1","ext":"mp4","vcodec":"avc1","acodec":"none","height":720},
          {"format_id":"v2","ext":"webm","vcodec":"vp9","acodec":"none","height":720},
          {"format_id":"a1","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","tbr":128}]}"#;
        let c = build(codecs_only, true, QualityPreset::Best, AudioPref::Best);
        assert_eq!(c.video.len(), 2);
        assert!(!c.needs_choice);
        // HLS-style muxed ladder.
        let ladder = r#"{"id":"x","title":"t","formats":[
          {"format_id":"h1","ext":"mp4","height":480},{"format_id":"h2","ext":"mp4","height":720}]}"#;
        assert!(build(ladder, false, QualityPreset::Best, AudioPref::Best).needs_choice);
        // Nothing at all.
        let none = r#"{"id":"x","title":"t"}"#;
        let c = build(none, true, QualityPreset::Best, AudioPref::Best);
        assert!(!c.needs_choice && c.video.is_empty() && c.suggested == Suggested::default());
    }

    #[test]
    fn size_falls_back_to_bitrate_times_duration() {
        let j = r#"{"id":"x","title":"t","duration":80,"formats":[
          {"format_id":"v","ext":"mp4","vcodec":"avc1","acodec":"none","height":720,"tbr":800}]}"#;
        let c = build(j, true, QualityPreset::Best, AudioPref::Best);
        assert_eq!(c.video[0].size_estimate, Some(8_000_000));
        assert_eq!(c.video[0].id, "v");
        assert!(!c.video[0].requires_ffmpeg);
    }
}
