//! The public tracker list (Q2): fetched when enabled, cached in the data
//! directory for offline starts, attached to new torrents and magnets.
//!
//! Nothing is requested unless `[search.tracker_list] enabled = true`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use dman_config::TrackerList;
use dman_proto::search::TrackersStatus;
use dman_search::{Searcher, parse_tracker_list};

/// Retry delay after a failed refresh (never longer than the refresh interval).
const RETRY: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Default)]
struct Inner {
    list: Vec<String>,
    last_refresh: Option<i64>,
    last_error: Option<String>,
}

/// Shared tracker-list state, read by the job manager and `trackers.status`.
#[derive(Debug)]
pub struct Trackers {
    config: Mutex<TrackerList>,
    inner: Mutex<Inner>,
}

impl Trackers {
    /// State for `config`; the list stays empty until [`Trackers::refresher`] loads it.
    pub fn new(config: TrackerList) -> Self {
        Self {
            config: Mutex::new(config),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn cfg(&self) -> TrackerList {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replaces the settings (config reload); the caller restarts the refresher.
    pub fn set_config(&self, config: TrackerList) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Trackers to add to a new torrent or magnet: empty unless the list is
    /// enabled and `attach_to_new` is set.
    pub fn attach_list(&self) -> Vec<String> {
        let c = self.cfg();
        if c.enabled && c.attach_to_new {
            self.lock().list.clone()
        } else {
            Vec::new()
        }
    }

    /// `trackers.status`.
    pub fn status(&self) -> TrackersStatus {
        let c = self.cfg();
        let g = self.lock();
        TrackersStatus {
            enabled: c.enabled,
            attach_to_new: c.attach_to_new,
            count: u32::try_from(g.list.len()).unwrap_or(u32::MAX),
            last_refresh: g.last_refresh,
            last_error: g.last_error.clone(),
        }
    }

    fn set_list(&self, list: Vec<String>, at: i64) {
        let mut g = self.lock();
        g.list = list;
        g.last_refresh = Some(at);
        g.last_error = None;
    }

    fn set_error(&self, e: String) {
        self.lock().last_error = Some(e);
    }

    /// Loads the cache, then fetches now and every `refresh_hours`. Returns
    /// immediately (spawning nothing, requesting nothing) when disabled.
    pub fn refresher(
        self: &std::sync::Arc<Self>,
        searcher: Searcher,
        cache: PathBuf,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !self.cfg().enabled {
            return None;
        }
        let this = self.clone();
        Some(tokio::spawn(async move {
            this.load_cache(&cache);
            loop {
                let every = Duration::from_secs(this.cfg().refresh_hours.max(1) * 3600);
                let wait = match this.refresh(&searcher, &cache).await {
                    Ok(()) => every,
                    Err(()) => RETRY.min(every),
                };
                tokio::time::sleep(wait).await;
            }
        }))
    }

    fn load_cache(&self, cache: &PathBuf) {
        let Ok(text) = std::fs::read_to_string(cache) else {
            return;
        };
        let list = parse_tracker_list(&text);
        if list.is_empty() {
            return;
        }
        let at = std::fs::metadata(cache)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(crate::manager::now, |d| {
                i64::try_from(d.as_secs()).unwrap_or(0)
            });
        tracing::info!(count = list.len(), "tracker list loaded from cache");
        self.set_list(list, at);
    }

    async fn refresh(&self, searcher: &Searcher, cache: &PathBuf) -> Result<(), ()> {
        match searcher.fetch_tracker_list(&self.cfg().url).await {
            Ok(list) => {
                tracing::info!(count = list.len(), "tracker list refreshed");
                let mut text = list.join("\n");
                text.push('\n');
                if let Err(e) = write_cache(cache, &text) {
                    tracing::warn!("cannot cache the tracker list: {e}");
                }
                self.set_list(list, crate::manager::now());
                Ok(())
            }
            Err(e) => {
                tracing::warn!("tracker list refresh failed: {e}");
                self.set_error(e.to_string());
                Err(())
            }
        }
    }
}

fn write_cache(path: &PathBuf, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// `existing` (a `bt-tracker` value: comma- or whitespace-separated) followed
/// by those of `extra` it does not contain yet, comma-joined.
pub fn merge_trackers(existing: Option<&str>, extra: &[String]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let parts = existing
        .into_iter()
        .flat_map(|e| e.split([',', '\n', '\r', ' ', '\t']))
        .chain(extra.iter().map(String::as_str));
    for t in parts.map(str::trim).filter(|t| !t.is_empty()) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_user_trackers_first_without_duplicates() {
        let extra = vec![
            "udp://a:1/announce".to_owned(),
            "udp://b:2/announce".to_owned(),
        ];
        assert_eq!(
            merge_trackers(Some("udp://b:2/announce, udp://u:3/x"), &extra),
            "udp://b:2/announce,udp://u:3/x,udp://a:1/announce"
        );
        assert_eq!(
            merge_trackers(None, &extra),
            "udp://a:1/announce,udp://b:2/announce"
        );
        assert_eq!(merge_trackers(Some(""), &[]), "");
    }

    #[test]
    fn attach_only_when_enabled_and_attached() {
        let mut c = TrackerList::default();
        let t = Trackers::new(c.clone());
        t.set_list(vec!["udp://a:1".into()], 5);
        assert!(t.attach_list().is_empty(), "disabled");
        c.enabled = true;
        let t = Trackers::new(c.clone());
        t.set_list(vec!["udp://a:1".into()], 5);
        assert_eq!(t.attach_list(), ["udp://a:1"]);
        assert_eq!(t.status().count, 1);
        c.attach_to_new = false;
        let t = Trackers::new(c);
        t.set_list(vec!["udp://a:1".into()], 5);
        assert!(t.attach_list().is_empty());
        assert_eq!(t.status().last_refresh, Some(5));
    }
}
