//! Source detection: what a line of the add form is, and local file checks.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::format::size;

/// What a source token is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `magnet:?xt=urn:btih:…`
    Magnet,
    /// `http://…`
    Http,
    /// `https://…`
    Https,
    /// `ftp://…`
    Ftp,
    /// `sftp://…`
    Sftp,
    /// Local `.torrent` file.
    TorrentFile,
    /// Local `.metalink` / `.meta4` file.
    MetalinkFile,
    /// A video page for the yt-dlp engine (detected or forced with "try as video").
    Video,
    /// Looks like a video page but yt-dlp is not installed: added as plain http.
    VideoUnavailable,
    /// Cannot be added; the text says why.
    Unsupported(&'static str),
}

impl Kind {
    /// Short label shown next to the line.
    pub fn label(self) -> String {
        match self {
            Kind::Magnet => "magnet".into(),
            Kind::Http => "http".into(),
            Kind::Https => "https".into(),
            Kind::Ftp => "ftp".into(),
            Kind::Sftp => "sftp".into(),
            Kind::TorrentFile => "torrent file".into(),
            Kind::MetalinkFile => "metalink file".into(),
            Kind::Video => "video".into(),
            Kind::VideoUnavailable => {
                "video (yt-dlp not installed — will download the page)".into()
            }
            Kind::Unsupported(why) => format!("unsupported: {why}"),
        }
    }

    /// A URI that can be one of several mirrors of a single download.
    pub const fn is_mirrorable(self) -> bool {
        matches!(
            self,
            Kind::Http | Kind::Https | Kind::Ftp | Kind::Sftp | Kind::VideoUnavailable
        )
    }

    /// Goes to the yt-dlp engine.
    pub const fn is_video(self) -> bool {
        matches!(self, Kind::Video)
    }

    /// A URI of any kind (sent as `AddSource::Uris`).
    pub const fn is_uri(self) -> bool {
        self.is_mirrorable() || matches!(self, Kind::Magnet)
    }

    /// A local file that is read and uploaded.
    pub const fn is_file(self) -> bool {
        matches!(self, Kind::TorrentFile | Kind::MetalinkFile)
    }

    /// Can be added.
    pub const fn is_supported(self) -> bool {
        !matches!(self, Kind::Unsupported(_))
    }
}

/// Classify one token (no whitespace handling).
pub fn classify(token: &str) -> Kind {
    let t = token.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        return if lower.contains("xt=urn:btih:") || lower.contains("xt=urn:btmh:") {
            Kind::Magnet
        } else {
            Kind::Unsupported("magnet without xt=urn:btih")
        };
    }
    if let Some((scheme, rest)) = lower.split_once("://") {
        let kind = match scheme {
            "http" => Kind::Http,
            "https" => Kind::Https,
            "ftp" => Kind::Ftp,
            "sftp" => Kind::Sftp,
            _ => return Kind::Unsupported("scheme not supported"),
        };
        return if rest.is_empty() || rest.starts_with('/') {
            Kind::Unsupported("incomplete URL")
        } else {
            kind
        };
    }
    if lower.ends_with(".torrent") {
        Kind::TorrentFile
    } else if lower.ends_with(".metalink") || lower.ends_with(".meta4") {
        Kind::MetalinkFile
    } else {
        Kind::Unsupported("not a URL, magnet or .torrent/.metalink path")
    }
}

/// What the add view knows about the video engine, and the user's per-URL
/// "try as video" overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoCtx {
    /// An `engine.status` entry for yt-dlp has arrived (until then nothing is
    /// classified as video).
    pub known: bool,
    /// yt-dlp can run.
    pub available: bool,
    /// ffmpeg is usable (merging, conversion, embedding).
    pub ffmpeg: bool,
    /// Why yt-dlp is unavailable, or why ffmpeg is missing.
    pub reason: Option<String>,
    /// URL text → forced (`true` = video, `false` = plain http).
    pub forced: std::collections::HashMap<String, bool>,
}

impl VideoCtx {
    /// Read the yt-dlp entry of `engines`.
    pub fn from_engines(&self, engines: &[dman_proto::method::EngineStatus]) -> Self {
        let mut out = Self {
            forced: self.forced.clone(),
            ..Self::default()
        };
        if let Some(e) = engines
            .iter()
            .find(|e| e.engine == dman_proto::Engine::YtDlp)
        {
            out.known = true;
            out.available = e.available;
            out.ffmpeg = e
                .features
                .iter()
                .any(|f| f == "merge" || f.starts_with("ffmpeg"));
            out.reason = e.reason.clone();
        }
        out
    }

