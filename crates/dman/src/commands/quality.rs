//! The video quality picker of `dman add` and `dman video prefs`.
//!
//! Flow (see `dman_proto::video`): preferences → (`ask_quality`) probe →
//! playlist: unchanged / no choice needed: defaults / else the prompts below.
//! The prompts run only when stdin and stdout are terminals and nothing in
//! the command line already decides the quality ([`picker_allowed`]).

use std::io::{BufRead, Write};

use dman_proto::video::{QualityChoice, QualityChoiceKind};
use dman_proto::{VideoPreferences, VideoPreferencesUpdate, VideoProbe};

use crate::cli::{PrefsArgs, VideoArgs};
use crate::commands::print_json;
use crate::completion::{AUDIO_PREFS, CONTAINERS, QUALITY_PRESETS};
use crate::connect::Ctx;
use crate::error::CliError;
use crate::format;

/// Printed after "never ask again".
pub const NEVER_ASK_HINT: &str =
    "quality picker turned off; turn it back on with `dman video prefs --ask true`";

/// Whether the picker may prompt: both ends are terminals, no `--json` /
/// `--yes`, stdin is not a source list, and no option already names the
/// quality or the playlist items.
pub fn picker_allowed(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    yes: bool,
    reads_stdin: bool,
    v: &VideoArgs,
) -> bool {
    stdin_tty
        && stdout_tty
        && !json
        && !yes
        && !reads_stdin
        && v.quality.is_none()
        && v.format_id.is_none()
        && !v.playlist
        && v.playlist_items.is_none()
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

/// What the answers download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// `VideoOptions::format_id`.
    pub format_id: String,
    /// Height of the video (none for audio only).
    pub height: Option<u32>,
    /// Codec of the audio that is part of the download, when it was chosen.
    pub audio_codec: Option<String>,
    /// Audio only.
    pub audio_only: bool,
}

/// The `format_id` for `video` (or audio only when `None`) and `audio`.
///
/// # Errors
/// The reason the choice cannot be downloaded.
pub fn pick(
    video: Option<&QualityChoice>,
    audio: Option<&QualityChoice>,
) -> Result<Picked, String> {
    let unavailable = |c: &QualityChoice| {
        format!(
            "{} is not available: {}",
            c.label,
            c.unavailable_reason.as_deref().unwrap_or("needs ffmpeg")
        )
    };
    let Some(c) = video else {
        let a = audio.ok_or("no audio to download")?;
        if !a.available {
            return Err(unavailable(a));
        }
        return Ok(Picked {
            format_id: a.id.clone(),
            height: None,
            audio_codec: a.codec.clone(),
            audio_only: true,
        });
    };
    if !c.available {
        return Err(unavailable(c));
    }
    let audio = audio.filter(|_| c.kind == QualityChoiceKind::VideoOnly);
    let format_id = match audio {
        Some(a) if !a.available => return Err(unavailable(a)),
        Some(a) if c.id.split_once('+').is_some_and(|(_, id)| id == a.id) => c.id.clone(),
        Some(a) => format!("{}+{}", c.stream_id, a.id),
        None => c.id.clone(),
    };
    Ok(Picked {
        format_id,
        height: c.height,
        audio_codec: audio.and_then(|a| a.codec.clone()),
        audio_only: false,
    })
}

/// The `video.preferences.set` payload for the answers (none = nothing ticked).
pub fn prefs_update(
    p: &Picked,
    use_default: bool,
    never_ask: bool,
) -> Option<VideoPreferencesUpdate> {
    let mut u = VideoPreferencesUpdate::default();
    if never_ask {
        u.ask_quality = Some(false);
    }
    if use_default {
        let family = audio_family(p.audio_codec.as_deref());
        if p.audio_only {
            u.default_quality = Some(format!("audio:{}", family.unwrap_or("m4a")));
            u.default_audio = Some(family.unwrap_or("best").to_owned());
        } else {
            u.default_quality = Some(preset_for_height(p.height));
            if p.audio_codec.is_some() {
                u.default_audio = Some(family.unwrap_or("best").to_owned());
            }
        }
    }
    (u != VideoPreferencesUpdate::default()).then_some(u)
}

