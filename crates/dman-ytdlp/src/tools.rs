use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

const DETECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Decision Q7: yt-dlp should not be older than 12 months.
const MAX_AGE_DAYS: i64 = 365;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub path: PathBuf,
    pub version: String,
}

/// Detected external tools. `None` means missing or not runnable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tools {
    pub ytdlp: Option<ToolInfo>,
    pub ffmpeg: Option<ToolInfo>,
}

impl Tools {
    /// Run `yt-dlp --version` / `ffmpeg -version` (5 s timeout each). An explicit path is used as
    /// given; otherwise the bare name is looked up on `PATH`.
    pub async fn detect(ytdlp_path: Option<PathBuf>, ffmpeg_path: Option<PathBuf>) -> Tools {
        let (ytdlp, ffmpeg) = tokio::join!(
            detect_one(ytdlp_path, "yt-dlp", "--version"),
            detect_one(ffmpeg_path, "ffmpeg", "-version"),
        );
        let tools = Tools { ytdlp, ffmpeg };
        if let Some(w) = tools.ytdlp_age_warning() {
            tracing::warn!("{w}");
        }
        tools
    }

    /// Separate audio and video streams can be merged (also needed for audio extraction,
    /// embedding and SponsorBlock cuts).
    pub fn merge_supported(&self) -> bool {
        self.ffmpeg.is_some()
    }

    /// Warning text when yt-dlp is more than 12 months old (extractors rot quickly).
    pub fn ytdlp_age_warning(&self) -> Option<String> {
        self.ytdlp_age_warning_at(now_days())
    }

    /// Same as [`Tools::ytdlp_age_warning`] with an explicit "today" (days since 1970-01-01).
    pub fn ytdlp_age_warning_at(&self, today_days: i64) -> Option<String> {
        let info = self.ytdlp.as_ref()?;
        let (y, m, d) = parse_ytdlp_date(&info.version)?;
        let age = today_days - days_from_civil(y, m, d);
        (age > MAX_AGE_DAYS).then(|| {
            format!(
                "yt-dlp {} is about {} months old; update it, site support breaks quickly",
                info.version,
                age / 30
            )
        })
    }
}

async fn detect_one(explicit: Option<PathBuf>, name: &str, flag: &str) -> Option<ToolInfo> {
    let candidate = explicit.clone().unwrap_or_else(|| PathBuf::from(name));
    let mut cmd = Command::new(&candidate);
    cmd.arg(flag)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(DETECT_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next()?.trim();
    let version = if name == "ffmpeg" {
        parse_ffmpeg_version(first)?
    } else {
        first.to_string()
    };
    if version.is_empty() {
        return None;
    }
    let path = explicit.or_else(|| which(name)).unwrap_or(candidate);
    Some(ToolInfo { path, version })
}

fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|p| Path::new(p).is_file())
}

/// "ffmpeg version 6.1.1-3ubuntu5 Copyright ..." -> "6.1.1-3ubuntu5".
pub(crate) fn parse_ffmpeg_version(line: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    (it.next()? == "ffmpeg" && it.next()? == "version")
        .then(|| it.next().map(str::to_string))
        .flatten()
}

/// "2026.10.26", "2026.10.26.232815" or "2026.10.26-nightly" -> (2026, 10, 26).
pub(crate) fn parse_ytdlp_date(version: &str) -> Option<(i64, i64, i64)> {
    let mut it = version.split(['.', '-', '+']);
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    ((2000..3000).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d))
        .then_some((y, m, d))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn now_days() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86400) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools_with(version: &str) -> Tools {
        Tools {
            ytdlp: Some(ToolInfo {
                path: "yt-dlp".into(),
                version: version.into(),
            }),
            ffmpeg: None,
        }
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 11, 9), 20766);
    }

    #[test]
    fn version_date_parsing() {
        assert_eq!(parse_ytdlp_date("2026.10.26"), Some((2026, 10, 26)));
        assert_eq!(parse_ytdlp_date("2026.10.26.232815"), Some((2026, 10, 26)));
        assert_eq!(parse_ytdlp_date("2026.10.26-nightly"), Some((2026, 10, 26)));
        assert_eq!(parse_ytdlp_date("garbage"), None);
        assert_eq!(parse_ytdlp_date("1.2.3"), None);
    }

    #[test]
    fn ffmpeg_version_parsing() {
        assert_eq!(
            parse_ffmpeg_version("ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2023"),
            Some("6.1.1-3ubuntu5".into())
        );
        assert_eq!(parse_ffmpeg_version("something else"), None);
    }

    #[test]
    fn age_warning() {
        let today = days_from_civil(2026, 11, 9);
        assert!(
            tools_with("2026.10.26")
                .ytdlp_age_warning_at(today)
                .is_none()
        );
        assert!(
            tools_with("2025.11.20")
                .ytdlp_age_warning_at(today)
                .is_none()
        );
        let w = tools_with("2025.01.01")
            .ytdlp_age_warning_at(today)
            .unwrap();
        assert!(w.contains("2025.01.01"), "{w}");
        assert!(Tools::default().ytdlp_age_warning_at(today).is_none());
    }

    #[test]
    fn merge_supported_follows_ffmpeg() {
        let mut t = Tools::default();
        assert!(!t.merge_supported());
        t.ffmpeg = Some(ToolInfo {
            path: "ffmpeg".into(),
            version: "7".into(),
        });
        assert!(t.merge_supported());
    }

    #[tokio::test]
    async fn detect_missing_binaries() {
        let t = Tools::detect(
            Some("/nonexistent/yt-dlp".into()),
            Some("/nonexistent/ffmpeg".into()),
        )
        .await;
        assert_eq!(t, Tools::default());
    }
}
