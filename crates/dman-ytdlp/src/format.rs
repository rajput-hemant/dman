use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::probe::{StreamKind, VideoInfo};
use crate::tools::Tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    Mp3,
    #[default]
    M4a,
    Opus,
}

impl AudioCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Opus => "opus",
        }
    }
}

/// Output container for merged video (`--merge-output-format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    #[default]
    Mp4,
    Mkv,
    Webm,
}

impl Container {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset {
    Best,
    P2160,
    P1440,
    P1080,
    P720,
    P480,
    AudioOnly { codec: AudioCodec },
}

impl QualityPreset {
    /// Height cap in pixels, `None` for Best and audio.
    pub fn max_height(self) -> Option<u32> {
        match self {
            Self::P2160 => Some(2160),
            Self::P1440 => Some(1440),
            Self::P1080 => Some(1080),
            Self::P720 => Some(720),
            Self::P480 => Some(480),
            Self::Best | Self::AudioOnly { .. } => None,
        }
    }
}

/// "best", "2160p".."480p", "audio:mp3|m4a|opus" (and bare "audio").
impl FromStr for QualityPreset {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let s = s.trim().to_ascii_lowercase();
        Ok(match s.as_str() {
            "best" => Self::Best,
            "2160p" | "2160" | "4k" => Self::P2160,
            "1440p" | "1440" => Self::P1440,
            "1080p" | "1080" => Self::P1080,
            "720p" | "720" => Self::P720,
            "480p" | "480" => Self::P480,
            "audio" | "audio:m4a" => Self::AudioOnly {
                codec: AudioCodec::M4a,
            },
            "audio:mp3" => Self::AudioOnly {
                codec: AudioCodec::Mp3,
            },
            "audio:opus" => Self::AudioOnly {
                codec: AudioCodec::Opus,
            },
            _ => return Err(Error::InvalidSpec(format!("unknown quality preset {s:?}"))),
        })
    }
}