/// A numbered list of choices; `default` (0-based) is marked as suggested.
pub fn format_choices(
    heading: &str,
    choices: &[QualityChoice],
    default: Option<usize>,
    audio_only_row: bool,
) -> String {
    let mut out = format!("{heading}:\n");
    for (i, c) in choices.iter().enumerate() {
        let mut label = c.label.clone();
        if !label.contains('~')
            && let Some(s) = c.size_estimate
        {
            label.push_str(&format!(" · ~{}", format::size(s)));
        }
        let note = if !c.available {
            format!(
                "  (unavailable: {})",
                c.unavailable_reason.as_deref().unwrap_or("needs ffmpeg")
            )
        } else if c.requires_ffmpeg {
            "  [needs ffmpeg]".to_owned()
        } else {
            String::new()
        };
        let mark = if default == Some(i) {
            "  (suggested)"
        } else {
            ""
        };
        out.push_str(&format!("  {:>2}) {label}{note}{mark}\n", i + 1));
    }
    if audio_only_row {
        let i = choices.len();
        let mark = if default == Some(i) {
            "  (suggested)"
        } else {
            ""
        };
        out.push_str(&format!("  {:>2}) Audio only (no video){mark}\n", i + 1));
    }
    out
}

/// A 1-based answer to a numbered list; empty accepts `default` (0-based).
///
/// # Errors
/// Why the answer is not usable.
pub fn parse_pick(answer: &str, len: usize, default: usize) -> Result<usize, String> {
    let a = answer.trim();
    if a.is_empty() {
        return Ok(default);
    }
    match a.parse::<usize>() {
        Ok(n) if (1..=len).contains(&n) => Ok(n - 1),
        _ => Err(format!("enter a number from 1 to {len}, or press Enter")),
    }
}

/// `y` / `yes` (anything else is no).
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn read_line(input: &mut impl BufRead) -> Result<String, CliError> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(CliError::refused("aborted (no answer)")
            .with_hint("answer in a terminal, or pass --quality / --yes to skip the picker"));
    }
    Ok(line.trim().to_owned())
}

/// Ask for a number until it is valid; `valid` may refuse a row (unavailable).
fn ask_number(
    prompt: &str,
    len: usize,
    default: usize,
    valid: &dyn Fn(usize) -> Result<(), String>,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<usize, CliError> {
    loop {
        write!(out, "{prompt} [{}]: ", default + 1)?;
        out.flush()?;
        let answer = read_line(input)?;
        match parse_pick(&answer, len, default).and_then(|i| valid(i).map(|()| i)) {
            Ok(i) => return Ok(i),
            Err(e) => writeln!(out, "  {e}")?,
        }
    }
}

/// The answers of one picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    /// What to download.
    pub picked: Picked,
    /// "Use as default" and/or "never ask again".
    pub update: Option<VideoPreferencesUpdate>,
    /// "Never ask again" was answered yes.
    pub never_ask: bool,
}

