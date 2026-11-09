use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("yt-dlp is not installed or not usable")]
    YtdlpMissing,
    #[error("failed to run {tool}: {source}")]
    Spawn {
        tool: &'static str,
        source: std::io::Error,
    },
    #[error("yt-dlp timed out after {0:?}")]
    Timeout(Duration),
    #[error("yt-dlp failed (exit {code:?}): {message}")]
    Failed { code: Option<i32>, message: String },
    #[error("could not parse yt-dlp output: {0}")]
    Parse(String),
    #[error("invalid download spec: {0}")]
    InvalidSpec(String),
    #[error("yt-dlp argument not allowed: {0}")]
    ArgRejected(String),
}
