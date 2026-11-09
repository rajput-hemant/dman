//! The optional yt-dlp (+ffmpeg) video engine: tool detection, engine status,
//! `video.probe`, and turning a job's stored [`VideoOptions`] into a
//! [`DownloadSpec`]. The job lifecycle itself lives in `manager::video`.
//!
//! ffmpeg is reported as a feature of the yt-dlp engine entry (`engine.status`
//! has no separate ffmpeg entry): `features` contains `"ffmpeg <version>"` and
//! `"merge"` when it is present; otherwise `reason` explains what is missing
//! while yt-dlp itself stays `available`.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use dman_config::Paths;
use dman_proto::method::EngineStatus;
use dman_proto::video::{
    PlaylistEntry, QualityChoice, QualityChoiceKind, QualityChoiceRef, VideoFormat, VideoPlan,
    VideoPlanKind, VideoPreferences, VideoPreferencesUpdate, VideoProbe, VideoProbeParams,
    VideoStreamKind,
};
use dman_proto::{Engine, ErrorCode, RpcError, VideoOptions, VideoSubtitles};
use dman_ytdlp::{
    AudioCodec, AudioPref, Choice, ChoiceDefaults, ChoiceKind, Container, DownloadSpec, Plan,
    PlanKind, ProbeOptions, ProbeResult, QualityPreset, SponsorBlock, StreamKind, SubOpts, Tools,
    VideoInfo,
};
use std::sync::Arc;
use tokio::sync::watch;
use url::Url;

/// `video.probe` gives up after this long.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

const INSTALL_HINT: &str = "install yt-dlp: pipx install yt-dlp / brew install yt-dlp \
     (or set video.ytdlp_path), then run `dman daemon engine-restart`";

/// Detected tools, shared with the job manager (which owns the video jobs)
/// and the API server (which runs probes off the manager).
#[derive(Debug)]
pub struct VideoService {
    tools: watch::Sender<Tools>,
    config: Mutex<dman_config::Video>,
    paths: Option<Paths>,
}

fn expand(p: &Option<String>) -> Option<PathBuf> {
    p.as_deref().map(dman_config::expand_tilde)
}

impl VideoService {
    /// Detects yt-dlp and ffmpeg (a few seconds at most; missing tools are fine).
    pub async fn start(config: &dman_config::Video, paths: Option<Paths>) -> Self {
        let tools = Tools::detect(expand(&config.ytdlp_path), expand(&config.ffmpeg_path)).await;
        log_tools(&tools);
        Self {
            tools: watch::Sender::new(tools),
            config: Mutex::new(config.clone()),
            paths,
        }
    }

    /// Current tools; the receiver sees re-detections.
    pub fn subscribe(&self) -> watch::Receiver<Tools> {
        self.tools.subscribe()
    }

    /// Current tools.
    pub fn tools(&self) -> Tools {
        self.tools.borrow().clone()
    }

    /// Re-reads `[video]` from the config file (so a fixed `ytdlp_path` is
    /// picked up) and detects the tools again (`engine.restart`).
    pub async fn redetect(&self) -> Tools {
        if let Some(paths) = &self.paths
            && paths.config_file().exists()
            && let Ok(cfg) = dman_config::Config::load(paths)
            && cfg.video.validate().is_ok()
            && let Ok(mut c) = self.config.lock()
        {
            *c = cfg.video;
        }
        let (y, f) = match self.config.lock() {
            Ok(c) => (expand(&c.ytdlp_path), expand(&c.ffmpeg_path)),
            Err(_) => (None, None),
        };
        let tools = Tools::detect(y, f).await;
        log_tools(&tools);
        self.tools.send_replace(tools.clone());
        tools
    }

    /// Replaces the remembered `[video]` section (`config.reload`).
    pub(crate) fn apply_config(&self, cfg: &dman_config::Video) {
        if let Ok(mut c) = self.config.lock() {
            *c = cfg.clone();
        }
    }