/// yt-dlp `-f` expression. With `merge` (ffmpeg present) separate streams are combined;
/// without it only pre-merged formats are requested (spike: `bv*+ba` without ffmpeg exits 0
/// but leaves `.fN.` split files).
pub fn selector(preset: QualityPreset, merge: bool) -> String {
    match (preset.max_height(), merge) {
        (None, _) if matches!(preset, QualityPreset::AudioOnly { .. }) => "ba/b".into(),
        (None, true) => "bv*+ba/b".into(),
        (None, false) => "b".into(),
        (Some(h), true) => format!("bv*[height<={h}]+ba/b[height<={h}]"),
        (Some(h), false) => format!("b[height<={h}]/b"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanKind {
    /// Best video + best audio merged by ffmpeg.
    Merged,
    /// One pre-merged (or only) format.
    SingleStream,
    /// Video and audio saved as separate files (no ffmpeg, no pre-merged format).
    SeparateStreams,
    /// Audio only; extracted/converted when `audio_extract` is set.
    AudioOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub kind: PlanKind,
    pub selector: String,
    /// `-x --audio-format` target (only with ffmpeg).
    pub audio_extract: Option<AudioCodec>,
    /// Human-readable explanation to show the user.
    pub explanation: String,
    /// A higher-quality option the user may prefer (separate streams without ffmpeg).
    pub alternative: Option<Box<Plan>>,
}

/// Decide how to satisfy `preset` for `info` with the available `tools`, degrading gracefully
/// instead of failing (spec: best-effort fallback).
pub fn fallback_plan(info: &VideoInfo, preset: QualityPreset, tools: &Tools) -> Plan {
    let merge = tools.merge_supported();
    if let QualityPreset::AudioOnly { codec } = preset {
        let has_audio = info
            .formats
            .iter()
            .any(|f| matches!(f.kind(), StreamKind::AudioOnly | StreamKind::Combined));
        let (extract, explanation) = if merge {
            (
                Some(codec),
                format!(
                    "Audio will be extracted and converted to {}.",
                    codec.as_str()
                ),
            )
        } else {
            (
                None,
                "ffmpeg not found: saving the original audio stream without conversion.".into(),
            )
        };
        let explanation = if has_audio || info.formats.is_empty() {
            explanation
        } else {
            format!("{explanation} No dedicated audio stream was listed; the best format is used.")
        };
        return Plan {
            kind: PlanKind::AudioOnly,
            selector: selector(preset, merge),
            audio_extract: extract,
            explanation,
            alternative: None,
        };
    }

    let cap = preset.max_height();
    let eligible = |h: Option<u32>| match (cap, h) {
        (Some(c), Some(h)) => h <= c,
        _ => true,
    };
    let best = |kinds: &[StreamKind]| -> Option<Option<u32>> {
        // Some(best known height or None) if any eligible stream of those kinds exists.
        let mut found = None;
        for f in &info.formats {
            if kinds.contains(&f.kind()) && eligible(f.height) {
                found = Some(found.flatten().max(f.height));
            }
        }
        found
    };
    let combined = best(&[StreamKind::Combined]);
    let video_only = best(&[StreamKind::VideoOnly]);
    let has_audio_only = best(&[StreamKind::AudioOnly]).is_some();
    let separate = video_only.is_some() && has_audio_only;
    let merged_sel = selector(preset, true);
    let describe = |h: Option<u32>| h.map_or("best".to_string(), |h| format!("{h}p"));

    if merge {
        let explanation = if separate {
            format!(
                "Best video ({}) and audio will be merged with ffmpeg.",
                describe(video_only.flatten())
            )
        } else {
            "Best available format will be downloaded.".into()
        };
        return Plan {
            kind: if separate {
                PlanKind::Merged
            } else {
                PlanKind::SingleStream
            },
            selector: merged_sel,
            audio_extract: None,
            explanation,
            alternative: None,
        };
    }

    let separate_plan = |why: String| Plan {
        kind: PlanKind::SeparateStreams,
        selector: merged_sel.clone(),
        audio_extract: None,
        explanation: why,
        alternative: None,
    };

    match (combined, separate) {
        (Some(ch), _) => {
            let better_separate =
                separate && video_only.flatten().max(Some(0)) > ch.max(Some(0));
            let alternative = better_separate.then(|| {
                Box::new(separate_plan(format!(
                    "Download video ({}) and audio as two separate files (no merging without ffmpeg).",
                    describe(video_only.flatten())
                )))
            });
            let explanation = if better_separate {
                format!(
                    "ffmpeg not found: using the best pre-merged format ({}). Install ffmpeg for {} or accept separate files.",
                    describe(ch),
                    describe(video_only.flatten())
                )
            } else {
                "ffmpeg not found: using the best pre-merged format.".into()
            };
            Plan {
                kind: PlanKind::SingleStream,
                selector: selector(preset, false),
                audio_extract: None,
                explanation,
                alternative,
            }
        }
        (None, true) => separate_plan(
            "ffmpeg not found and no pre-merged format exists: video and audio will be saved as separate files."
                .into(),
        ),
        (None, false) => Plan {
            kind: PlanKind::SingleStream,
            selector: selector(preset, false),
            audio_extract: None,
            explanation: "Best available format will be downloaded.".into(),
            alternative: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Format;
    use crate::tools::ToolInfo;

    fn fmt(id: &str, v: Option<&str>, a: Option<&str>, h: Option<u32>) -> Format {
        Format {
            format_id: id.into(),
            ext: "mp4".into(),
            vcodec: v.map(Into::into),
            acodec: a.map(Into::into),
            width: None,
            height: h,
            fps: None,
            tbr: None,
            filesize: None,
            filesize_approx: None,
            protocol: None,
            note: None,
        }
    }

    fn info(formats: Vec<Format>) -> VideoInfo {
        VideoInfo {
            id: "x".into(),
            title: "t".into(),
            duration: None,
            uploader: None,
            thumbnail: None,
            formats,
            subtitles: vec![],
            automatic_captions: vec![],
        }
    }

    fn with_ffmpeg() -> Tools {
        Tools {
            ytdlp: None,
            ffmpeg: Some(ToolInfo {
                path: "ffmpeg".into(),
                version: "7".into(),
            }),
        }
    }

    fn dash() -> VideoInfo {
        info(vec![
            fmt("18", Some("avc1"), Some("mp4a"), Some(360)),
            fmt("136", Some("avc1"), Some("none"), Some(720)),
            fmt("137", Some("avc1"), Some("none"), Some(1080)),
            fmt("140", Some("none"), Some("mp4a"), None),
        ])
    }

    #[test]
    fn selectors() {
        assert_eq!(selector(QualityPreset::Best, true), "bv*+ba/b");
        assert_eq!(selector(QualityPreset::Best, false), "b");
        assert_eq!(
            selector(QualityPreset::P1080, true),
            "bv*[height<=1080]+ba/b[height<=1080]"
        );
        assert_eq!(selector(QualityPreset::P720, false), "b[height<=720]/b");
        let a = QualityPreset::AudioOnly {
            codec: AudioCodec::Mp3,
        };
        assert_eq!(selector(a, true), "ba/b");
        assert_eq!(selector(a, false), "ba/b");
    }

    #[test]
    fn preset_parsing() {
        assert_eq!(
            "1080p".parse::<QualityPreset>().unwrap(),
            QualityPreset::P1080
        );
        assert_eq!(
            "BEST".parse::<QualityPreset>().unwrap(),
            QualityPreset::Best
        );
        assert_eq!(
            "audio:opus".parse::<QualityPreset>().unwrap(),
            QualityPreset::AudioOnly {
                codec: AudioCodec::Opus
            }
        );
        assert!("potato".parse::<QualityPreset>().is_err());
    }

    #[test]
    fn plan_merged_with_ffmpeg() {
        let p = fallback_plan(&dash(), QualityPreset::P720, &with_ffmpeg());
        assert_eq!(p.kind, PlanKind::Merged);
        assert_eq!(p.selector, "bv*[height<=720]+ba/b[height<=720]");
        assert!(p.explanation.contains("720p"), "{}", p.explanation);
    }

    #[test]
    fn plan_without_ffmpeg_uses_combined_and_offers_separate() {
        let p = fallback_plan(&dash(), QualityPreset::Best, &Tools::default());
        assert_eq!(p.kind, PlanKind::SingleStream);
        assert_eq!(p.selector, "b");
        let alt = p.alternative.expect("separate streams offered");
        assert_eq!(alt.kind, PlanKind::SeparateStreams);
        assert_eq!(alt.selector, "bv*+ba/b");
    }

    #[test]
    fn plan_without_ffmpeg_and_no_gain_has_no_alternative() {
        let i = info(vec![fmt("18", Some("avc1"), Some("mp4a"), Some(360))]);
        let p = fallback_plan(&i, QualityPreset::P720, &Tools::default());
        assert_eq!(p.kind, PlanKind::SingleStream);
        assert!(p.alternative.is_none());
    }

    #[test]
    fn plan_separate_streams_when_nothing_merged() {
        let i = info(vec![
            fmt("v", Some("vp9"), Some("none"), Some(1080)),
            fmt("a", Some("none"), Some("opus"), None),
        ]);
        let p = fallback_plan(&i, QualityPreset::Best, &Tools::default());
        assert_eq!(p.kind, PlanKind::SeparateStreams);
        assert_eq!(p.selector, "bv*+ba/b");
    }

    #[test]
    fn plan_unknown_codecs_generic_extractor() {
        let i = info(vec![fmt("0", None, None, None)]);
        let p = fallback_plan(&i, QualityPreset::Best, &Tools::default());
        assert_eq!(p.kind, PlanKind::SingleStream);
        assert!(p.alternative.is_none());
        let p = fallback_plan(&i, QualityPreset::Best, &with_ffmpeg());
        assert_eq!(p.kind, PlanKind::SingleStream);
    }

    #[test]
    fn plan_audio() {
        let a = QualityPreset::AudioOnly {
            codec: AudioCodec::Mp3,
        };
        let p = fallback_plan(&dash(), a, &with_ffmpeg());
        assert_eq!(p.kind, PlanKind::AudioOnly);
        assert_eq!(p.audio_extract, Some(AudioCodec::Mp3));
        let p = fallback_plan(&dash(), a, &Tools::default());
        assert_eq!(p.audio_extract, None);
        assert!(p.explanation.contains("ffmpeg"));
    }
}
