//! Video downloads through the yt-dlp engine: per-job options, `video.probe`
//! and the quality-picker preferences.
//!
//! # Quality picker flow (every client implements it the same way)
//!
//! ```text
//! prefs = video.preferences.get
//! if !engine.status(yt-dlp).available      -> no video (as before)
//! if !prefs.ask_quality                    -> jobs.add Video { VideoOptions::default() }
//!                                             (no probe; the daemon applies the saved defaults)
//! else:
//!     probe = video.probe { url }
//!     if probe.playlist                    -> playlist/item picker; quality from `probe.plans`
//!                                             (presets) or the defaults
//!     elif !probe.needs_choice             -> jobs.add Video { VideoOptions::default() }
//!     else                                 -> show the picker (below)
//! ```
//!
//! The picker preselects `probe.suggested` and lists `video_choices` (best
//! first) and `audio_choices`. Entries with `available == false` are shown
//! greyed with `unavailable_reason` (ffmpeg missing) and cannot be chosen.
//! It offers a "use as default" checkbox and a "never ask again" checkbox.
//!
//! What the choice becomes in `VideoOptions`:
//! * a video choice: `format_id = choice.id` (`VID+AUD` or a single id);
//! * a different audio track for a video-only choice:
//!   `format_id = "{video.stream_id}+{audio.id}"`;
//! * an audio-only download of the original stream: `format_id = audio.id`;
//!   converted to a codec (needs ffmpeg): `audio_only = "mp3"|"m4a"|"opus"`;
//! * a preset instead of exact formats: `quality = "1080p"`, `"best"`,
//!   `"audio:mp3"`, optionally `"1080p+m4a"` (preset plus audio preference).
//!
//! `format_id` may hold anything yt-dlp's `-f` accepts, in particular
//! `137+140` and `137+140/18`; a `+` needs ffmpeg (without it the add is
//! refused unless `allow_separate_streams` is set).
//!
//! When "use as default" and/or "never ask again" are ticked the client calls
//! `video.preferences.set` (before or after adding):
//! * "use as default": `default_quality` (preset name matching the choice's
//!   height, e.g. `1080p`, or `audio:m4a`) and `default_audio` (`m4a`, `opus`,
//!   `mp3` from the audio codec, else `best`);
//! * "never ask again": `ask_quality = false`.
//!
//! Settings screens read the preferences and use the same call to turn asking
//! back on (`ask_quality = true`) or to change the defaults. With neither
//! `quality`, `format_id` nor `audio_only` set, the daemon uses
//! `default_quality` + `default_audio` (stored as `quality`, e.g. `1080p+m4a`)
//! and `default_container`.

//!
//! # `dman probe --json` schema
//!
//! `dman probe URL --json` prints a [`VideoProbe`] exactly as `video.probe` returns it.
//! The field names, types and meanings below are stable within API major version 1: fields
//! may be added, never renamed or removed, and consumers must ignore unknown fields. An
//! optional field (`?`) is left out when unknown; `null` is never used.
//!
//! ```text
//! {
//!   "url": string,                  // the URL probed
//!   "id": string?, "title": string?, "uploader": string?, "thumbnail": string?,
//!   "duration": number?,            // seconds
//!   "formats": [ { "id": string, "ext": string, "kind": "combined"|"video_only"|"audio_only"|"other",
//!                  "vcodec": string?, "acodec": string?, "width": int?, "height": int?,
//!                  "fps": number?, "size": int?  /* bytes */, "note": string? } ],
//!   "subtitles": [string],          // uploaded subtitle languages
//!   "automatic_captions": [string],
//!   "playlist": bool,               // true: see "entries"; "formats" is empty
//!   "entries": [ { "index": int /* 1-based */, "title": string?, "duration": number? } ],
//!   "ffmpeg": bool,                 // ffmpeg usable on the daemon host (merging, conversion)
//!   "plans": [ { "preset": string, "kind": "merged"|"single"|"separate"|"audio",
//!                "selector": string, "explanation": string, "alternative": <plan>? } ],
//!   "video_choices": [<choice>], "audio_choices": [<choice>],  // quality picker entries
//!   "needs_choice": bool,           // the quality picker would be shown
//!   "suggested": { "video": string?, "audio": string? }
//! }
//! choice = { "id": string, "stream_id": string, "label": string,
//!            "kind": "muxed"|"video_only"|"audio_only",
//!            "height": int?, "fps": number?, "codec": string?, "size_estimate": int?,
//!            "requires_ffmpeg": bool, "available": bool, "unavailable_reason": string? }
//! ```
//!
//! `dman probe URL --plan --json` prints a [`VideoDownloadPlan`] (`video.plan`) instead:
//! what a download would use, without contacting the video site.

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
    /// `audio:m4a|mp3|opus`), a preset with an audio preference (`1080p+m4a`,
    /// `best+opus`) or a raw yt-dlp format selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Exact format id(s) from `video.probe` (e.g. `137+140`, `137+140/18`;
    /// any yt-dlp `-f` expression without spaces); wins over `quality`. A `+`
    /// merges streams and needs ffmpeg.
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
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoProbeParams {
    pub url: String,
    /// List the playlist entries instead of probing a single video.
    #[serde(default)]
    pub playlist: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies_from_browser: Option<String>,
}