    /// The engine facts only (not the overrides) differ from `other`.
    pub fn same_engine(&self, other: &Self) -> bool {
        (self.known, self.available, self.ffmpeg, &self.reason)
            == (other.known, other.available, other.ffmpeg, &other.reason)
    }
}

/// Whether `text` looks like a video site URL (offline heuristic, D10).
pub fn looks_like_video(text: &str) -> bool {
    url::Url::parse(text)
        .map(|u| dman_ytdlp::is_probably_video_url(&u))
        .unwrap_or(false)
}

/// One source of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The text as it will be used (a URI, or a local path).
    pub text: String,
    /// What it is.
    pub kind: Kind,
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// The sources on one line. A line is split on whitespace when every piece is
/// supported on its own; otherwise it is one source (a path with spaces).
/// Quotes around a path and `\ ` escapes (as pasted by drag-and-drop) are
/// removed. Blank lines and `#` comments give nothing.
pub fn tokens(line: &str) -> Vec<Token> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Vec::new();
    }
    let whole = unquote(line).replace("\\ ", " ");
    let whole_kind = classify(&whole);
    let pieces: Vec<&str> = line.split_whitespace().collect();
    if pieces.len() > 1 {
        let kinds: Vec<Kind> = pieces.iter().map(|p| classify(p)).collect();
        // Only a local file path may contain spaces.
        if kinds.iter().all(|k| k.is_supported()) || !whole_kind.is_file() {
            return pieces
                .into_iter()
                .zip(kinds)
                .map(|(p, kind)| Token {
                    text: p.to_owned(),
                    kind,
                })
                .collect();
        }
    }
    vec![Token {
        text: whole,
        kind: whole_kind,
    }]
}

/// Like [`tokens`], with http(s) URLs classified for the video engine.
pub fn tokens_with(line: &str, ctx: &VideoCtx) -> Vec<Token> {
    let mut t = tokens(line);
    if ctx.known {
        for tok in &mut t {
            if matches!(tok.kind, Kind::Http | Kind::Https) {
                tok.kind = video_kind(&tok.text, tok.kind, ctx);
            }
        }
    }
    t
}

fn video_kind(text: &str, plain: Kind, ctx: &VideoCtx) -> Kind {
    let detected = looks_like_video(text);
    if !ctx.available {
        return if detected {
            Kind::VideoUnavailable
        } else {
            plain
        };
    }
    if ctx.forced.get(text).copied().unwrap_or(detected) {
        Kind::Video
    } else {
        plain
    }
}

/// Result of checking a local file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStatus {
    /// Check requested, no answer yet.
    Checking,
    /// A readable regular file.
    Ready {
        /// Size in bytes.
        size: u64,
    },
    /// Does not exist.
    Missing,
    /// Exists but is not a regular file.
    NotAFile,
    /// Exists but cannot be read.
    Unreadable(String),
}

/// Largest local file that is uploaded.
pub const MAX_FILE_BYTES: u64 = 32 << 20;

/// `~` expanded, as a path.
pub fn local_path(token: &str) -> PathBuf {
    dman_config::expand_tilde(token)
}

/// Check a local path (blocking; run from the runtime, not from `update`).
pub fn check_path(token: &str) -> PathStatus {
    let p = local_path(token);
    match std::fs::metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PathStatus::Missing,
        Err(e) => PathStatus::Unreadable(e.to_string()),
        Ok(m) if !m.is_file() => PathStatus::NotAFile,
        Ok(m) => match std::fs::File::open(&p) {
            Ok(_) => PathStatus::Ready { size: m.len() },
            Err(e) => PathStatus::Unreadable(e.to_string()),
        },
    }
}

