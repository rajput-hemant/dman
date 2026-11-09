use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use url::Url;

use crate::error::{Error, Result};
use crate::tools::Tools;

#[derive(Debug, Clone)]
pub struct ProbeOptions {
    pub cookies_from_browser: Option<String>,
    /// Treat a playlist URL as a playlist (flat listing) instead of `--no-playlist`.
    pub playlist: bool,
    pub timeout: Duration,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            cookies_from_browser: None,
            playlist: false,
            timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProbeResult {
    Video(VideoInfo),
    Playlist {
        id: Option<String>,
        title: Option<String>,
        entries: Vec<Entry>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
    pub uploader: Option<String>,
    pub thumbnail: Option<String>,
    pub formats: Vec<Format>,
    /// Language codes of uploaded subtitles.
    pub subtitles: Vec<String>,
    /// Language codes of automatic captions.
    pub automatic_captions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Format {
    pub format_id: String,
    pub ext: String,
    /// yt-dlp reports `"none"` for absent streams; `None` means unknown (e.g. generic extractor).
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub tbr: Option<f64>,
    pub filesize: Option<u64>,
    pub filesize_approx: Option<u64>,
    pub protocol: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Video and audio in one stream (or codecs unknown).
    Combined,
    VideoOnly,
    AudioOnly,
    /// Neither (storyboards and such).
    Other,
}

impl Format {
    pub fn kind(&self) -> StreamKind {
        let present = |c: &Option<String>| c.as_deref().map(|c| c != "none");
        match (present(&self.vcodec), present(&self.acodec)) {
            (Some(false), Some(false)) => StreamKind::Other,
            (Some(false), _) => StreamKind::AudioOnly,
            (_, Some(false)) => StreamKind::VideoOnly,
            _ => StreamKind::Combined,
        }
    }

    /// Exact size if known, else the estimate.
    pub fn size(&self) -> Option<u64> {
        self.filesize.or(self.filesize_approx)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// 1-based position in the playlist.
    pub index: u32,
    pub id: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub duration: Option<f64>,
}

/// Run `yt-dlp -J` for `url`. Dropping the returned future kills the child process.
pub async fn probe(tools: &Tools, url: &Url, opts: &ProbeOptions) -> Result<ProbeResult> {
    let ytdlp = tools.ytdlp.as_ref().ok_or(Error::YtdlpMissing)?;
    let mut cmd = Command::new(&ytdlp.path);
    cmd.args(probe_args(url, opts))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(opts.timeout, cmd.output())
        .await
        .map_err(|_| Error::Timeout(opts.timeout))?
        .map_err(|source| Error::Spawn {
            tool: "yt-dlp",
            source,
        })?;
    if !out.status.success() {
        return Err(Error::Failed {
            code: out.status.code(),
            message: error_message(&String::from_utf8_lossy(&out.stderr)),
        });
    }
    parse_probe_json(&out.stdout)
}

pub(crate) fn probe_args(url: &Url, opts: &ProbeOptions) -> Vec<String> {
    let mut a: Vec<String> = ["--ignore-config", "-J"].map(String::from).into();
    if opts.playlist {
        a.push("--flat-playlist".into());
    } else {
        a.push("--no-playlist".into());
    }
    a.push("--no-warnings".into());
    if let Some(b) = &opts.cookies_from_browser {
        a.push("--cookies-from-browser".into());
        a.push(b.clone());
    }
    a.push("--".into());
    a.push(url.to_string());
    a
}

/// Prefer the last `ERROR:` line, else the last non-empty line.
pub(crate) fn error_message(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or(lines.last())
        .map(|l| l.to_string())
        .unwrap_or_else(|| "yt-dlp failed without output".into())
}

pub fn parse_probe_json(bytes: &[u8]) -> Result<ProbeResult> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| Error::Parse(e.to_string()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| Error::Parse("top level is not an object".into()))?;
    let s = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_string);

    if obj.get("_type").and_then(Value::as_str) == Some("playlist") {
        let entries = obj
            .get("entries")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .enumerate()
                    .filter(|(_, e)| e.is_object())
                    .map(|(i, e)| parse_entry(i as u32 + 1, e))
                    .collect()
            })
            .unwrap_or_default();
        return Ok(ProbeResult::Playlist {
            id: s("id"),
            title: s("title"),
            entries,
        });
    }

    let id = s("id").ok_or_else(|| Error::Parse("missing id".into()))?;
    let formats = obj
        .get("formats")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_format).collect())
        .unwrap_or_default();
    Ok(ProbeResult::Video(VideoInfo {
        title: s("title").unwrap_or_else(|| id.clone()),
        id,
        duration: obj.get("duration").and_then(Value::as_f64),
        uploader: s("uploader").or_else(|| s("channel")),
        thumbnail: s("thumbnail"),
        formats,
        subtitles: keys(obj.get("subtitles")),
        automatic_captions: keys(obj.get("automatic_captions")),
    }))
}