impl std::fmt::Debug for VideoProbeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoProbeParams")
            .field("url", &crate::redact::redact_url(&self.url))
            .field("playlist", &self.playlist)
            .field("cookies_from_browser", &self.cookies_from_browser)
            .finish()
    }
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
    /// Picker entries for the video, best first (empty for playlists).
    #[serde(default)]
    pub video_choices: Vec<QualityChoice>,
    /// Picker entries for the audio track, best bitrate first.
    #[serde(default)]
    pub audio_choices: Vec<QualityChoice>,
    /// Show the picker: more than one meaningfully different video quality
    /// (height / frame-rate class) or more than one audio option. False for
    /// single-format sources such as direct file URLs. True for playlists
    /// (items are picked there; qualities come from `plans`).
    #[serde(default)]
    pub needs_choice: bool,
    /// What the current defaults would pick.
    #[serde(default)]
    pub suggested: QualityChoiceRef,
}

/// `video.plan` parameters: what `jobs.add` would be given for a video.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPlanParams {
    pub url: String,
    /// The options as the client would send them; unset fields take the daemon's
    /// `[video]` defaults, exactly as when the job is added.
    #[serde(default)]
    pub options: VideoOptions,
}

impl std::fmt::Debug for VideoPlanParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoPlanParams")
            .field("url", &crate::redact::redact_url(&self.url))
            .field("options", &self.options)
            .finish()
    }
}

/// Result of `video.plan`: what the daemon would run for these options with the
/// preferences and tools it has now. Nothing is downloaded or probed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoDownloadPlan {
    pub url: String,
    /// Where the format choice comes from: `format_id`, `audio_only`, `quality`
    /// (given in the request) or `defaults` (the saved `[video]` defaults).
    pub source: String,
    /// The quality preset or raw selector in effect; unset for an exact `format_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// The yt-dlp `-f` expression.
    pub selector: String,
    /// ffmpeg is available on the daemon host.
    pub ffmpeg: bool,
    /// Container yt-dlp is told to merge into; unset when none or when ffmpeg is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Audio codec the download is converted to (`-x`); unset when not converted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_extract: Option<String>,
    /// Whether the download is a playlist (`playlist_items` narrows it).
    #[serde(default)]
    pub playlist: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_items: Option<String>,
    /// yt-dlp output template, relative to the job directory.
    pub output_template: String,
    /// Post-processing steps that will run, in plain words.
    #[serde(default)]
    pub post_processing: Vec<String>,
    /// Steps that were asked for (or are in the defaults) but cannot run without ffmpeg.
    #[serde(default)]
    pub skipped: Vec<String>,
    /// `dman add` would show the quality picker (before the video is probed: the picker
    /// is skipped when the source has a single quality).
    #[serde(default)]
    pub asks_quality: bool,
    /// Why the picker would or would not be shown.
    #[serde(default)]
    pub ask_reason: String,
    /// Consequences worth knowing, e.g. a lower quality because ffmpeg is missing.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// What a quality choice downloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityChoiceKind {
    /// One pre-merged stream (or codecs unknown).
    #[default]
    Muxed,
    /// A video stream, combined with the preferred audio stream when there is one.
    VideoOnly,
    AudioOnly,
}

/// One entry of the quality picker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityChoice {
    /// yt-dlp format id, or `VID+AUD` for a combined choice: use as `VideoOptions::format_id`.
    pub id: String,
    /// The format id of the video (or audio) stream alone.
    #[serde(default)]
    pub stream_id: String,
    /// "1080p60 · VP9 · ~1.2 GiB"
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// Codec family: H.264, VP9, AV1, AAC, Opus, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// Bytes (video plus audio for a combined choice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_estimate: Option<u64>,
    pub kind: QualityChoiceKind,
    /// ffmpeg must merge video and audio.
    #[serde(default)]
    pub requires_ffmpeg: bool,
    /// False when it needs ffmpeg and ffmpeg is missing; see `unavailable_reason`.
    #[serde(default)]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// The choices the current defaults would pick (ids of `video_choices` and
/// `audio_choices`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityChoiceRef {
    /// `video_choices[].id`; unset for audio-only defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    /// `audio_choices[].id`: the audio track of a combined video choice or of
    /// an audio-only default; unset for a pre-merged video choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

/// `video.preferences.get` result and `video.preferences.set` result: the
/// `[video]` defaults of the quality picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPreferences {
    /// Show the picker before downloading (false = "never ask again").
    pub ask_quality: bool,
    /// Preset: `best`, `2160p`, `1440p`, `1080p`, `720p`, `480p`, `audio:m4a|mp3|opus`.
    pub default_quality: String,
    /// Preferred audio track when combining: `best`, `m4a`, `opus`, `mp3`.
    pub default_audio: String,
    /// `mp4`, `mkv` or `webm`; unset = yt-dlp's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_container: Option<String>,
}

