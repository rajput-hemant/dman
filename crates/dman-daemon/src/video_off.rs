//! Stand-in for [`crate::video`] in builds without the `video` feature: yt-dlp is never
//! looked for, the engine reports itself unavailable and video operations explain the
//! build. `[video]` stays valid configuration.

use std::sync::Arc;

use dman_config::Paths;
use dman_proto::method::EngineStatus;
use dman_proto::{
    Engine, ErrorCode, RpcError, VideoPreferences, VideoPreferencesUpdate, VideoProbe,
    VideoProbeParams,
};
use dman_ytdlp::Tools;
use tokio::sync::watch;

const MESSAGE: &str = "this build of dman has no video support; rebuild with --features video";

/// No tools, ever.
#[derive(Debug)]
pub struct VideoService {
    tools: watch::Sender<Tools>,
}

impl VideoService {
    pub async fn start(_config: &dman_config::Video, _paths: Option<Paths>) -> Self {
        Self {
            tools: watch::Sender::new(Tools::default()),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<Tools> {
        self.tools.subscribe()
    }

    pub async fn redetect(&self) -> Tools {
        Tools::default()
    }

    pub(crate) fn apply_config(&self, _cfg: &dman_config::Video) {}

    pub fn preferences(&self) -> Result<VideoPreferences, RpcError> {
        Err(unavailable())
    }

    pub async fn probe(&self, _p: VideoProbeParams) -> Result<VideoProbe, RpcError> {
        Err(unavailable())
    }
}

pub(crate) async fn set_preferences(
    _state: &Arc<crate::server::ServerState>,
    _u: VideoPreferencesUpdate,
) -> Result<VideoPreferences, RpcError> {
    Err(unavailable())
}

/// `EngineUnavailable` for video operations.
pub fn unavailable() -> RpcError {
    RpcError::new(ErrorCode::EngineUnavailable, MESSAGE)
}

/// The `engine.status` entry of the video engine: unavailable, with the reason.
pub fn engine_status(_t: &Tools) -> EngineStatus {
    EngineStatus {
        engine: Engine::YtDlp,
        available: false,
        version: None,
        features: Vec::new(),
        reason: Some(MESSAGE.to_owned()),
    }
}