fn keys(v: Option<&Value>) -> Vec<String> {
    let mut k: Vec<String> = v
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    k.sort();
    k
}

fn parse_entry(index: u32, e: &Value) -> Entry {
    let s = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
    Entry {
        index,
        id: s("id"),
        title: s("title"),
        url: s("webpage_url").or_else(|| s("url")),
        duration: e.get("duration").and_then(Value::as_f64),
    }
}

fn parse_format(f: &Value) -> Option<Format> {
    let s = |k: &str| f.get(k).and_then(Value::as_str).map(str::to_string);
    let num = |k: &str| f.get(k).and_then(Value::as_f64);
    let int = |k: &str| num(k).filter(|n| *n >= 0.0).map(|n| n as u64);
    Some(Format {
        format_id: s("format_id")?,
        ext: s("ext").unwrap_or_default(),
        vcodec: s("vcodec"),
        acodec: s("acodec"),
        width: int("width").map(|n| n as u32),
        height: int("height").map(|n| n as u32),
        fps: num("fps"),
        tbr: num("tbr"),
        filesize: int("filesize"),
        filesize_approx: int("filesize_approx"),
        protocol: s("protocol"),
        note: s("format_note"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video() {
        let json = br#"{"id":"abc","title":"T","duration":10.5,"uploader":"U","thumbnail":"http://t/x.jpg",
            "subtitles":{"fr":[],"en":[]},"automatic_captions":{"de":[]},
            "formats":[
             {"format_id":"137","ext":"mp4","vcodec":"avc1","acodec":"none","width":1920,"height":1080,"fps":30,"tbr":4000.5,"filesize_approx":1000.0,"protocol":"https","format_note":"1080p"},
             {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a","tbr":128,"filesize":99},
             {"format_id":"0","ext":"mp4"},
             {"ext":"bad"}]}"#;
        let ProbeResult::Video(v) = parse_probe_json(json).unwrap() else {
            panic!("not video")
        };
        assert_eq!(v.id, "abc");
        assert_eq!(v.subtitles, ["en", "fr"]);
        assert_eq!(v.automatic_captions, ["de"]);
        assert_eq!(v.formats.len(), 3);
        assert_eq!(v.formats[0].kind(), StreamKind::VideoOnly);
        assert_eq!(v.formats[0].height, Some(1080));
        assert_eq!(v.formats[0].size(), Some(1000));
        assert_eq!(v.formats[1].kind(), StreamKind::AudioOnly);
        assert_eq!(v.formats[2].kind(), StreamKind::Combined);
    }

    #[test]
    fn parses_playlist_skipping_null_entries() {
        let json = br#"{"_type":"playlist","id":"PL","title":"P","entries":[
            {"id":"a","title":"A","url":"https://x/a","duration":3},null,{"id":"c","webpage_url":"https://x/c"}]}"#;
        let ProbeResult::Playlist { id, entries, .. } = parse_probe_json(json).unwrap() else {
            panic!("not playlist")
        };
        assert_eq!(id.as_deref(), Some("PL"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 1);
        assert_eq!(entries[1].index, 3);
        assert_eq!(entries[1].url.as_deref(), Some("https://x/c"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_probe_json(b"not json").is_err());
        assert!(parse_probe_json(b"{}").is_err());
    }

    #[test]
    fn args_golden() {
        let url = Url::parse("https://example.com/v").unwrap();
        let a = probe_args(&url, &ProbeOptions::default());
        assert_eq!(
            a,
            [
                "--ignore-config",
                "-J",
                "--no-playlist",
                "--no-warnings",
                "--",
                "https://example.com/v"
            ]
        );
        let a = probe_args(
            &url,
            &ProbeOptions {
                playlist: true,
                cookies_from_browser: Some("firefox".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            a,
            [
                "--ignore-config",
                "-J",
                "--flat-playlist",
                "--no-warnings",
                "--cookies-from-browser",
                "firefox",
                "--",
                "https://example.com/v"
            ]
        );
    }

    #[test]
    fn error_message_prefers_error_line() {
        assert_eq!(
            error_message("WARNING: x\nERROR: [generic] 404\n"),
            "ERROR: [generic] 404"
        );
        assert_eq!(error_message(""), "yt-dlp failed without output");
    }

    #[tokio::test]
    async fn probe_without_ytdlp() {
        let url = Url::parse("https://example.com/v").unwrap();
        let e = probe(&Tools::default(), &url, &ProbeOptions::default()).await;
        assert!(matches!(e, Err(Error::YtdlpMissing)));
    }
}
