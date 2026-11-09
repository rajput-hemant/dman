//! Configuration schema, loading and validation.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::schedule::{ScheduleWindow, validate_windows};
use crate::units::{ByteLimit, DiskThreshold};
use crate::{ConfigError, Paths, fsutil};

/// Environment variable selecting the active profile.
pub const ENV_PROFILE: &str = "DMAN_PROFILE";

/// How a profile reaches its daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum DaemonTarget {
    /// A daemon on this machine, listening on `listen`.
    Local {
        /// Listen address, e.g. `127.0.0.1:6810`.
        #[serde(default = "default_listen")]
        listen: String,
        /// Name of the secret holding the API token.
        #[serde(default = "default_local_secret")]
        secret_ref: String,
    },
    /// A daemon elsewhere, reached over `ws://` or `wss://`.
    Remote {
        /// WebSocket URL.
        url: String,
        /// Name of the secret holding the API token.
        secret_ref: String,
    },
}

fn default_listen() -> String {
    "127.0.0.1:6810".into()
}
fn default_local_secret() -> String {
    "daemon".into()
}

impl Default for DaemonTarget {
    fn default() -> Self {
        Self::Local {
            listen: default_listen(),
            secret_ref: default_local_secret(),
        }
    }
}

impl DaemonTarget {
    /// Name of the secret holding the API token.
    pub fn secret_ref(&self) -> &str {
        match self {
            Self::Local { secret_ref, .. } | Self::Remote { secret_ref, .. } => secret_ref,
        }
    }
}

/// One named daemon connection plus its download policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    /// Daemon location.
    pub daemon: DaemonTarget,
    /// Default download directory; a leading `~` is expanded by [`Profile::download_path`].
    pub download_dir: String,
    /// Below this much free space downloads are paused.
    pub min_free: DiskThreshold,
    /// Below this much free space a warning is shown.
    pub warn_free: DiskThreshold,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            daemon: DaemonTarget::default(),
            download_dir: "~/Downloads".into(),
            min_free: DiskThreshold::Absolute(1 << 30),
            warn_free: DiskThreshold::Absolute(5 << 30),
        }
    }
}

impl Profile {
    /// `download_dir` with a leading `~` expanded to the home directory.
    pub fn download_path(&self) -> PathBuf {
        expand_tilde(&self.download_dir)
    }
}

/// Expand a leading `~` or `~/` using the user's home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    let rest = if p == "~" {
        Some("")
    } else {
        p.strip_prefix("~/")
    };
    match (rest, etcetera::home_dir()) {
        (Some(rest), Ok(home)) => {
            if rest.is_empty() {
                home
            } else {
                home.join(rest)
            }
        }
        _ => PathBuf::from(p),
    }
}

/// Global transfer limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Download speed cap in bytes/s (0 = unlimited).
    pub download: ByteLimit,
    /// Upload speed cap in bytes/s (0 = unlimited).
    pub upload: ByteLimit,
    /// Maximum simultaneously active downloads.
    pub max_concurrent: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            download: ByteLimit(0),
            upload: ByteLimit(0),
            max_concurrent: 3,
        }
    }
}

/// aria2 engine settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Aria2 {
    /// Executable name or path.
    pub path: String,
    /// Local RPC port.
    pub rpc_port: u16,
    /// Extra command line arguments.
    pub extra_args: Vec<String>,
    /// Polling interval in milliseconds.
    pub tick_ms: u64,
}

impl Default for Aria2 {
    fn default() -> Self {
        Self {
            path: "aria2c".into(),
            rpc_port: 6800,
            extra_args: Vec::new(),
            tick_ms: 1000,
        }
    }
}

/// Terminal UI settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// Theme name.
    pub theme: String,
    /// Enable mouse support.
    pub mouse: bool,
    /// Redraw/refresh interval in milliseconds.
    pub refresh_ms: u64,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "default".into(),
            mouse: false,
            refresh_ms: 250,
        }
    }
}