    fn config_now(&self) -> dman_config::Video {
        self.config
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// `video.preferences.get`.
    pub fn preferences(&self) -> VideoPreferences {
        preferences_of(&self.config_now())
    }

    /// The engine status entry of yt-dlp.
    pub fn status(&self) -> EngineStatus {
        engine_status(&self.tools.borrow())
    }

    /// `video.probe`. Dropping the future kills yt-dlp (client disconnect).
    pub async fn probe(&self, p: VideoProbeParams) -> Result<VideoProbe, RpcError> {
        let url = parse_url(&p.url)?;
        let tools = self.tools();
        if tools.ytdlp.is_none() {
            return Err(unavailable());
        }
        if let Some(b) = &p.cookies_from_browser
            && (b.trim().is_empty() || b.starts_with('-'))
        {
            return Err(RpcError::invalid("invalid cookies_from_browser value"));
        }
        let opts = ProbeOptions {
            cookies_from_browser: p.cookies_from_browser.clone(),
            playlist: p.playlist,
            timeout: PROBE_TIMEOUT,
        };
        let res = dman_ytdlp::probe(&tools, &url, &opts)
            .await
            .map_err(|e| match e {
                dman_ytdlp::Error::YtdlpMissing => unavailable(),
                dman_ytdlp::Error::Timeout(d) => RpcError::new(
                    ErrorCode::General,
                    format!("yt-dlp did not answer within {} s", d.as_secs()),
                ),
                dman_ytdlp::Error::Failed { message, .. } => {
                    RpcError::new(ErrorCode::General, message)
                }
                other => RpcError::new(ErrorCode::General, other.to_string()),
            })?;
        Ok(probe_dto(url.as_str(), res, &tools, &self.config_now()))
    }
}

fn preferences_of(c: &dman_config::Video) -> VideoPreferences {
    VideoPreferences {
        ask_quality: c.ask_quality,
        default_quality: c.default_quality.clone(),
        default_audio: c.default_audio.clone(),
        default_container: c.default_container.clone(),
    }
}

fn general(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::General, e.to_string())
}

/// `video.preferences.set`: validates, writes the given keys to `[video]` in
/// `config.toml` (comments preserved) and applies them at once to the jobs
/// added from now on. Nothing changes on any error.
pub(crate) async fn set_preferences(
    state: &Arc<crate::server::ServerState>,
    u: VideoPreferencesUpdate,
) -> Result<VideoPreferences, RpcError> {
    let svc = &state.video;
    let Some(paths) = svc.paths.as_ref() else {
        return Err(general("the daemon has no config file to save to"));
    };
    let mut new = svc.config_now();
    let mut keys: Vec<(&str, Result<bool, String>)> = Vec::new();
    let mut clear_container = false;
    if let Some(a) = u.ask_quality {
        new.ask_quality = a;
        keys.push(("ask_quality", Ok(a)));
    }
    if let Some(q) = nonempty(&u.default_quality) {
        let preset = QualityPreset::from_str(q).map_err(|e| RpcError::invalid(e.to_string()))?;
        new.default_quality = preset.name();
        keys.push(("default_quality", Err(new.default_quality.clone())));
    }
    if let Some(a) = nonempty(&u.default_audio) {
        let pref = AudioPref::from_str(a).map_err(|e| RpcError::invalid(e.to_string()))?;
        new.default_audio = pref.as_str().to_owned();
        keys.push(("default_audio", Err(new.default_audio.clone())));
    }
    if let Some(c) = &u.default_container {
        if c.trim().is_empty() {
            new.default_container = None;
            clear_container = true;
        } else {
            let c = Container::from_str(c).map_err(|e| RpcError::invalid(e.to_string()))?;
            new.default_container = Some(c.as_str().to_owned());
            keys.push(("default_container", Err(c.as_str().to_owned())));
        }
    }
    new.validate().map_err(RpcError::invalid)?;
    if !keys.is_empty() || clear_container {
        let mut doc = dman_config::ConfigDoc::open(paths).map_err(general)?;
        for (k, v) in keys {
            match v {
                Ok(b) => doc.set(&["video", k], b),
                Err(t) => doc.set(&["video", k], t),
            }
            .map_err(general)?;
        }
        if clear_container {
            doc.remove(&["video", "default_container"])
                .map_err(general)?;
        }
        doc.save().map_err(general)?;
    }
    state
        .manager
        .call(crate::manager::Request::SetVideoConfig(Box::new(
            new.clone(),
        )))
        .await?;
    svc.apply_config(&new);
    state.reload.note_video(&new);
    Ok(preferences_of(&new))
}

