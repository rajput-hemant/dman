use std::path::PathBuf;

use url::Url;

use crate::error::{Error, Result};
use crate::format::{AudioCodec, Container, Plan};
use crate::tools::Tools;

pub const DEFAULT_TEMPLATE: &str = "%(title)s [%(id)s].%(ext)s";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubOpts {
    pub langs: Vec<String>,
    pub embed: bool,
    /// Also fetch automatic captions.
    pub auto: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SponsorBlock {
    pub remove: Vec<String>,
    pub mark: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DownloadSpec {
    pub url: Url,
    pub dir: PathBuf,
    /// yt-dlp output template relative to `dir` (no absolute paths, no `..`).
    pub output_template: String,
    /// `-f` expression, see [`crate::selector`] / [`crate::fallback_plan`].
    pub selector: String,
    /// `--merge-output-format` (only used with ffmpeg).
    pub container: Option<Container>,
    /// `-x --audio-format` (only with ffmpeg).
    pub audio_extract: Option<AudioCodec>,
    /// Download a playlist (`--yes-playlist`) instead of `--no-playlist`.
    pub playlist: bool,
    /// `--playlist-items` spec such as "1-3,7" (playlist only).
    pub playlist_items: Option<String>,
    pub subtitles: Option<SubOpts>,
    pub embed_thumbnail: bool,
    pub embed_metadata: bool,
    pub cookies_from_browser: Option<String>,
    /// Bytes per second.
    pub rate_limit: Option<u64>,
    pub sponsorblock: Option<SponsorBlock>,
    /// Extra yt-dlp flags, checked by [`validate_extra_args`].
    pub extra_args: Vec<String>,
}

impl DownloadSpec {
    pub fn new(url: Url, dir: impl Into<PathBuf>) -> Self {
        Self {
            url,
            dir: dir.into(),
            output_template: DEFAULT_TEMPLATE.into(),
            selector: "b".into(),
            container: None,
            audio_extract: None,
            playlist: false,
            playlist_items: None,
            subtitles: None,
            embed_thumbnail: false,
            embed_metadata: false,
            cookies_from_browser: None,
            rate_limit: None,
            sponsorblock: None,
            extra_args: vec![],
        }
    }

    /// Take selector and audio extraction from a [`Plan`].
    pub fn apply_plan(&mut self, plan: &Plan) {
        self.selector = plan.selector.clone();
        self.audio_extract = plan.audio_extract;
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::InvalidSpec(m.into()));
        if !matches!(self.url.scheme(), "http" | "https") {
            return bad("url must be http(s)");
        }
        let t = &self.output_template;
        if t.is_empty()
            || t.starts_with('/')
            || t.starts_with('\\')
            || t.contains('\0')
            || t.split(['/', '\\']).any(|c| c == "..")
            || t.as_bytes().get(1) == Some(&b':')
        {
            return bad("output template must be a relative path without '..'");
        }
        if self.selector.is_empty() || self.selector.starts_with('-') {
            return bad("invalid format selector");
        }
        if let Some(c) = &self.cookies_from_browser
            && (c.is_empty() || c.starts_with('-'))
        {
            return bad("invalid cookies-from-browser value");
        }
        if let Some(p) = &self.playlist_items
            && (p.is_empty() || !p.chars().all(|c| c.is_ascii_digit() || ",-:".contains(c)))
        {
            return bad("invalid playlist items");
        }
        if let Some(s) = &self.subtitles
            && (s.langs.is_empty()
                || !s.langs.iter().all(|l| {
                    !l.is_empty()
                        && !l.starts_with('-')
                        && l.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.*".contains(c))
                }))
        {
            return bad("invalid subtitle languages");
        }
        if let Some(sb) = &self.sponsorblock {
            const CATS: &[&str] = &[
                "sponsor",
                "intro",
                "outro",
                "selfpromo",
                "preview",
                "filler",
                "interaction",
                "music_offtopic",
                "poi_highlight",
                "chapter",
                "all",
            ];
            if !sb
                .remove
                .iter()
                .chain(&sb.mark)
                .all(|c| CATS.contains(&c.as_str()))
            {
                return bad("unknown sponsorblock category");
            }
        }
        validate_extra_args(&self.extra_args)
    }
}

/// Long flags without a value that callers may pass through.
const FLAGS: &[&str] = &[
    "--no-mtime",
    "--restrict-filenames",
    "--windows-filenames",
    "--no-overwrites",
    "--geo-bypass",
    "--force-ipv4",
    "--force-ipv6",
    "--write-description",
    "--write-info-json",
    "--write-thumbnail",
    "--embed-chapters",
    "--prefer-free-formats",
    "--ignore-errors",
    "--playlist-reverse",
    "--no-write-comments",
    "--xattrs",
];

/// Long flags (and short aliases) taking exactly one value.
const VALUE_FLAGS: &[&str] = &[
    "--proxy",
    "--retries",
    "-R",
    "--fragment-retries",
    "--extractor-retries",
    "--concurrent-fragments",
    "-N",
    "--socket-timeout",
    "--user-agent",
    "--referer",
    "--add-header",
    "--extractor-args",
    "--age-limit",
    "--match-filter",
    "--max-filesize",
    "--min-filesize",
    "--sleep-interval",
    "--max-sleep-interval",
    "--sleep-requests",
    "--max-downloads",
    "--dateafter",
    "--datebefore",
    "--source-address",
    "--sub-format",
    "--convert-subs",
];

/// Only allow-listed flags. Everything else is rejected: notably `--exec`, `--config-locations`,
/// `-o/--output`, `--paths`, `--downloader`, `--cookies`, `--batch-file`, `--plugin-dirs`,
/// `--print-to-file`, `-a`, ... A value flag consumes the next argument verbatim (`--flag=value`
/// is accepted too); stray positional arguments are rejected.
pub fn validate_extra_args(args: &[String]) -> Result<()> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let (name, inline) = match a.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (a.as_str(), false),
        };
        if FLAGS.contains(&name) && !inline {
            i += 1;
        } else if VALUE_FLAGS.contains(&name) {
            if inline {
                i += 1;
            } else if i + 1 < args.len() {
                i += 2;
            } else {
                return Err(Error::ArgRejected(format!("{a} requires a value")));
            }
        } else {
            return Err(Error::ArgRejected(a.clone()));
        }
    }
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('%', "%%")
}

