//! `[video]`: the optional yt-dlp (+ffmpeg) engine and defaults for video jobs.

use std::str::FromStr;

use dman_ytdlp::{AudioPref, Container, DownloadSpec, QualityPreset, SponsorBlock, SubOpts};
use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}

fn default_quality() -> String {
    "best".into()
}

fn default_audio() -> String {
    "best".into()
}

fn default_template() -> String {
    dman_ytdlp::DEFAULT_TEMPLATE.into()
}

/// `[video.subtitles]`: subtitles fetched by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoSubtitles {
    /// Language codes; empty = no subtitles.
    pub langs: Vec<String>,
    /// Embed into the video (needs ffmpeg).
    #[serde(default = "yes")]
    pub embed: bool,
}

impl Default for VideoSubtitles {
    fn default() -> Self {
        Self {
            langs: Vec::new(),
            embed: true,
        }
    }
}

/// The `[video]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Video {
    /// yt-dlp executable; unset = `yt-dlp` on `PATH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ytdlp_path: Option<String>,
    /// ffmpeg executable; unset = `ffmpeg` on `PATH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffmpeg_path: Option<String>,
    /// Show the quality picker before a video download starts (clients probe
    /// the URL and let the user choose). `false` = "never ask again": videos
    /// are added directly with the defaults below.
    #[serde(default = "yes")]
    pub ask_quality: bool,
    /// Quality preset: best, 2160p, 1440p, 1080p, 720p, 480p, audio:m4a|mp3|opus.
    #[serde(default = "default_quality")]
    pub default_quality: String,
    /// Preferred audio track when video and audio are combined:
    /// best, m4a, opus or mp3 (falls back to the best audio).
    #[serde(default = "default_audio")]
    pub default_audio: String,
    /// Container for merged video: mp4, mkv or webm; unset = yt-dlp's choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_container: Option<String>,
    /// yt-dlp output template, relative to the download directory.
    #[serde(default = "default_template")]
    pub output_template: String,
    /// SponsorBlock categories removed by default (needs ffmpeg).
    pub sponsorblock_remove: Vec<String>,
    /// Embed metadata (needs ffmpeg).
    #[serde(default = "yes")]
    pub embed_metadata: bool,
    /// Embed the thumbnail (needs ffmpeg).
    pub embed_thumbnail: bool,
    /// Default subtitles.
    pub subtitles: VideoSubtitles,
    /// Browser to read cookies from by default (`firefox`, `chrome`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookies_from_browser: Option<String>,
}

impl Default for Video {
    fn default() -> Self {
        Self {
            ytdlp_path: None,
            ffmpeg_path: None,
            ask_quality: true,
            default_quality: default_quality(),
            default_audio: default_audio(),
            default_container: None,
            output_template: default_template(),
            sponsorblock_remove: Vec::new(),
            embed_metadata: true,
            embed_thumbnail: false,
            subtitles: VideoSubtitles::default(),
            cookies_from_browser: None,
        }
    }
}

impl Video {
    /// Semantic checks (the same rules a video job's options must pass).
    pub fn validate(&self) -> Result<(), String> {
        let empty = |o: &Option<String>| o.as_deref().is_some_and(|s| s.trim().is_empty());
        if empty(&self.ytdlp_path) || empty(&self.ffmpeg_path) {
            return Err("video.ytdlp_path / video.ffmpeg_path must not be empty".into());
        }
        QualityPreset::from_str(&self.default_quality)
            .map_err(|e| format!("video.default_quality: {e}"))?;
        AudioPref::from_str(&self.default_audio)
            .map_err(|e| format!("video.default_audio: {e}"))?;
        if let Some(c) = &self.default_container {
            Container::from_str(c).map_err(|e| format!("video.default_container: {e}"))?;
        }
        let url = url::Url::parse("https://example.org/v").map_err(|e| e.to_string())?;
        let mut spec = DownloadSpec::new(url, "/");
        spec.output_template = self.output_template.clone();
        spec.cookies_from_browser = self.cookies_from_browser.clone();
        if !self.sponsorblock_remove.is_empty() {
            spec.sponsorblock = Some(SponsorBlock {
                remove: self.sponsorblock_remove.clone(),
                mark: Vec::new(),
            });
        }
        if !self.subtitles.langs.is_empty() {
            spec.subtitles = Some(SubOpts {
                langs: self.subtitles.langs.clone(),
                embed: self.subtitles.embed,
                auto: false,
            });
        }
        spec.validate().map_err(|e| format!("[video]: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_valid() {
        Video::default().validate().unwrap();
    }

    #[test]
    fn invalid_values_rejected() {
        let bad = [
            Video {
                default_quality: "999p".into(),
                ..Video::default()
            },
            Video {
                default_container: Some("avi".into()),
                ..Video::default()
            },
            Video {
                default_audio: "flac".into(),
                ..Video::default()
            },
            Video {
                output_template: "../x.%(ext)s".into(),
                ..Video::default()
            },
            Video {
                output_template: "/abs/%(title)s".into(),
                ..Video::default()
            },
            Video {
                sponsorblock_remove: vec!["bogus".into()],
                ..Video::default()
            },
            Video {
                cookies_from_browser: Some("--exec".into()),
                ..Video::default()
            },
            Video {
                subtitles: VideoSubtitles {
                    langs: vec!["-x".into()],
                    embed: true,
                },
                ..Video::default()
            },
            Video {
                ytdlp_path: Some(" ".into()),
                ..Video::default()
            },
        ];
        for v in bad {
            assert!(v.validate().is_err(), "{v:?}");
        }
        let ok = Video {
            default_quality: "audio:mp3".into(),
            default_container: Some("mkv".into()),
            default_audio: "opus".into(),
            ask_quality: false,
            sponsorblock_remove: vec!["sponsor".into(), "intro".into()],
            ..Video::default()
        };
        ok.validate().unwrap();
    }

    #[test]
    fn new_keys_default_and_parse() {
        let v: Video = toml::from_str("").unwrap();
        assert!(v.ask_quality);
        assert_eq!(v.default_audio, "best");
        let v: Video = toml::from_str("ask_quality = false\ndefault_audio = \"m4a\"").unwrap();
        assert!(!v.ask_quality);
        assert_eq!(v.default_audio, "m4a");
        v.validate().unwrap();
    }
}