fn log_tools(t: &Tools) {
    match (&t.ytdlp, &t.ffmpeg) {
        (None, _) => tracing::info!("yt-dlp not found; video downloads are unavailable"),
        (Some(y), None) => {
            tracing::info!(version = %y.version, "yt-dlp found; ffmpeg missing (no merging)");
        }
        (Some(y), Some(f)) => {
            tracing::info!(ytdlp = %y.version, ffmpeg = %f.version, "video engine ready");
        }
    }
}

/// `EngineUnavailable` for video operations without yt-dlp.
pub fn unavailable() -> RpcError {
    RpcError::new(
        ErrorCode::EngineUnavailable,
        format!("yt-dlp is not available, so video downloads are disabled: {INSTALL_HINT}"),
    )
}

/// The `engine.status` entry of the video engine (see the module docs).
pub fn engine_status(t: &Tools) -> EngineStatus {
    let Some(y) = &t.ytdlp else {
        return EngineStatus {
            engine: Engine::YtDlp,
            available: false,
            version: None,
            features: Vec::new(),
            reason: Some(format!(
                "yt-dlp not found, video downloads are disabled; {INSTALL_HINT}"
            )),
        };
    };
    let mut features = Vec::new();
    let mut notes = Vec::new();
    match &t.ffmpeg {
        Some(f) => {
            features.push(format!("ffmpeg {}", f.version));
            features.push("merge".to_owned());
        }
        None => notes.push(
            "ffmpeg not found: separate video and audio streams cannot be merged (the best \
             single-file format is used instead), audio is not converted and nothing is \
             embedded; install ffmpeg or set video.ffmpeg_path"
                .to_owned(),
        ),
    }
    if let Some(w) = t.ytdlp_age_warning() {
        notes.push(w);
    }
    EngineStatus {
        engine: Engine::YtDlp,
        available: true,
        version: Some(y.version.clone()),
        features,
        reason: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

fn parse_url(s: &str) -> Result<Url, RpcError> {
    let url = Url::parse(s.trim()).map_err(|e| RpcError::invalid(format!("invalid URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(RpcError::invalid("video URLs must be http or https"));
    }
    Ok(url)
}

fn stream_kind(k: StreamKind) -> VideoStreamKind {
    match k {
        StreamKind::Combined => VideoStreamKind::Combined,
        StreamKind::VideoOnly => VideoStreamKind::VideoOnly,
        StreamKind::AudioOnly => VideoStreamKind::AudioOnly,
        StreamKind::Other => VideoStreamKind::Other,
    }
}

fn plan_dto(preset: &str, p: &Plan) -> VideoPlan {
    VideoPlan {
        preset: preset.to_owned(),
        kind: match p.kind {
            PlanKind::Merged => VideoPlanKind::Merged,
            PlanKind::SingleStream => VideoPlanKind::Single,
            PlanKind::SeparateStreams => VideoPlanKind::Separate,
            PlanKind::AudioOnly => VideoPlanKind::Audio,
        },
        selector: p.selector.clone(),
        explanation: p.explanation.clone(),
        alternative: p
            .alternative
            .as_ref()
            .map(|a| Box::new(plan_dto(preset, a))),
    }
}

/// One plan per preset for `info` (an empty info for playlists).
pub fn plans(info: &VideoInfo, tools: &Tools) -> Vec<VideoPlan> {
    QualityPreset::ALL
        .iter()
        .map(|&p| plan_dto(&p.name(), &dman_ytdlp::fallback_plan(info, p, tools)))
        .collect()
}

fn choice_dto(c: &Choice) -> QualityChoice {
    QualityChoice {
        id: c.id.clone(),
        stream_id: c.stream_id.clone(),
        label: c.label.clone(),
        height: c.height,
        fps: c.fps,
        codec: c.codec.clone(),
        size_estimate: c.size_estimate,
        kind: match c.kind {
            ChoiceKind::Muxed => QualityChoiceKind::Muxed,
            ChoiceKind::VideoOnly => QualityChoiceKind::VideoOnly,
            ChoiceKind::AudioOnly => QualityChoiceKind::AudioOnly,
        },
        requires_ffmpeg: c.requires_ffmpeg,
        available: c.available,
        unavailable_reason: c.unavailable_reason.clone(),
    }
}

/// The preset and audio preference of the `[video]` defaults (invalid values,
/// which validation rejects, fall back to best).
fn default_choice_prefs(d: &dman_config::Video) -> ChoiceDefaults {
    ChoiceDefaults {
        preset: QualityPreset::from_str(&d.default_quality).unwrap_or(QualityPreset::Best),
        audio: AudioPref::from_str(&d.default_audio).unwrap_or_default(),
    }
}

fn apply_choices(out: &mut VideoProbe, v: &VideoInfo, tools: &Tools, d: &dman_config::Video) {
    let c = dman_ytdlp::build_choices(v, tools, default_choice_prefs(d));
    out.video_choices = c.video.iter().map(choice_dto).collect();
    out.audio_choices = c.audio.iter().map(choice_dto).collect();
    out.needs_choice = c.needs_choice;
    out.suggested = QualityChoiceRef {
        video: c.suggested.video,
        audio: c.suggested.audio,
    };
}

/// Converts a probe result into the API type.
pub fn probe_dto(
    url: &str,
    res: ProbeResult,
    tools: &Tools,
    defaults: &dman_config::Video,
) -> VideoProbe {
    let mut out = VideoProbe {
        url: url.to_owned(),
        ffmpeg: tools.merge_supported(),
        ..VideoProbe::default()
    };
    match res {
        ProbeResult::Video(v) => {
            out.plans = plans(&v, tools);
            apply_choices(&mut out, &v, tools, defaults);
            out.id = Some(v.id).filter(|s| !s.is_empty());
            out.title = Some(v.title).filter(|s| !s.is_empty());
            out.duration = v.duration;
            out.uploader = v.uploader;
            out.thumbnail = v.thumbnail;
            out.subtitles = v.subtitles;
            out.automatic_captions = v.automatic_captions;
            out.formats = v
                .formats
                .iter()
                .map(|f| VideoFormat {
                    id: f.format_id.clone(),
                    ext: f.ext.clone(),
                    vcodec: f.vcodec.clone(),
                    acodec: f.acodec.clone(),
                    width: f.width,
                    height: f.height,
                    fps: f.fps,
                    size: f.size(),
                    note: f.note.clone(),
                    kind: stream_kind(f.kind()),
                })
                .collect();
        }
        ProbeResult::Playlist { id, title, entries } => {
            let empty = VideoInfo {
                id: String::new(),
                title: String::new(),
                duration: None,
                uploader: None,
                thumbnail: None,
                formats: Vec::new(),
                subtitles: Vec::new(),
                automatic_captions: Vec::new(),
            };
            out.plans = plans(&empty, tools);
            out.playlist = true;
            out.needs_choice = true;
            out.id = id;
            out.title = title;
            out.entries = entries
                .into_iter()
                .map(|e| PlaylistEntry {
                    index: e.index,
                    title: e.title,
                    duration: e.duration,
                })
                .collect();
        }
    }
    out
}

/// Fills unset options from the `[video]` defaults. The result is what gets
/// stored with the job, so later config changes do not alter it.
pub fn resolve_options(mut o: VideoOptions, d: &dman_config::Video) -> VideoOptions {
    let blank = |s: &Option<String>| s.as_deref().is_none_or(|s| s.trim().is_empty());
    if blank(&o.quality) && blank(&o.format_id) && blank(&o.audio_only) {
        // The saved defaults: preset plus audio preference, e.g. `1080p+m4a`.
        o.quality = Some(
            match (
                QualityPreset::from_str(&d.default_quality),
                AudioPref::from_str(&d.default_audio),
            ) {
                (Ok(p), Ok(a)) => dman_ytdlp::quality_string(p, a),
                _ => d.default_quality.clone(),
            },
        );
    }
    if blank(&o.container) {
        o.container = d.default_container.clone();
    }
    if blank(&o.output_template) {
        o.output_template = Some(d.output_template.clone());
    }
    if o.sponsorblock_remove.is_none() && !d.sponsorblock_remove.is_empty() {
        o.sponsorblock_remove = Some(d.sponsorblock_remove.clone());
    }
    o.embed_metadata.get_or_insert(d.embed_metadata);
    o.embed_thumbnail.get_or_insert(d.embed_thumbnail);
    if o.subtitles.is_none() && !d.subtitles.langs.is_empty() {
        o.subtitles = Some(VideoSubtitles {
            langs: d.subtitles.langs.clone(),
            embed: d.subtitles.embed,
            auto: false,
        });
    }
    if blank(&o.cookies_from_browser) {
        o.cookies_from_browser = d.cookies_from_browser.clone();
    }
    if o.playlist_items.is_some() {
        o.playlist = true;
    }
    o
}

fn nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn invalid(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The yt-dlp spec for a job: `url` into `dir` with the stored options, the
/// job's rate limit and the tools detected now (the format selector honours
/// ffmpeg: without it only single-file formats are requested unless separate
/// streams were allowed). Errors are user-facing validation messages.
pub fn build_spec(
    url: &str,
    dir: &Path,
    o: &VideoOptions,
    rate_limit: Option<u64>,
    tools: &Tools,
) -> Result<DownloadSpec, String> {
    let url = parse_url(url).map_err(|e| e.message)?;
    let merge = tools.merge_supported();
    let mut spec = DownloadSpec::new(url, dir);
    if let Some(codec) = nonempty(&o.audio_only) {
        let codec = AudioCodec::from_str(codec).map_err(invalid)?;
        let preset = QualityPreset::AudioOnly { codec };
        spec.selector = dman_ytdlp::selector(preset, merge);
        spec.audio_extract = merge.then_some(codec);
    } else if let Some(id) = nonempty(&o.format_id) {
        if id.chars().any(char::is_whitespace) {
            return Err("format_id must not contain spaces".into());
        }
        if id.contains('+') && !merge && !o.allow_separate_streams {
            return Err(format!(
                "format {id} merges separate video and audio streams, which needs ffmpeg \
                 (not found); install ffmpeg, pick a single-file format or allow separate files"
            ));
        }
        spec.selector = id.to_owned();
    } else if let Some(q) = nonempty(&o.quality) {
        match dman_ytdlp::parse_quality(q) {
            Ok((preset @ QualityPreset::AudioOnly { codec }, _)) => {
                spec.selector = dman_ytdlp::selector(preset, merge);
                spec.audio_extract = merge.then_some(codec);
            }
            Ok((preset, audio)) => {
                spec.selector = dman_ytdlp::selector_with_audio(
                    preset,
                    merge || o.allow_separate_streams,
                    audio,
                );
            }
            Err(e)
                if q.split_once('+')
                    .is_some_and(|(p, _)| QualityPreset::from_str(p).is_ok()) =>
            {
                return Err(invalid(e));
            }
            Err(_) => {
                // A raw yt-dlp selector.
                if q.chars().any(char::is_whitespace) {
                    return Err(format!(
                        "unknown quality {q:?}: use best, 2160p, 1440p, 1080p, 720p, 480p, \
                         audio:m4a|mp3|opus or a yt-dlp format selector"
                    ));
                }
                spec.selector = q.to_owned();
            }
        }
    }
    if let Some(c) = nonempty(&o.container) {
        spec.container = Some(Container::from_str(c).map_err(invalid)?);
    }
    spec.playlist = o.playlist || o.playlist_items.is_some();
    spec.playlist_items = o.playlist_items.clone();
    if let Some(s) = &o.subtitles
        && !s.langs.is_empty()
    {
        spec.subtitles = Some(SubOpts {
            langs: s.langs.clone(),
            embed: s.embed,
            auto: s.auto,
        });
    }
    spec.embed_thumbnail = o.embed_thumbnail.unwrap_or(false);
    spec.embed_metadata = o.embed_metadata.unwrap_or(false);
    if let Some(t) = nonempty(&o.output_template) {
        spec.output_template = t.to_owned();
    }
    spec.cookies_from_browser = nonempty(&o.cookies_from_browser).map(str::to_owned);
    if let Some(sb) = &o.sponsorblock_remove
        && !sb.is_empty()
    {
        spec.sponsorblock = Some(SponsorBlock {
            remove: sb.clone(),
            mark: Vec::new(),
        });
    }
    spec.rate_limit = rate_limit.filter(|r| *r > 0);
    spec.extra_args = o.extra_args.clone();
    spec.validate().map_err(invalid)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_ytdlp::ToolInfo;

    fn tools(ffmpeg: bool) -> Tools {
        Tools {
            ytdlp: Some(ToolInfo {
                path: "yt-dlp".into(),
                version: "2026.10.26".into(),
            }),
            ffmpeg: ffmpeg.then(|| ToolInfo {
                path: "ffmpeg".into(),
                version: "7.0".into(),
            }),
        }
    }

    #[test]
    fn status_explains_missing_tools() {
        let s = engine_status(&Tools::default());
        assert!(!s.available);
        let r = s.reason.unwrap();
        assert!(r.contains("pipx install yt-dlp") && r.contains("brew install yt-dlp"));
        let s = engine_status(&tools(false));
        assert!(s.available);
        assert!(s.reason.unwrap().contains("ffmpeg not found"));
        assert!(!s.features.iter().any(|f| f == "merge"));
        let s = engine_status(&tools(true));
        assert!(s.reason.is_none());
        assert!(s.features.iter().any(|f| f == "merge"));
    }

    #[test]
    fn defaults_fill_unset_options() {
        let d = dman_config::Video {
            default_quality: "720p".into(),
            default_container: Some("mkv".into()),
            sponsorblock_remove: vec!["sponsor".into()],
            ..Default::default()
        };
        let o = resolve_options(VideoOptions::default(), &d);
        assert_eq!(o.quality.as_deref(), Some("720p"));
        assert_eq!(o.container.as_deref(), Some("mkv"));
        assert_eq!(o.embed_metadata, Some(true));
        assert_eq!(o.sponsorblock_remove, Some(vec!["sponsor".to_owned()]));
        let o = resolve_options(
            VideoOptions {
                audio_only: Some("mp3".into()),
                playlist_items: Some("1-2".into()),
                embed_metadata: Some(false),
                ..Default::default()
            },
            &d,
        );
        assert_eq!(o.quality, None);
        assert!(o.playlist);
        assert_eq!(o.embed_metadata, Some(false));
    }

    #[test]
    fn spec_honours_ffmpeg_and_consent() {
        let dir = Path::new("/dl");
        let o = VideoOptions {
            quality: Some("1080p".into()),
            ..Default::default()
        };
        let s = build_spec("https://e.com/v", dir, &o, Some(100), &tools(true)).unwrap();
        assert!(s.selector.contains('+'), "{}", s.selector);
        assert_eq!(s.rate_limit, Some(100));
        let s = build_spec("https://e.com/v", dir, &o, Some(0), &tools(false)).unwrap();
        assert!(!s.selector.contains('+'), "{}", s.selector);
        assert_eq!(s.rate_limit, None);
        let consent = VideoOptions {
            allow_separate_streams: true,
            ..o.clone()
        };
        let s = build_spec("https://e.com/v", dir, &consent, None, &tools(false)).unwrap();
        assert!(s.selector.contains('+'), "{}", s.selector);
        let audio = VideoOptions {
            quality: Some("audio:mp3".into()),
            ..Default::default()
        };
        let s = build_spec("https://e.com/v", dir, &audio, None, &tools(true)).unwrap();
        assert_eq!(s.audio_extract, Some(AudioCodec::Mp3));
        let s = build_spec("https://e.com/v", dir, &audio, None, &tools(false)).unwrap();
        assert_eq!(s.audio_extract, None);
        let raw = VideoOptions {
            format_id: Some("137+140".into()),
            ..Default::default()
        };
        let s = build_spec("https://e.com/v", dir, &raw, None, &tools(true)).unwrap();
        assert_eq!(s.selector, "137+140");
    }

    #[test]
    fn saved_defaults_become_quality_with_audio_preference() {
        let d = dman_config::Video {
            default_quality: "1080p".into(),
            default_audio: "m4a".into(),
            ..Default::default()
        };
        let o = resolve_options(VideoOptions::default(), &d);
        assert_eq!(o.quality.as_deref(), Some("1080p+m4a"));
        let dir = Path::new("/dl");
        let s = build_spec("https://e.com/v", dir, &o, None, &tools(true)).unwrap();
        assert_eq!(
            s.selector,
            "bv*[height<=1080]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]"
        );
        // Without ffmpeg the preference cannot apply: single-file formats only.
        let s = build_spec("https://e.com/v", dir, &o, None, &tools(false)).unwrap();
        assert_eq!(s.selector, "b[height<=1080]/b");
        // Best audio keeps the plain preset; an explicit quality is left alone.
        let d2 = dman_config::Video {
            default_quality: "720p".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_options(VideoOptions::default(), &d2)
                .quality
                .as_deref(),
            Some("720p")
        );
        let explicit = VideoOptions {
            quality: Some("480p".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_options(explicit, &d).quality.as_deref(),
            Some("480p")
        );
        // Audio presets take no preference.
        let a = dman_config::Video {
            default_quality: "audio:mp3".into(),
            default_audio: "opus".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_options(VideoOptions::default(), &a)
                .quality
                .as_deref(),
            Some("audio:mp3")
        );
        // A typo in the audio part is reported, not passed on as a selector.
        let bad = VideoOptions {
            quality: Some("1080p+flac".into()),
            ..Default::default()
        };
        assert!(build_spec("https://e.com/v", dir, &bad, None, &tools(true)).is_err());
    }

    #[test]
    fn combined_format_ids_need_ffmpeg_or_consent() {
        let dir = Path::new("/dl");
        let o = VideoOptions {
            format_id: Some("137+140".into()),
            ..Default::default()
        };
        let e = build_spec("https://e.com/v", dir, &o, None, &tools(false)).unwrap_err();
        assert!(e.contains("ffmpeg"), "{e}");
        let consent = VideoOptions {
            allow_separate_streams: true,
            ..o.clone()
        };
        assert!(build_spec("https://e.com/v", dir, &consent, None, &tools(false)).is_ok());
        let single = VideoOptions {
            format_id: Some("18".into()),
            ..Default::default()
        };
        assert!(build_spec("https://e.com/v", dir, &single, None, &tools(false)).is_ok());
        let fb = VideoOptions {
            format_id: Some("137+140/18".into()),
            ..Default::default()
        };
        assert_eq!(
            build_spec("https://e.com/v", dir, &fb, None, &tools(true))
                .unwrap()
                .selector,
            "137+140/18"
        );
    }

    #[test]
    fn spec_validation_errors() {
        let dir = Path::new("/dl");
        let t = tools(true);
        let bad = [
            VideoOptions {
                output_template: Some("../x".into()),
                ..Default::default()
            },
            VideoOptions {
                container: Some("avi".into()),
                ..Default::default()
            },
            VideoOptions {
                extra_args: vec!["--exec".into(), "rm".into()],
                ..Default::default()
            },
            VideoOptions {
                quality: Some("not a preset".into()),
                ..Default::default()
            },
            VideoOptions {
                playlist_items: Some("1;2".into()),
                ..Default::default()
            },
            VideoOptions {
                sponsorblock_remove: Some(vec!["nope".into()]),
                ..Default::default()
            },
        ];
        for o in bad {
            assert!(
                build_spec("https://e.com/v", dir, &o, None, &t).is_err(),
                "{o:?}"
            );
        }
        assert!(build_spec("ftp://e.com/v", dir, &VideoOptions::default(), None, &t).is_err());
    }

    #[test]
    fn probe_dto_lists_plans() {
        let info = dman_ytdlp::parse_probe_json(
            br#"{"id":"x","title":"T","formats":[{"format_id":"18","ext":"mp4","vcodec":"avc1","acodec":"mp4a","height":360}]}"#,
        )
        .unwrap();
        let p = probe_dto(
            "https://e.com/v",
            info,
            &tools(false),
            &dman_config::Video::default(),
        );
        assert_eq!(p.title.as_deref(), Some("T"));
        assert_eq!(p.formats.len(), 1);
        assert_eq!(p.formats[0].kind, VideoStreamKind::Combined);
        assert_eq!(p.plans.len(), QualityPreset::ALL.len());
        assert_eq!(p.plans[0].preset, "best");
        assert!(!p.ffmpeg);
        // A single-format source needs no picker.
        assert!(!p.needs_choice);
        assert_eq!(p.video_choices.len(), 1);
        assert_eq!(p.suggested.video.as_deref(), Some("18"));
    }
}
