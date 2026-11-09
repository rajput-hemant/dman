//! Stand-in for [`crate::web`] in builds without the `web` feature: the same surface, no
//! listener. `[web]` stays valid configuration; asking for the UI explains the build.

use std::sync::Arc;

use dman_config::{Paths, Web};
use dman_proto::RpcError;
use dman_proto::method::{WebStartParams, WebStatus};

use crate::server::ServerState;

const MESSAGE: &str = "this build of dman has no web UI; rebuild with --features web";

/// Nothing to check: the web UI is never started in this build.
pub fn preflight(_web: &Web, _paths: &Paths) -> Result<(), RpcError> {
    Ok(())
}

/// Holds the configured `[web]` values so `config.reload` can still compare them.
pub struct WebHost {
    settings: Web,
}

impl WebHost {
    pub fn new(_paths: Paths, settings: Web) -> Self {
        Self { settings }
    }

    pub(crate) fn settings(&self) -> Web {
        self.settings.clone()
    }

    /// `web.status`: never running.
    pub async fn status(&self) -> WebStatus {
        WebStatus {
            enabled: false,
            url: None,
            exposed: !self.settings.is_loopback(),
            auth: false,
        }
    }

    /// `[web] enabled = true` in the config: ignored with a warning.
    pub async fn start_configured(&self, _state: &Arc<ServerState>) -> Result<WebStatus, RpcError> {
        tracing::warn!("[web] enabled is ignored: {MESSAGE}");
        Ok(self.status().await)
    }

    /// `web.start`: refused.
    pub async fn start_with(
        &self,
        _state: &Arc<ServerState>,
        _p: WebStartParams,
    ) -> Result<WebStatus, RpcError> {
        Err(RpcError::refused(MESSAGE))
    }

    /// `web.stop`: there is never anything to stop.
    pub async fn stop_with(&self, _persist: bool) -> Result<WebStatus, RpcError> {
        Ok(self.status().await)
    }

    pub async fn stop(&self) {}
}