/// The whole `config.toml`.
///
/// `deny_unknown_fields` cannot be combined with `#[serde(flatten)]`, so the
/// top level is lenient: unknown tables (future `video`, `providers`, `web`,
/// `notify` sections) are captured in [`Config::extra`] and written
/// back unchanged. All nested sections remain strict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Profile used when none is requested.
    pub default_profile: String,
    /// Named profiles.
    pub profiles: BTreeMap<String, Profile>,
    /// Transfer limits.
    pub limits: Limits,
    /// aria2 settings.
    pub aria2: Aria2,
    /// UI settings.
    pub ui: Ui,
    /// Schedule windows (`[[schedule]]`); first match wins, see [`crate::schedule`].
    pub schedule: Vec<ScheduleWindow>,
    /// Notifications, hooks and webhook.
    pub notify: crate::Notify,
    /// Sections not yet understood by this version.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_profile: "local".into(),
            profiles: BTreeMap::from([("local".to_owned(), Profile::default())]),
            limits: Limits::default(),
            aria2: Aria2::default(),
            ui: Ui::default(),
            schedule: Vec::new(),
            notify: crate::Notify::default(),
            extra: toml::Table::new(),
        }
    }
}

/// Commented default config written by [`Config::write_default`].
pub const DEFAULT_CONFIG_TOML: &str = r#"# dman configuration. Edit freely; comments are preserved by `dman config set`.

# Profile used when --profile / DMAN_PROFILE are not given.
default_profile = "local"

[profiles.local]
# download_dir supports a leading "~".
download_dir = "~/Downloads"
# Pause downloads below this free space; warn below warn_free.
# Absolute ("5GiB", "500MB") or percent ("5%").
min_free = "1GiB"
warn_free = "5GiB"

[profiles.local.daemon]
kind = "local"
listen = "127.0.0.1:6810"
# Name of the entry in secrets.toml holding the API token.
secret_ref = "daemon"

# A remote daemon:
# [profiles.work]
# daemon = { kind = "remote", url = "wss://host:6810", secret_ref = "work" }

[limits]
# Bytes per second ("10MiB" or an integer); 0 = unlimited.
download = "0"
upload = "0"
max_concurrent = 3

[aria2]
path = "aria2c"
rpc_port = 6800
extra_args = []
tick_ms = 1000

[ui]
theme = "default"
mouse = false
refresh_ms = 250

# Schedule windows (all times are local wall-clock time). Outside every window
# [limits] applies; with no windows everything runs always. Windows may overlap:
# the first match wins. An end before the start crosses midnight, and `days`
# names the day the window starts on. Actions: "run" (optional limits override
# [limits]) or "pause" (dman starts nothing and pauses what it started).
# days: mon..sun, weekdays, weekends, daily (default).
#
# [[schedule]]
# name = "night"
# days = ["daily"]
# start = "01:00"
# end = "07:00"
# action = "run"
# download_limit = "0"        # unlimited at night
# max_concurrent = 6
#
# [[schedule]]
# name = "work"
# days = ["weekdays"]
# start = "09:00"
# end = "17:00"
# action = "pause"

[notify]
# Desktop notifications shown by the TUI (on the machine where it runs).
desktop = true
# Also notify on the daemon host (needs a notification service there).
desktop_on_daemon = false
# Events that notify: completed, failed, seed_completed.
on = ["completed", "failed", "seed_completed"]

# POST a JSON body {event, id, name, path, status, size, engine, error, at}.
# With secret_ref the body is signed: X-Dman-Signature: sha256=<HMAC-SHA256 hex>.
# [notify.webhook]
# url = "https://example.org/hook"
# timeout_secs = 10
# retries = 3
# secret_ref = "webhook"

# Hooks run without a shell: command is the argv. Environment: DMAN_EVENT,
# DMAN_ID, DMAN_NAME, DMAN_PATH, DMAN_STATUS, DMAN_ENGINE, DMAN_SIZE, DMAN_ERROR.
# [[notify.hook]]
# on = ["completed"]
# command = ["/home/me/bin/on-done", "--quiet"]
# timeout_secs = 60
"#;

