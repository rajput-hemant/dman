//! `config.reload`: re-read `config.toml` and apply what can change live.
//!
//! The file is loaded and validated first; on any error nothing changes and
//! the caller gets `InvalidInput` (the old configuration keeps running).
//! Applied live: `[limits]`, `[[schedule]]`, `[torrents]`, `[notify]` (hooks,
//! webhook, desktop on daemon), `[search]` (providers, proxy, tracker list)
//! and `[video]` defaults (and the yt-dlp/ffmpeg paths, which re-detect the
//! tools). Reported in `restart_required` instead: the served profile
//! (listen address, directory, secret), `[aria2]` and `[web]`.
//! A reload drops runtime limit overrides and re-evaluates the schedule.

use std::sync::{Arc, Mutex};

use dman_config::{Config, DaemonTarget, Paths, Secrets};
use dman_proto::method::{Level, ReloadResult};
use dman_proto::{Limits, RpcError};
use tokio::sync::watch;

use crate::manager::Settings;
use crate::notify::Notifier;
use crate::server::ServerState;

/// What the daemon currently runs with, and the handles to change it.
pub struct Reloader {
    paths: Paths,
    profile: String,
    /// The configuration as applied so far (restart-only parts as at startup).
    running: Mutex<Config>,
    notifier: watch::Sender<Notifier>,
}

impl Reloader {
    pub fn new(
        paths: Paths,
        profile: String,
        running: Config,
        notifier: watch::Sender<Notifier>,
    ) -> Self {
        Self {
            paths,
            profile,
            running: Mutex::new(running),
            notifier,
        }
    }
}

fn invalid(e: impl std::fmt::Display) -> RpcError {
    RpcError::invalid(format!("config.toml: {e}"))
}

/// The settings groups that differ between `old` and `new`: `(applied, restart_required)`.
/// `web` is the `[web]` section the web host currently uses.
pub(crate) fn diff(
    old: &Config,
    web: &dman_config::Web,
    new: &Config,
    profile: &str,
) -> (Vec<&'static str>, Vec<String>) {
    let mut applied = Vec::new();
    if old.limits != new.limits {
        applied.push("limits");
    }
    if old.schedule != new.schedule {
        applied.push("schedule");
    }
    if old.torrents != new.torrents {
        applied.push("torrents");
    }
    if old.notify != new.notify {
        applied.push("notify");
    }
    if old.search != new.search {
        applied.push("search");
    }
    if old.video != new.video {
        applied.push("video");
    }
    let mut restart = Vec::new();
    if old.aria2 != new.aria2 {
        restart.push("aria2 (path, rpc_port, extra_args, tick_ms)".to_owned());
    }
    if web != &new.web {
        restart.push("web (use `dman serve` to change the running web UI)".to_owned());
    }
    if old.profiles.get(profile) != new.profiles.get(profile) {
        restart.push(format!(
            "profiles.{profile} (listen address, download_dir, thresholds)"
        ));
    }
    (applied, restart)
}

impl Reloader {
    /// `video.preferences.set` changed `[video]`: remember it as applied.
    pub(crate) fn note_video(&self, video: &dman_config::Video) {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).video = video.clone();
    }

    pub(crate) async fn reload(&self, state: &Arc<ServerState>) -> Result<ReloadResult, RpcError> {
        let new = Config::load(&self.paths).map_err(invalid)?;
        new.validate().map_err(invalid)?;
        let profile = new
            .profile(Some(&self.profile))
            .map_err(|e| invalid(format!("profile `{}`: {e}", self.profile)))?
            .1
            .clone();
        if !matches!(profile.daemon, DaemonTarget::Local { .. }) {
            return Err(invalid(format!(
                "profile `{}` no longer points to a local daemon",
                self.profile
            )));
        }
        let secrets = Secrets::load(&self.paths).map_err(invalid)?;
        // Everything fallible is built before anything is applied.
        let webhook_secret = match new
            .notify
            .webhook
            .as_ref()
            .and_then(|w| w.secret_ref.as_ref())
        {
            Some(r) => Some(secrets.get(r).ok_or_else(|| {
                invalid(format!(
                    "notify.webhook.secret_ref `{r}` is not defined in secrets.toml"
                ))
            })?),
            None => None,
        };
        let notifier = Notifier::new(new.notify.clone(), webhook_secret, state.events.clone())
            .map_err(|e| invalid(format!("notify: {e}")))?;
        let search = state
            .search
            .prepare_reload(&new, &secrets)
            .map_err(invalid)?;

        let old = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (applied, restart_required) = diff(&old, &state.web.settings(), &new, &self.profile);

        let video_paths_changed = old.video.ytdlp_path != new.video.ytdlp_path
            || old.video.ffmpeg_path != new.video.ffmpeg_path;
        if applied.contains(&"limits")
            || applied.contains(&"schedule")
            || applied.contains(&"torrents")
            || applied.contains(&"video")
        {
            let settings = Settings {
                limits: Limits {
                    download: new.limits.download.0,
                    upload: new.limits.upload.0,
                    max_concurrent: new.limits.max_concurrent,
                },
                schedule: new.schedule.clone(),
                torrents: new.torrents.clone(),
                video: new.video.clone(),
            };
            state
                .manager
                .call(crate::manager::Request::ApplySettings(Box::new(settings)))
                .await?;
        }
        if applied.contains(&"notify") {
            let _ = self.notifier.send(notifier);
        }
        if applied.contains(&"search") {
            state.search.apply_reload(search);
        }
        if applied.contains(&"video") {
            state.video.apply_config(&new.video);
        }
        if video_paths_changed {
            state.video.redetect().await;
        }
        // Remember what is applied; the restart-only parts stay as running.
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            let mut next = new.clone();
            next.aria2 = running.aria2.clone();
            next.web = running.web.clone();
            if let Some(p) = running.profiles.get(&self.profile) {
                next.profiles.insert(self.profile.clone(), p.clone());
            }
            *running = next;
        }
        let text = if applied.is_empty() && restart_required.is_empty() {
            "configuration reloaded: no changes".to_owned()
        } else {
            let mut t = format!("configuration reloaded: applied [{}]", applied.join(", "));
            if !restart_required.is_empty() {
                t.push_str(&format!(
                    "; restart required for {}",
                    restart_required.join(", ")
                ));
            }
            t
        };
        let _ = state.events.send(dman_proto::method::Event::Message {
            level: if restart_required.is_empty() {
                Level::Info
            } else {
                Level::Warn
            },
            text,
        });
        Ok(ReloadResult {
            applied: applied.into_iter().map(str::to_owned).collect(),
            restart_required,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Config {
        Config::from_toml_str(text, std::path::Path::new("t.toml")).unwrap()
    }

    #[test]
    fn diff_splits_live_and_restart_settings() {
        let old = Config::default();
        let web = old.web.clone();
        let new = cfg(r#"
[limits]
download = "5MiB"
[torrents]
remove_unselected = "ask"
[aria2]
rpc_port = 7000
[web]
port = 7777
[profiles.local]
download_dir = "/elsewhere"
"#);
        let (applied, restart) = diff(&old, &web, &new, "local");
        assert_eq!(applied, ["limits", "torrents"]);
        assert_eq!(restart.len(), 3, "{restart:?}");
        assert!(restart[0].starts_with("aria2") && restart[1].starts_with("web"));
        assert!(restart[2].starts_with("profiles.local"));
        let (a, r) = diff(&old, &web, &old, "local");
        assert!(a.is_empty() && r.is_empty());
    }
}