/// `video.preferences.set` parameters: only the given fields change. They are
/// applied immediately and written to `[video]` in `config.toml` (comments
/// preserved).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPreferencesUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_quality: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_audio: Option<String>,
    /// An empty string clears the container default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_container: Option<String>,
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

    fn sorted_keys(v: &serde_json::Value) -> String {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k.join(",")
    }

    /// The documented `probe --json` keys; renaming or dropping one breaks scripts.
    #[test]
    fn probe_json_schema_is_stable() {
        let choice = QualityChoice {
            id: "137+140".into(),
            stream_id: "137".into(),
            label: "1080p".into(),
            height: Some(1080),
            fps: Some(30.0),
            codec: Some("H.264".into()),
            size_estimate: Some(1),
            kind: QualityChoiceKind::VideoOnly,
            requires_ffmpeg: true,
            available: true,
            unavailable_reason: Some("x".into()),
        };
        let plan = VideoPlan {
            preset: "best".into(),
            kind: VideoPlanKind::Merged,
            selector: "bv*+ba/b".into(),
            explanation: "e".into(),
            alternative: Some(Box::new(VideoPlan {
                preset: "best".into(),
                kind: VideoPlanKind::Single,
                selector: "b".into(),
                explanation: "e".into(),
                alternative: None,
            })),
        };
        let probe = VideoProbe {
            url: "https://e.com/v".into(),
            id: Some("i".into()),
            title: Some("t".into()),
            duration: Some(1.0),
            uploader: Some("u".into()),
            thumbnail: Some("th".into()),
            formats: vec![VideoFormat {
                id: "137".into(),
                ext: "mp4".into(),
                vcodec: Some("avc1".into()),
                acodec: Some("none".into()),
                width: Some(1920),
                height: Some(1080),
                fps: Some(30.0),
                size: Some(1),
                note: Some("n".into()),
                kind: VideoStreamKind::VideoOnly,
            }],
            subtitles: vec!["en".into()],
            automatic_captions: vec!["de".into()],
            playlist: false,
            entries: vec![PlaylistEntry {
                index: 1,
                title: Some("t".into()),
                duration: Some(1.0),
            }],
            ffmpeg: true,
            plans: vec![plan],
            video_choices: vec![choice.clone()],
            audio_choices: vec![choice],
            needs_choice: true,
            suggested: QualityChoiceRef {
                video: Some("v".into()),
                audio: Some("a".into()),
            },
        };
        let v = serde_json::to_value(&probe).unwrap();
        assert_eq!(
            sorted_keys(&v),
            "audio_choices,automatic_captions,duration,entries,ffmpeg,formats,id,needs_choice,\
             plans,playlist,subtitles,suggested,thumbnail,title,uploader,url,video_choices"
        );
        assert_eq!(
            sorted_keys(&v["formats"][0]),
            "acodec,ext,fps,height,id,kind,note,size,vcodec,width"
        );
        assert_eq!(v["formats"][0]["kind"], "video_only");
        assert_eq!(sorted_keys(&v["entries"][0]), "duration,index,title");
        assert_eq!(
            sorted_keys(&v["plans"][0]),
            "alternative,explanation,kind,preset,selector"
        );
        assert_eq!(v["plans"][0]["kind"], "merged");
        assert_eq!(
            sorted_keys(&v["video_choices"][0]),
            "available,codec,fps,height,id,kind,label,requires_ffmpeg,size_estimate,stream_id,\
             unavailable_reason"
        );
        assert_eq!(sorted_keys(&v["suggested"]), "audio,video");
        assert_eq!(serde_json::from_value::<VideoProbe>(v).unwrap(), probe);
    }

    #[test]
    fn plan_json_schema_is_stable() {
        let plan = VideoDownloadPlan {
            url: "https://e.com/v".into(),
            source: "defaults".into(),
            quality: Some("1080p".into()),
            selector: "b".into(),
            ffmpeg: true,
            container: Some("mkv".into()),
            audio_extract: Some("mp3".into()),
            playlist: true,
            playlist_items: Some("1-2".into()),
            output_template: "%(title)s.%(ext)s".into(),
            post_processing: vec!["a".into()],
            skipped: vec!["b".into()],
            asks_quality: true,
            ask_reason: "r".into(),
            notes: vec!["n".into()],
        };
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            sorted_keys(&v),
            "ask_reason,asks_quality,audio_extract,container,ffmpeg,notes,output_template,\
             playlist,playlist_items,post_processing,quality,selector,skipped,source,url"
        );
        assert_eq!(
            serde_json::from_value::<VideoDownloadPlan>(v).unwrap(),
            plan
        );
        let p: VideoPlanParams = serde_json::from_str(r#"{"url":"https://e.com/v"}"#).unwrap();
        assert_eq!(p.options, VideoOptions::default());
    }

    #[test]
    fn old_probe_json_still_parses() {
        let p: VideoProbe = serde_json::from_str(r#"{"url":"https://e.com/v"}"#).unwrap();
        assert!(p.video_choices.is_empty() && !p.needs_choice);
        assert_eq!(p.suggested, QualityChoiceRef::default());
        let u: VideoPreferencesUpdate = serde_json::from_str("{}").unwrap();
        assert_eq!(u, VideoPreferencesUpdate::default());
    }
}