impl Config {
    /// Load `config.toml`, returning defaults when the file does not exist.
    /// The result is not validated; call [`Config::validate`].
    pub fn load(paths: &Paths) -> Result<Self, ConfigError> {
        let path = paths.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_toml_str(&text, &path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::io(path, e)),
        }
    }

    /// Parse configuration text; `path` is only used in error messages.
    pub fn from_toml_str(text: &str, path: &std::path::Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_owned(),
            message: e.to_string().trim_end().to_owned(),
            span: e.span(),
        })
    }

    /// Write the commented default config unless a file already exists.
    /// Returns `true` if a file was written.
    pub fn write_default(paths: &Paths) -> Result<bool, ConfigError> {
        let path = paths.config_file();
        if path.exists() {
            return Ok(false);
        }
        fsutil::atomic_write(&path, DEFAULT_CONFIG_TOML.as_bytes(), None)?;
        Ok(true)
    }

    /// Check semantic validity (references, addresses, URLs, thresholds).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        if !self.profiles.contains_key(&self.default_profile) {
            return bad(format!(
                "default_profile `{}` is not defined under [profiles]",
                self.default_profile
            ));
        }
        for (name, p) in &self.profiles {
            let valid_name = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && !name.starts_with('.');
            if !valid_name {
                return bad(format!(
                    "profile name `{name}` must be non-empty and contain only letters, digits, `-`, `_`, `.`"
                ));
            }
            if p.daemon.secret_ref().trim().is_empty() {
                return bad(format!("profile `{name}`: secret_ref must not be empty"));
            }
            match &p.daemon {
                DaemonTarget::Local { listen, .. } => {
                    if listen.parse::<SocketAddr>().is_err() {
                        return bad(format!(
                            "profile `{name}`: listen `{listen}` is not a valid host:port address"
                        ));
                    }
                }
                DaemonTarget::Remote { url, .. } => match url::Url::parse(url) {
                    Ok(u) if matches!(u.scheme(), "ws" | "wss") && u.host_str().is_some() => {}
                    Ok(_) => {
                        return bad(format!(
                            "profile `{name}`: url `{url}` must be ws:// or wss:// with a host"
                        ));
                    }
                    Err(e) => return bad(format!("profile `{name}`: invalid url `{url}`: {e}")),
                },
            }
            if p.download_dir.trim().is_empty() {
                return bad(format!("profile `{name}`: download_dir must not be empty"));
            }
            let sane = match (p.min_free, p.warn_free) {
                (DiskThreshold::Absolute(a), DiskThreshold::Absolute(b)) => a <= b,
                (DiskThreshold::Percent(a), DiskThreshold::Percent(b)) => a <= b,
                _ => true,
            };
            if !sane {
                return bad(format!(
                    "profile `{name}`: warn_free ({}) must not be below min_free ({})",
                    p.warn_free, p.min_free
                ));
            }
        }
        if self.limits.max_concurrent == 0 {
            return bad("limits.max_concurrent must be at least 1".into());
        }
        if let Err(m) = validate_windows(&self.schedule) {
            return bad(m);
        }
        if let Err(m) = self.notify.validate() {
            return bad(m);
        }
        if self.aria2.rpc_port == 0 {
            return bad("aria2.rpc_port must be non-zero".into());
        }
        if self.aria2.tick_ms == 0 || self.ui.refresh_ms == 0 {
            return bad("aria2.tick_ms and ui.refresh_ms must be non-zero".into());
        }
        Ok(())
    }

    /// Resolve a profile: explicit `name`, else `DMAN_PROFILE`, else `default_profile`.
    /// Returns the profile's name and definition.
    pub fn profile(&self, name: Option<&str>) -> Result<(&str, &Profile), ConfigError> {
        let env = std::env::var(ENV_PROFILE).ok();
        self.profile_with_env(name, env.as_deref())
    }

    /// Like [`Config::profile`] with the `DMAN_PROFILE` value passed explicitly.
    pub fn profile_with_env(
        &self,
        name: Option<&str>,
        env: Option<&str>,
    ) -> Result<(&str, &Profile), ConfigError> {
        let wanted = name
            .filter(|n| !n.is_empty())
            .or(env.filter(|n| !n.is_empty()))
            .unwrap_or(&self.default_profile);
        self.profiles
            .get_key_value(wanted)
            .map(|(k, v)| (k.as_str(), v))
            .ok_or_else(|| ConfigError::UnknownProfile(wanted.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Config, ConfigError> {
        Config::from_toml_str(s, std::path::Path::new("config.toml"))
    }

    #[test]
    fn defaults_are_valid_and_match_spec() {
        let c = Config::default();
        c.validate().unwrap();
        let (n, p) = c.profile_with_env(None, None).unwrap();
        assert_eq!(n, "local");
        assert_eq!(p.download_dir, "~/Downloads");
        assert_eq!(p.min_free, DiskThreshold::Absolute(1 << 30));
        assert_eq!(p.warn_free, DiskThreshold::Absolute(5 << 30));
        assert_eq!(c.limits.max_concurrent, 3);
        assert_eq!(c.aria2.rpc_port, 6800);
    }

    #[test]
    fn default_file_parses_to_defaults() {
        assert_eq!(parse(DEFAULT_CONFIG_TOML).unwrap(), Config::default());
    }

    #[test]
    fn serialize_round_trip() {
        let c = Config::default();
        let text = toml::to_string(&c).unwrap();
        assert_eq!(parse(&text).unwrap(), c);
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn unknown_top_level_tables_kept_but_nested_unknowns_rejected() {
        let c = parse("[video]\nengine = \"yt-dlp\"\n").unwrap();
        assert!(c.extra.contains_key("video"));
        assert!(parse("[ui]\nbogus = 1\n").is_err());
    }

    #[test]
    fn parse_error_has_path_and_span() {
        let err = parse("[ui]\nmouse = \"yes\"\n").unwrap_err();
        let ConfigError::Parse { span, .. } = &err else {
            panic!("wrong variant: {err}");
        };
        assert!(span.is_some());
        let msg = err.to_string();
        assert!(
            msg.contains("config.toml") && msg.contains("line 2"),
            "{msg}"
        );
    }

    #[test]
    fn schedule_section_parses_validates_and_round_trips() {
        let c = parse(
            "[[schedule]]\nname = \"night\"\nstart = \"22:00\"\nend = \"06:00\"\ndownload_limit = \"10MiB\"\nmax_concurrent = 5\n\n[[schedule]]\nname = \"work\"\ndays = [\"weekdays\"]\nstart = \"09:00\"\nend = \"17:00\"\naction = \"pause\"\n",
        )
        .unwrap();
        c.validate().unwrap();
        assert!(c.extra.is_empty());
        assert_eq!(c.schedule.len(), 2);
        assert_eq!(c.schedule[0].download_limit, Some(ByteLimit(10 << 20)));
        assert_eq!(c.schedule[1].action, crate::ScheduleAction::Pause);
        let text = toml::to_string(&c).unwrap();
        assert_eq!(parse(&text).unwrap(), c);
    }

    #[test]
    fn schedule_validation_failures() {
        let dup = "[[schedule]]\nname=\"a\"\nstart=\"01:00\"\nend=\"02:00\"\n[[schedule]]\nname=\"a\"\nstart=\"03:00\"\nend=\"04:00\"\n";
        assert!(parse(dup).unwrap().validate().is_err());
        let bad_time = "[[schedule]]\nname=\"a\"\nstart=\"25:00\"\nend=\"02:00\"\n";
        assert!(parse(bad_time).is_err());
        let equal = "[[schedule]]\nname=\"a\"\nstart=\"01:00\"\nend=\"01:00\"\n";
        assert!(parse(equal).unwrap().validate().is_err());
    }

    #[test]
    fn notify_section_parses_and_validates() {
        let c = parse(
            "[notify]\ndesktop = false\non = [\"failed\"]\n[notify.webhook]\nurl = \"https://x.test/h\"\nsecret_ref = \"wh\"\n[[notify.hook]]\ncommand = [\"/bin/true\", \"a\"]\n[[notify.hook]]\non = [\"completed\"]\ncommand = [\"x\"]\ntimeout_secs = 5\n",
        )
        .unwrap();
        c.validate().unwrap();
        assert!(c.extra.is_empty());
        assert!(!c.notify.desktop && !c.notify.desktop_on_daemon);
        assert!(c.notify.enabled(crate::NotifyEvent::Failed));
        assert!(!c.notify.enabled(crate::NotifyEvent::Completed));
        let w = c.notify.webhook.as_ref().unwrap();
        assert_eq!((w.timeout_secs, w.retries), (10, 3));
        assert_eq!(c.notify.hook[0].timeout_secs, 60);
        assert_eq!(c.notify.hook[0].on.len(), 3);
        let text = toml::to_string(&c).unwrap();
        assert_eq!(parse(&text).unwrap(), c);
    }

    #[test]
    fn notify_validation_failures() {
        for bad in [
            "[notify.webhook]\nurl = \"ftp://x/\"\n",
            "[notify.webhook]\nurl = \"nonsense\"\n",
            "[notify.webhook]\nurl = \"http://x/\"\ntimeout_secs = 0\n",
            "[notify.webhook]\nurl = \"http://x/\"\nretries = 99\n",
            "[[notify.hook]]\ncommand = []\n",
            "[[notify.hook]]\ncommand = [\"\"]\n",
            "[[notify.hook]]\ncommand = [\"a\"]\non = []\n",
            "[[notify.hook]]\ncommand = [\"a\"]\ntimeout_secs = 0\n",
        ] {
            assert!(parse(bad).unwrap().validate().is_err(), "{bad}");
        }
        assert!(parse("[notify]\non = [\"bogus\"]\n").is_err());
        assert!(parse("[notify]\nbogus = 1\n").is_err());
    }

    #[test]
    fn remote_profile_parses() {
        let c = parse(
            "default_profile = \"nas\"\n[profiles.nas]\ndaemon = { kind = \"remote\", url = \"wss://nas:6810\", secret_ref = \"nas\" }\nmin_free = \"5%\"\nwarn_free = \"10%\"\n",
        )
        .unwrap();
        c.validate().unwrap();
        assert_eq!(c.profiles["nas"].min_free, DiskThreshold::Percent(5.0));
    }

    #[test]
    fn validation_failures() {
        let c = Config {
            default_profile: "nope".into(),
            ..Config::default()
        };
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.profiles.get_mut("local").unwrap().daemon = DaemonTarget::Local {
            listen: "not-an-addr".into(),
            secret_ref: "daemon".into(),
        };
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.profiles.get_mut("local").unwrap().daemon = DaemonTarget::Remote {
            url: "http://x:1".into(),
            secret_ref: "s".into(),
        };
        assert!(c.validate().is_err());

        let mut c = Config::default();
        let p = c.profiles.get_mut("local").unwrap();
        p.min_free = DiskThreshold::Absolute(10 << 30);
        assert!(c.validate().is_err());
    }

    #[test]
    fn profile_resolution_precedence() {
        let mut c = Config::default();
        c.profiles.insert("b".into(), Profile::default());
        assert_eq!(c.profile_with_env(None, None).unwrap().0, "local");
        assert_eq!(c.profile_with_env(None, Some("b")).unwrap().0, "b");
        assert_eq!(
            c.profile_with_env(Some("local"), Some("b")).unwrap().0,
            "local"
        );
        assert!(matches!(
            c.profile_with_env(Some("zzz"), None),
            Err(ConfigError::UnknownProfile(_))
        ));
    }

    #[test]
    fn load_missing_file_gives_defaults_and_write_default_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("cfg"));
        assert_eq!(Config::load(&paths).unwrap(), Config::default());
        assert!(Config::write_default(&paths).unwrap());
        assert!(!Config::write_default(&paths).unwrap());
        let loaded = Config::load(&paths).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded, Config::default());
    }

    #[test]
    fn tilde_expansion() {
        assert_eq!(expand_tilde("/abs/x"), PathBuf::from("/abs/x"));
        assert_eq!(expand_tilde("rel~/x"), PathBuf::from("rel~/x"));
        if let Ok(home) = etcetera::home_dir() {
            assert_eq!(expand_tilde("~/Downloads"), home.join("Downloads"));
        }
    }
}