/// Pure command-line construction (everything after the binary path). The spec is assumed
/// valid ([`DownloadSpec::validate`]); `job_tag` names the `.dman-<tag>.final` file in `dir`
/// that receives the final path(s) via `--print-to-file after_move:filepath`.
///
/// Options needing ffmpeg (merge format, audio extraction, embedding, SponsorBlock) are only
/// emitted when `tools` has ffmpeg; without it `--ffmpeg-location /nonexistent` is passed so
/// yt-dlp cannot find one on its own and the plan stays consistent with detection.
pub fn build_args(spec: &DownloadSpec, tools: &Tools, job_tag: &str) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    push(&["--ignore-config", "--newline", "--no-colors"]);
    if spec.playlist {
        push(&["--yes-playlist"]);
        if let Some(items) = &spec.playlist_items {
            push(&["--playlist-items", items]);
        }
    } else {
        push(&["--no-playlist"]);
    }
    push(&["--continue", "--part"]);
    if let Some(r) = spec.rate_limit.filter(|r| *r > 0) {
        push(&["--limit-rate", &r.to_string()]);
    }
    let merge = tools.merge_supported();
    match &tools.ffmpeg {
        Some(f) => push(&["--ffmpeg-location", &f.path.to_string_lossy()]),
        None => push(&["--ffmpeg-location", "/nonexistent"]),
    }
    push(&["-f", &spec.selector]);
    if merge {
        if let Some(codec) = spec.audio_extract {
            push(&["-x", "--audio-format", codec.as_str()]);
        } else if let Some(c) = spec.container {
            push(&["--merge-output-format", c.as_str()]);
        }
    }
    if let Some(s) = &spec.subtitles {
        push(&["--write-subs"]);
        if s.auto {
            push(&["--write-auto-subs"]);
        }
        push(&["--sub-langs", &s.langs.join(",")]);
        if s.embed && merge {
            push(&["--embed-subs"]);
        }
    }
    if merge {
        if spec.embed_thumbnail {
            push(&["--embed-thumbnail"]);
        }
        if spec.embed_metadata {
            push(&["--embed-metadata"]);
        }
        if let Some(sb) = &spec.sponsorblock {
            if !sb.remove.is_empty() {
                push(&["--sponsorblock-remove", &sb.remove.join(",")]);
            }
            if !sb.mark.is_empty() {
                push(&["--sponsorblock-mark", &sb.mark.join(",")]);
            }
        }
    }
    if let Some(c) = &spec.cookies_from_browser {
        push(&["--cookies-from-browser", c]);
    }
    a.extend(spec.extra_args.iter().cloned());
    let dir = spec.dir.to_string_lossy();
    let out = format!(
        "{}/{}",
        esc(dir.trim_end_matches('/')),
        spec.output_template
    );
    let final_file = final_file_path(spec, job_tag);
    a.extend(
        [
            "-o",
            &out,
            "--print-to-file",
            "after_move:filepath",
            &esc(&final_file.to_string_lossy()),
            "--progress-template",
            "download:%(progress)j",
            "--",
            spec.url.as_str(),
        ]
        .map(String::from),
    );
    a
}