/// Show the choices of `probe` and read the answers.
///
/// # Errors
/// End of input (aborted) or an I/O error.
pub fn run_picker(
    probe: &VideoProbe,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Answers, CliError> {
    let v = &probe.video_choices;
    let a = &probe.audio_choices;
    let audio_only_row = !a.is_empty();
    let by_id = |list: &[QualityChoice], id: Option<&str>| {
        id.and_then(|id| list.iter().position(|c| c.id == id))
    };
    let sugg_v = by_id(v, probe.suggested.video.as_deref());
    let sugg_a = by_id(a, probe.suggested.audio.as_deref());
    let video_default = sugg_v
        .or_else(|| (sugg_a.is_some() && audio_only_row).then_some(v.len()))
        .or_else(|| v.iter().position(|c| c.available))
        .unwrap_or(0);

    writeln!(out)?;
    let mut head = probe.title.clone().unwrap_or_else(|| probe.url.clone());
    if let Some(u) = &probe.uploader {
        head.push_str(&format!(" — by {u}"));
    }
    if let Some(d) = probe.duration {
        head.push_str(&format!(" ({})", format::duration(d)));
    }
    writeln!(out, "{head}")?;
    if !probe.ffmpeg {
        writeln!(out, "(ffmpeg is missing: merged qualities are unavailable)")?;
    }
    write!(
        out,
        "{}",
        format_choices("Video", v, Some(video_default), audio_only_row)
    )?;
    let rows = v.len() + usize::from(audio_only_row);
    let vi = ask_number(
        "Video",
        rows,
        video_default,
        &|i| match v.get(i) {
            Some(c) if !c.available => pick(Some(c), None).map(|_| ()),
            _ => Ok(()),
        },
        input,
        out,
    )?;
    let video = v.get(vi);
    let needs_audio = !a.is_empty() && video.is_none_or(|c| c.kind == QualityChoiceKind::VideoOnly);
    let audio = if needs_audio {
        let by_video = video
            .and_then(|c| c.id.split_once('+'))
            .and_then(|(_, id)| by_id(a, Some(id)));
        let audio_default = by_video.or(sugg_a).unwrap_or(0);
        write!(
            out,
            "{}",
            format_choices("Audio", a, Some(audio_default), false)
        )?;
        let ai = ask_number(
            "Audio",
            a.len(),
            audio_default,
            &|i| pick(video, a.get(i)).map(|_| ()),
            input,
            out,
        )?;
        a.get(ai)
    } else {
        None
    };
    let picked = pick(video, audio).map_err(|e| {
        CliError::invalid(e)
            .with_hint("answer with a number from the list, or press Enter for the suggestion")
    })?;
    write!(out, "Use as default? [y/N] ")?;
    out.flush()?;
    let use_default = is_yes(&read_line(input)?);
    write!(out, "Never ask again? [y/N] ")?;
    out.flush()?;
    let never_ask = is_yes(&read_line(input)?);
    let update = prefs_update(&picked, use_default, never_ask);
    Ok(Answers {
        picked,
        update,
        never_ask,
    })
}

/// The table of `dman video prefs`.
pub fn prefs_lines(p: &VideoPreferences) -> Vec<(&'static str, String)> {
    vec![
        (
            "ask quality",
            if p.ask_quality {
                "yes (the picker is shown before downloading)".into()
            } else {
                "no (downloads use the defaults)".into()
            },
        ),
        ("default quality", p.default_quality.clone()),
        ("default audio", p.default_audio.clone()),
        (
            "default container",
            p.default_container
                .clone()
                .unwrap_or_else(|| "(yt-dlp's choice)".into()),
        ),
    ]
}

/// Validate `dman video prefs` flags into an update.
///
/// # Errors
/// A value that is not allowed.
pub fn prefs_args_update(a: &PrefsArgs) -> Result<VideoPreferencesUpdate, CliError> {
    let check = |what: &str, v: &str, allowed: &[&str]| {
        if allowed.contains(&v) {
            Ok(v.to_owned())
        } else {
            let close = crate::suggest::closest(v, allowed.iter().copied(), 2);
            let hint = crate::suggest::did_you_mean(&close)
                .unwrap_or_else(|| format!("use one of {}", allowed.join(", ")));
            Err(CliError::invalid(format!(
                "invalid {what} '{v}': use one of {}",
                allowed.join(", ")
            ))
            .with_hint(hint))
        }
    };
    Ok(VideoPreferencesUpdate {
        ask_quality: a.ask,
        default_quality: a
            .quality
            .as_deref()
            .map(|v| check("quality", v, QUALITY_PRESETS))
            .transpose()?,
        default_audio: a
            .audio
            .as_deref()
            .map(|v| check("audio", v, AUDIO_PREFS))
            .transpose()?,
        default_container: match a.container.as_deref() {
            None => None,
            Some("none" | "") => Some(String::new()),
            Some(v) => Some(check("container", v, CONTAINERS)?),
        },
    })
}

/// `dman video prefs`.
pub async fn prefs(ctx: &Ctx, a: &PrefsArgs) -> Result<i32, CliError> {
    let update = prefs_args_update(a)?;
    let client = ctx.connect().await?;
    let p = if update == VideoPreferencesUpdate::default() {
        client.video_preferences().await?
    } else {
        client.video_preferences_set(update).await?
    };
    if ctx.json {
        print_json(&p)?;
    } else {
        print!("{}", format::key_values(&prefs_lines(&p)));
        if !p.ask_quality {
            println!("\n{NEVER_ASK_HINT}");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use dman_proto::video::QualityChoiceRef;

    use super::*;

    fn c(
        id: &str,
        stream: &str,
        label: &str,
        h: Option<u32>,
        codec: &str,
        kind: QualityChoiceKind,
        ok: bool,
    ) -> QualityChoice {
        let needs = kind == QualityChoiceKind::VideoOnly;
        QualityChoice {
            id: id.into(),
            stream_id: stream.into(),
            label: label.into(),
            height: h,
            codec: Some(codec.into()),
            size_estimate: Some(50 << 20),
            kind,
            requires_ffmpeg: needs,
            available: !needs || ok,
            unavailable_reason: (needs && !ok).then(|| "needs ffmpeg".to_owned()),
            ..QualityChoice::default()
        }
    }

    fn probe(ffmpeg: bool) -> VideoProbe {
        use QualityChoiceKind::{AudioOnly, Muxed, VideoOnly};
        VideoProbe {
            url: "https://youtu.be/x".into(),
            title: Some("Clip".into()),
            uploader: Some("Someone".into()),
            duration: Some(125.0),
            ffmpeg,
            video_choices: vec![
                c(
                    "137+251",
                    "137",
                    "1080p · H.264 · ~83 MiB",
                    Some(1080),
                    "H.264",
                    VideoOnly,
                    ffmpeg,
                ),
                c(
                    "136+251",
                    "136",
                    "720p · H.264 · ~41 MiB",
                    Some(720),
                    "H.264",
                    VideoOnly,
                    ffmpeg,
                ),
                c(
                    "18",
                    "18",
                    "360p · H.264 · ~12 MiB",
                    Some(360),
                    "H.264",
                    Muxed,
                    ffmpeg,
                ),
            ],
            audio_choices: vec![
                c(
                    "251",
                    "251",
                    "Opus · 135 kbps",
                    None,
                    "Opus",
                    AudioOnly,
                    ffmpeg,
                ),
                c(
                    "140",
                    "140",
                    "AAC · 129 kbps",
                    None,
                    "AAC",
                    AudioOnly,
                    ffmpeg,
                ),
            ],
            needs_choice: true,
            suggested: QualityChoiceRef {
                video: Some("137+251".into()),
                audio: Some("251".into()),
            },
            ..VideoProbe::default()
        }
    }

    fn run(p: &VideoProbe, answers: &str) -> (Result<Answers, CliError>, String) {
        let mut out = Vec::new();
        let r = run_picker(p, &mut Cursor::new(answers.to_owned()), &mut out);
        (r, String::from_utf8(out).expect("utf8"))
    }

    #[test]
    fn the_picker_only_prompts_in_a_real_terminal() {
        let v = VideoArgs::default();
        assert!(picker_allowed(true, true, false, false, false, &v));
        assert!(
            !picker_allowed(false, true, false, false, false, &v),
            "piped stdin"
        );
        assert!(
            !picker_allowed(true, false, false, false, false, &v),
            "piped stdout"
        );
        assert!(
            !picker_allowed(true, true, true, false, false, &v),
            "--json"
        );
        assert!(!picker_allowed(true, true, false, true, false, &v), "--yes");
        assert!(
            !picker_allowed(true, true, false, false, true, &v),
            "sources on stdin"
        );
        let q = VideoArgs {
            quality: Some("720p".into()),
            ..Default::default()
        };
        assert!(!picker_allowed(true, true, false, false, false, &q));
        let f = VideoArgs {
            format_id: Some("18".into()),
            ..Default::default()
        };
        assert!(!picker_allowed(true, true, false, false, false, &f));
        let p = VideoArgs {
            playlist_items: Some("1-3".into()),
            ..Default::default()
        };
        assert!(!picker_allowed(true, true, false, false, false, &p));
    }

    #[test]
    fn choice_list_formatting() {
        let p = probe(false);
        let t = format_choices("Video", &p.video_choices, Some(0), true);
        assert_eq!(
            t,
            "Video:\n   1) 1080p · H.264 · ~83 MiB  (unavailable: needs ffmpeg)  (suggested)\n   \
             2) 720p · H.264 · ~41 MiB  (unavailable: needs ffmpeg)\n   \
             3) 360p · H.264 · ~12 MiB\n   4) Audio only (no video)\n"
        );
        let p = probe(true);
        let t = format_choices("Video", &p.video_choices, Some(0), false);
        assert!(
            t.contains("1) 1080p · H.264 · ~83 MiB  [needs ffmpeg]  (suggested)"),
            "{t}"
        );
        assert!(!t.contains("Audio only"));
        // A label without size gets the estimate.
        let mut x = p.video_choices[2].clone();
        x.label = "360p".into();
        assert!(format_choices("V", &[x], None, false).contains("360p · ~50.0 MiB"));
    }

    #[test]
    fn answers_are_parsed() {
        assert_eq!(parse_pick("", 3, 1), Ok(1));
        assert_eq!(parse_pick(" 3 ", 3, 1), Ok(2));
        assert!(parse_pick("0", 3, 1).is_err());
        assert!(parse_pick("4", 3, 1).is_err());
        assert!(parse_pick("x", 3, 1).is_err());
        assert!(is_yes("Y") && is_yes("yes") && !is_yes("") && !is_yes("n"));
    }

    #[test]
    fn enter_accepts_the_suggestions() {
        let (r, out) = run(&probe(true), "\n\n\n\n");
        let a = r.expect("answers");
        assert_eq!(a.picked.format_id, "137+251");
        assert_eq!(a.update, None);
        assert!(out.contains("Video [1]: "), "{out}");
        assert!(out.contains("Audio [1]: "), "{out}");
        assert!(out.contains("Use as default? [y/N] "), "{out}");
        assert!(out.contains("Never ask again? [y/N] "), "{out}");
        assert!(out.contains("Clip — by Someone (2:05)"), "{out}");
    }

    #[test]
    fn different_audio_and_muxed_and_audio_only() {
        let (r, _) = run(&probe(true), "2\n2\nn\nn\n");
        assert_eq!(r.expect("a").picked.format_id, "136+140");
        // Pre-merged: no audio question (the next two answers are the checkboxes).
        let (r, out) = run(&probe(true), "3\ny\nn\n");
        let a = r.expect("a");
        assert_eq!(a.picked.format_id, "18");
        assert!(!out.contains("Audio ["));
        assert_eq!(
            a.update,
            Some(VideoPreferencesUpdate {
                default_quality: Some("best".into()),
                ..Default::default()
            })
        );
        // Audio only (row 4), AAC.
        let (r, _) = run(&probe(true), "4\n2\ny\ny\n");
        let a = r.expect("a");
        assert_eq!(a.picked.format_id, "140");
        assert!(a.never_ask);
        assert_eq!(
            a.update,
            Some(VideoPreferencesUpdate {
                ask_quality: Some(false),
                default_quality: Some("audio:m4a".into()),
                default_audio: Some("m4a".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn use_as_default_maps_height_and_audio_family() {
        let (r, _) = run(&probe(true), "\n\ny\nn\n");
        assert_eq!(
            r.expect("a").update,
            Some(VideoPreferencesUpdate {
                default_quality: Some("1080p".into()),
                default_audio: Some("opus".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn unavailable_rows_are_refused_and_invalid_input_reprompts() {
        let (r, out) = run(&probe(false), "1\nx\n3\nn\nn\n");
        assert_eq!(r.expect("a").picked.format_id, "18");
        assert!(
            out.contains("is not available: needs ffmpeg") || out.contains("not available"),
            "{out}"
        );
        assert!(out.contains("enter a number from 1 to 4"), "{out}");
    }

    #[test]
    fn end_of_input_aborts() {
        let (r, _) = run(&probe(true), "");
        assert_eq!(r.expect_err("aborted").exit, 6);
    }

    #[test]
    fn prefs_flags_are_validated() {
        let ok = prefs_args_update(&PrefsArgs {
            ask: Some(false),
            quality: Some("720p".into()),
            audio: Some("opus".into()),
            container: Some("none".into()),
        })
        .expect("valid");
        assert_eq!(
            ok,
            VideoPreferencesUpdate {
                ask_quality: Some(false),
                default_quality: Some("720p".into()),
                default_audio: Some("opus".into()),
                default_container: Some(String::new()),
            }
        );
        assert_eq!(
            prefs_args_update(&PrefsArgs::default()).expect("empty"),
            VideoPreferencesUpdate::default()
        );
        for bad in [
            PrefsArgs {
                quality: Some("4k".into()),
                ..Default::default()
            },
            PrefsArgs {
                audio: Some("flac".into()),
                ..Default::default()
            },
            PrefsArgs {
                container: Some("avi".into()),
                ..Default::default()
            },
        ] {
            assert_eq!(prefs_args_update(&bad).expect_err("invalid").exit, 2);
        }
    }

    #[test]
    fn prefs_table() {
        let t = format::key_values(&prefs_lines(&VideoPreferences {
            ask_quality: false,
            default_quality: "1080p".into(),
            default_audio: "m4a".into(),
            default_container: None,
        }));
        assert!(t.contains("no (downloads use the defaults)"), "{t}");
        assert!(t.contains("default container:  (yt-dlp's choice)"), "{t}");
    }
}
