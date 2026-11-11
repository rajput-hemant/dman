//! Wire types shared by the dman daemon and every client (TUI, web, CLI).
//!
//! This crate performs no I/O. Everything here is serialised as JSON and is part of
//! the public API, so changes must stay backwards compatible within an
//! [`API_VERSION`].

pub mod error;
pub mod job;
pub mod method;
pub mod redact;
pub mod rpc;
pub mod search;
pub mod video;

pub use error::{ErrorCode, RpcError};
pub use job::*;
pub use video::{
    QualityChoice, QualityChoiceKind, QualityChoiceRef, VideoDownloadPlan, VideoOptions,
    VideoPlanParams, VideoPreferences, VideoPreferencesUpdate, VideoProbe, VideoProbeParams,
    VideoSubtitles,
};

/// Major version of the daemon API. Clients refuse to talk to a daemon with a
/// different major version.
pub const API_VERSION: u32 = 1;