/// `<dir>/.dman-<job_tag>.final`
pub(crate) fn final_file_path(spec: &DownloadSpec, job_tag: &str) -> PathBuf {
    spec.dir.join(format!(".dman-{job_tag}.final"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolInfo;

    fn spec() -> DownloadSpec {
        DownloadSpec::new(Url::parse("https://example.com/v.mp4").unwrap(), "/dl")
    }

    fn ffmpeg() -> Tools {
        Tools {
            ytdlp: None,
            ffmpeg: Some(ToolInfo {
                path: "/usr/bin/ffmpeg".into(),
                version: "7".into(),
            }),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn golden_minimal_without_ffmpeg() {
        let a = build_args(&spec(), &Tools::default(), "j1");
        assert_eq!(
            a,
            s(&[
                "--ignore-config",
                "--newline",
                "--no-colors",
                "--no-playlist",
                "--continue",
                "--part",
                "--ffmpeg-location",
                "/nonexistent",
                "-f",
                "b",
                "-o",
                "/dl/%(title)s [%(id)s].%(ext)s",
                "--print-to-file",
                "after_move:filepath",
                "/dl/.dman-j1.final",
                "--progress-template",
                "download:%(progress)j",
                "--",
                "https://example.com/v.mp4",
            ])
        );
        assert!(!a.contains(&"-q".to_string()));
    }

    #[test]
    fn golden_full_with_ffmpeg() {
        let mut sp = spec();
        sp.selector = "bv*[height<=720]+ba/b[height<=720]".into();
        sp.container = Some(Container::Mkv);
        sp.playlist = true;
        sp.playlist_items = Some("1-3,7".into());
        sp.rate_limit = Some(500_000);
        sp.subtitles = Some(SubOpts {
            langs: vec!["en".into(), "fr".into()],
            embed: true,
            auto: true,
        });
        sp.embed_thumbnail = true;
        sp.embed_metadata = true;
        sp.cookies_from_browser = Some("firefox:default".into());
        sp.sponsorblock = Some(SponsorBlock {
            remove: vec!["sponsor".into()],
            mark: vec!["intro".into(), "outro".into()],
        });
        sp.extra_args = s(&["--retries", "5", "--no-mtime"]);
        let a = build_args(&sp, &ffmpeg(), "j2");
        assert_eq!(
            a,
            s(&[
                "--ignore-config",
                "--newline",
                "--no-colors",
                "--yes-playlist",
                "--playlist-items",
                "1-3,7",
                "--continue",
                "--part",
                "--limit-rate",
                "500000",
                "--ffmpeg-location",
                "/usr/bin/ffmpeg",
                "-f",
                "bv*[height<=720]+ba/b[height<=720]",
                "--merge-output-format",
                "mkv",
                "--write-subs",
                "--write-auto-subs",
                "--sub-langs",
                "en,fr",
                "--embed-subs",
                "--embed-thumbnail",
                "--embed-metadata",
                "--sponsorblock-remove",
                "sponsor",
                "--sponsorblock-mark",
                "intro,outro",
                "--cookies-from-browser",
                "firefox:default",
                "--retries",
                "5",
                "--no-mtime",
                "-o",
                "/dl/%(title)s [%(id)s].%(ext)s",
                "--print-to-file",
                "after_move:filepath",
                "/dl/.dman-j2.final",
                "--progress-template",
                "download:%(progress)j",
                "--",
                "https://example.com/v.mp4",
            ])
        );
    }

    #[test]
    fn audio_extract_and_ffmpeg_only_options_dropped() {
        let mut sp = spec();
        sp.audio_extract = Some(AudioCodec::Mp3);
        sp.container = Some(Container::Mp4);
        sp.embed_metadata = true;
        sp.subtitles = Some(SubOpts {
            langs: vec!["en".into()],
            embed: true,
            auto: false,
        });
        let with = build_args(&sp, &ffmpeg(), "j");
        assert!(
            with.windows(3)
                .any(|w| w == ["-x", "--audio-format", "mp3"])
        );
        assert!(!with.contains(&"--merge-output-format".to_string()));
        let without = build_args(&sp, &Tools::default(), "j");
        for flag in [
            "-x",
            "--embed-metadata",
            "--embed-subs",
            "--merge-output-format",
        ] {
            assert!(!without.contains(&flag.to_string()), "{flag}");
        }
        assert!(without.contains(&"--write-subs".to_string()));
    }

    #[test]
    fn percent_in_dir_is_escaped() {
        let mut sp = spec();
        sp.dir = "/dl/100%/x/".into();
        let a = build_args(&sp, &Tools::default(), "j");
        assert!(a.contains(&"/dl/100%%/x/%(title)s [%(id)s].%(ext)s".to_string()));
        assert!(a.contains(&"/dl/100%%/x/.dman-j.final".to_string()));
    }

    #[test]
    fn extra_args_allow_list() {
        assert!(validate_extra_args(&s(&["--retries", "3", "--no-mtime", "-N", "4"])).is_ok());
        assert!(validate_extra_args(&s(&["--proxy=http://p:1"])).is_ok());
        assert!(validate_extra_args(&s(&["--add-header", "--exec"])).is_ok());
        for bad in [
            &["--exec", "rm -rf /"][..],
            &["--exec=touch x"],
            &["--config-locations", "/x"],
            &["-o", "/etc/x"],
            &["--output", "x"],
            &["--paths", "/x"],
            &["--downloader", "curl"],
            &["--retries"],
            &["--no-mtime=1"],
            &["stray"],
            &["-a", "file"],
        ] {
            assert!(validate_extra_args(&s(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_validation() {
        assert!(spec().validate().is_ok());
        for t in ["/abs/x.%(ext)s", "../x.%(ext)s", "a/../b", "C:\\x", ""] {
            let mut sp = spec();
            sp.output_template = t.into();
            assert!(sp.validate().is_err(), "{t}");
        }
        let mut sp = spec();
        sp.output_template = "sub/dir/%(title)s.%(ext)s".into();
        assert!(sp.validate().is_ok());
        let mut sp = spec();
        sp.url = Url::parse("file:///etc/passwd").unwrap();
        assert!(sp.validate().is_err());
        let mut sp = spec();
        sp.playlist_items = Some("1;rm".into());
        assert!(sp.validate().is_err());
        let mut sp = spec();
        sp.sponsorblock = Some(SponsorBlock {
            remove: vec!["bogus".into()],
            mark: vec![],
        });
        assert!(sp.validate().is_err());
        let mut sp = spec();
        sp.extra_args = s(&["--exec", "x"]);
        assert!(sp.validate().is_err());
        let mut sp = spec();
        sp.cookies_from_browser = Some("--exec".into());
        assert!(sp.validate().is_err());
    }
}
