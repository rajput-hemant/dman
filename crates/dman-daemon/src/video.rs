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
    PlaylistEntry, VideoFormat, VideoPlan, VideoPlanKind, VideoProbe, VideoProbeParams,
    VideoStreamKind,
};
use dman_proto::{Engine, ErrorCode, RpcError, VideoOptions, VideoSubtitles};
use dman_ytdlp::{
    AudioCodec, Container, DownloadSpec, Plan, PlanKind, ProbeOptions, ProbeResult, QualityPreset,
    SponsorBlock, StreamKind, SubOpts, Tools, VideoInfo,
};
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
        Ok(probe_dto(url.as_str(), res, &tools))
    }
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

/// Converts a probe result into the API type.
pub fn probe_dto(url: &str, res: ProbeResult, tools: &Tools) -> VideoProbe {
    let mut out = VideoProbe {
        url: url.to_owned(),
        ffmpeg: tools.merge_supported(),
        ..VideoProbe::default()
    };
    match res {
        ProbeResult::Video(v) => {
            out.plans = plans(&v, tools);
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
        o.quality = Some(d.default_quality.clone());
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
        spec.selector = id.to_owned();
    } else if let Some(q) = nonempty(&o.quality) {
        match QualityPreset::from_str(q) {
            Ok(preset @ QualityPreset::AudioOnly { codec }) => {
                spec.selector = dman_ytdlp::selector(preset, merge);
                spec.audio_extract = merge.then_some(codec);
            }
            Ok(preset) => {
                spec.selector = dman_ytdlp::selector(preset, merge || o.allow_separate_streams);
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
        let p = probe_dto("https://e.com/v", info, &tools(false));
        assert_eq!(p.title.as_deref(), Some("T"));
        assert_eq!(p.formats.len(), 1);
        assert_eq!(p.formats[0].kind, VideoStreamKind::Combined);
        assert_eq!(p.plans.len(), QualityPreset::ALL.len());
        assert_eq!(p.plans[0].preset, "best");
        assert!(!p.ffmpeg);
    }
}