/// Read a local source file (blocking).
pub fn read_file(token: &str) -> Result<Vec<u8>, String> {
    let p = local_path(token);
    let len = std::fs::metadata(&p)
        .map_err(|e| format!("{}: {e}", p.display()))?
        .len();
    if len > MAX_FILE_BYTES {
        return Err(format!("{} is too large ({})", p.display(), size(len)));
    }
    std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Text and severity of the live note beside a token.
pub fn describe(t: &Token, paths: &HashMap<String, PathStatus>) -> (String, Severity) {
    if let Kind::Unsupported(_) = t.kind {
        return (t.kind.label(), Severity::Error);
    }
    if !t.kind.is_file() {
        return (t.kind.label(), Severity::Ok);
    }
    let label = t.kind.label();
    match paths.get(&t.text) {
        None | Some(PathStatus::Checking) => (format!("{label} · checking…"), Severity::Warn),
        Some(PathStatus::Ready { size: s }) => (format!("{label} · {}", size(*s)), Severity::Ok),
        Some(PathStatus::Missing) => (format!("{label} · not found"), Severity::Error),
        Some(PathStatus::NotAFile) => (format!("{label} · not a file"), Severity::Error),
        Some(PathStatus::Unreadable(e)) => (format!("{label} · unreadable: {e}"), Severity::Error),
    }
}

/// How a note is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Fine.
    Ok,
    /// Pending or doubtful.
    Warn,
    /// Cannot be added.
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_kind() {
        assert_eq!(classify("magnet:?xt=urn:btih:abc&dn=x"), Kind::Magnet);
        assert!(matches!(classify("magnet:?dn=x"), Kind::Unsupported(_)));
        assert_eq!(classify("http://a.org/f"), Kind::Http);
        assert_eq!(classify("HTTPS://a.org/f"), Kind::Https);
        assert_eq!(classify("ftp://h/f"), Kind::Ftp);
        assert_eq!(classify("sftp://u@h/f"), Kind::Sftp);
        assert!(matches!(classify("https://"), Kind::Unsupported(_)));
        assert!(matches!(
            classify("file:///x.torrent"),
            Kind::Unsupported(_)
        ));
        assert_eq!(classify("/tmp/a.torrent"), Kind::TorrentFile);
        assert_eq!(classify("./A.TORRENT"), Kind::TorrentFile);
        assert_eq!(classify("x.metalink"), Kind::MetalinkFile);
        assert_eq!(classify("x.meta4"), Kind::MetalinkFile);
        assert!(matches!(classify("hello"), Kind::Unsupported(_)));
        assert!(matches!(classify("/tmp/a.zip"), Kind::Unsupported(_)));
    }

    #[test]
    fn splits_on_whitespace_only_when_all_pieces_are_sources() {
        let t = tokens("http://a/x   https://b/y");
        assert_eq!(t.len(), 2);
        // A path with spaces stays whole.
        let t = tokens("/home/me/My Files/a.torrent");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, Kind::TorrentFile);
        // Unsupported piece next to a URL: still split so each gets its verdict.
        let t = tokens("http://a/x junk");
        assert_eq!(t.len(), 2);
        assert!(!t[1].kind.is_supported());
        assert!(tokens("   ").is_empty());
        assert!(tokens("# comment http://a/x").is_empty());
    }

    #[test]
    fn unquotes_and_unescapes_dropped_paths() {
        assert_eq!(tokens("'/a b/c.torrent'")[0].text, "/a b/c.torrent");
        assert_eq!(tokens("\"/a/c.torrent\"")[0].text, "/a/c.torrent");
        assert_eq!(tokens(r"/a\ b/c.torrent")[0].text, "/a b/c.torrent");
    }

    #[test]
    fn checks_files() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.torrent");
        std::fs::write(&f, b"1234").unwrap();
        let s = |p: &std::path::Path| check_path(&p.display().to_string());
        assert_eq!(s(&f), PathStatus::Ready { size: 4 });
        assert_eq!(s(&d.path().join("nope.torrent")), PathStatus::Missing);
        assert_eq!(s(d.path()), PathStatus::NotAFile);
        assert_eq!(read_file(&f.display().to_string()).unwrap(), b"1234");
        assert!(read_file(&d.path().join("x").display().to_string()).is_err());
    }

    #[test]
    fn describes_with_status() {
        let t = Token {
            text: "/a.torrent".into(),
            kind: Kind::TorrentFile,
        };
        let mut m = HashMap::new();
        assert_eq!(describe(&t, &m).1, Severity::Warn);
        m.insert("/a.torrent".into(), PathStatus::Ready { size: 2048 });
        assert_eq!(describe(&t, &m).0, "torrent file · 2.0 KiB");
        m.insert("/a.torrent".into(), PathStatus::Missing);
        assert_eq!(
            describe(&t, &m),
            ("torrent file · not found".into(), Severity::Error)
        );
    }
}
