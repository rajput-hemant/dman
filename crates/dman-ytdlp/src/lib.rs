//! yt-dlp (+ffmpeg) video engine adapter.
//!
//! * [`Tools::detect`] finds the binaries; [`is_probably_video_url`] is the offline URL heuristic.
//! * [`probe`] runs `yt-dlp -J` and returns a [`ProbeResult`].
//! * [`QualityPreset`] / [`fallback_plan`] turn a user's quality choice into a format selector,
//!   honouring whether ffmpeg is available.
//! * [`DownloadSpec`] + [`build_args`] + [`spawn`] run a download; [`Running`] streams
//!   [`Progress`] and ends in an [`Outcome`]. Pause = [`Running::stop`]; resume = spawn again
//!   with the same spec (`--continue` picks up the `.part` file).
//!
//! Command lines are never logged (they may contain cookies settings or proxy credentials).

mod args;
mod detect;
mod error;
mod format;
mod probe;
mod progress;
mod run;
mod tools;

pub use args::{DownloadSpec, SponsorBlock, SubOpts, build_args, validate_extra_args};
pub use detect::is_probably_video_url;
pub use error::{Error, Result};
pub use format::{AudioCodec, Container, Plan, PlanKind, QualityPreset, fallback_plan, selector};
pub use probe::{
    Entry, Format, ProbeOptions, ProbeResult, StreamKind, VideoInfo, parse_probe_json, probe,
};
pub use progress::{ItemProgress, Progress, Status, parse_item_line, parse_progress_line};
pub use run::{Outcome, Running, StopHandle, classify_outcome, spawn};
pub use tools::{ToolInfo, Tools};
