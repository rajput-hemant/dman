//! Stand-in for [`crate::search`] in builds without the `search` feature: the same surface,
//! no providers and no tracker list. `[search]` stays valid configuration; every search,
//! provider and tracker method explains the build.

use std::sync::Arc;

use dman_config::{Config, Paths, Secrets};
use dman_proto::RpcError;
use dman_proto::method::Event;
use dman_store::Store;
use serde_json::Value;
use tokio::sync::broadcast;

const MESSAGE: &str = "this build of dman has no torrent search; rebuild with --features search";

/// Whether `method` is a search, provider or tracker-list method.
pub fn handles(method: &str) -> bool {
    ["search.", "providers.", "trackers."]
        .iter()
        .any(|p| method.starts_with(p))
}

/// The tracker list: always empty here.
pub struct Trackers;

impl Trackers {
    pub fn attach_list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Keeps a list of comma-separated trackers unchanged (there is nothing to add).
pub fn merge_trackers(existing: Option<&str>, _extra: &[String]) -> String {
    existing.unwrap_or_default().to_owned()
}

pub struct SearchService;

/// Nothing to validate or apply.
pub struct PreparedSearch;

impl SearchService {
    pub fn new(
        _paths: Paths,
        _store: Store,
        _events: broadcast::Sender<Event>,
        config: &Config,
        _secrets: &Secrets,
    ) -> anyhow::Result<Arc<Self>> {
        if config.search.tracker_list.enabled {
            tracing::warn!("[search.tracker_list] is ignored: {MESSAGE}");
        }
        Ok(Arc::new(Self))
    }

    pub fn trackers(&self) -> Arc<Trackers> {
        Arc::new(Trackers)
    }

    pub fn spawn_tracker_refresher(&self) {}

    pub fn prepare_reload(&self, _: &Config, _: &Secrets) -> anyhow::Result<PreparedSearch> {
        Ok(PreparedSearch)
    }

    pub fn apply_reload(&self, _: PreparedSearch) {}

    pub fn close(&self) {}

    pub async fn call(self: &Arc<Self>, _method: &str, _params: Value) -> Result<Value, RpcError> {
        Err(RpcError::refused(MESSAGE))
    }
}
