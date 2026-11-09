//! Video downloads through the yt-dlp engine: per-job options and `video.probe`.

use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}

/// Subtitles to fetch for a video job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoSubtitles {
    /// Language codes such as `en`, `de`, `en.*`.
    pub langs: Vec<String>,
    /// Embed them into the video (needs ffmpeg); otherwise written next to it.
    #[serde(default = "yes")]
    pub embed: bool,
    /// Also fetch automatic captions.
    #[serde(default)]
    pub auto: bool,
}

impl Default for VideoSubtitles {
    fn default() -> Self {
        Self {
            langs: Vec::new(),
            embed: true,
            auto: false,
        }
    }
}

/// Options of a video job (`AddSource::Video`). Unset fields take the
/// daemon's `[video]` defaults when the job is added; the resolved options
/// are stored with the job. The download rate limit is
/// `JobOptions::max_download_limit`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoOptions {
    /// Quality preset (`best`, `2160p`, `1440p`, `1080p`, `720p`, `480p`,
    /// `audio:m4a|mp3|opus`) or a raw yt-dlp format selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Exact format id(s) from `video.probe` (e.g. `137+140`); wins over `quality`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_id: Option<String>,
    /// Container for merged output: `mp4`, `mkv` or `webm` (needs ffmpeg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Audio only, converted to this codec (`mp3`, `m4a`, `opus`; conversion
    /// needs ffmpeg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_only: Option<String>,
    /// Download the whole playlist (implied by `playlist_items`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub playlist: bool,
    /// Playlist items such as `1-3,7`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_items: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<VideoSubtitles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_thumbnail: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_metadata: Option<bool>,
    /// yt-dlp output template relative to the job directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_template: Option<String>,
    /// Browser to read cookies from (`firefox`, `chrome`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookies_from_browser: Option<String>,
    /// SponsorBlock categories to cut out (needs ffmpeg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsorblock_remove: Option<Vec<String>>,
    /// Consent to keep video and audio as separate files when they cannot be
    /// merged (no ffmpeg). Without it such a download fails with an explanation.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub allow_separate_streams: bool,
    /// Extra yt-dlp flags (allow-listed by the daemon).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

/// `video.probe` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoProbeParams {
    pub url: String,
    /// List the playlist entries instead of probing a single video.
    #[serde(default)]
    pub playlist: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies_from_browser: Option<String>,
}

/// What a format contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoStreamKind {
    /// Video and audio together (or codecs unknown).
    Combined,
    VideoOnly,
    AudioOnly,
    /// Storyboards and the like.
    Other,
}

/// One downloadable format of a video.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoFormat {
    pub id: String,
    pub ext: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcodec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acodec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// Bytes, exact or estimated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub kind: VideoStreamKind,
}

/// One entry of a probed playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistEntry {
    /// 1-based position (as used by `playlist_items`).
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

/// How a quality choice would be satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoPlanKind {
    /// Best video and audio merged by ffmpeg.
    Merged,
    /// One pre-merged format.
    Single,
    /// Video and audio as two files (no ffmpeg); needs `allow_separate_streams`.
    Separate,
    /// Audio only.
    Audio,
}

/// The plan for one quality preset, with a human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPlan {
    /// The preset name, usable as `VideoOptions::quality`.
    pub preset: String,
    pub kind: VideoPlanKind,
    /// The yt-dlp format selector that would be used.
    pub selector: String,
    pub explanation: String,
    /// A better option the user may choose instead (separate streams without ffmpeg).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative: Option<Box<VideoPlan>>,
}

/// Result of `video.probe`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoProbe {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub formats: Vec<VideoFormat>,
    /// Languages of uploaded subtitles.
    #[serde(default)]
    pub subtitles: Vec<String>,
    /// Languages of automatic captions.
    #[serde(default)]
    pub automatic_captions: Vec<String>,
    /// The URL is a playlist; see `entries`.
    #[serde(default)]
    pub playlist: bool,
    #[serde(default)]
    pub entries: Vec<PlaylistEntry>,
    /// ffmpeg is available on the daemon host (merging, conversion, embedding).
    #[serde(default)]
    pub ffmpeg: bool,
    /// One plan per quality preset, honouring ffmpeg availability.
    #[serde(default)]
    pub plans: Vec<VideoPlan>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_and_round_trip() {
        let o: VideoOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(o, VideoOptions::default());
        assert_eq!(serde_json::to_string(&o).unwrap(), "{}");
        let o = VideoOptions {
            quality: Some("1080p".into()),
            subtitles: Some(VideoSubtitles {
                langs: vec!["en".into()],
                ..Default::default()
            }),
            allow_separate_streams: true,
            ..Default::default()
        };
        let v = serde_json::to_value(&o).unwrap();
        assert_eq!(v["subtitles"]["embed"], true);
        assert_eq!(serde_json::from_value::<VideoOptions>(v).unwrap(), o);
        let s: VideoSubtitles = serde_json::from_str(r#"{"langs":["de"]}"#).unwrap();
        assert!(s.embed && !s.auto);
    }
}
