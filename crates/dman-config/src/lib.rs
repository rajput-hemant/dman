//! Configuration, filesystem layout and secrets for dman.
//!
//! * [`Paths`] - where files live (XDG layout on Linux and macOS).
//! * [`Config`] - the typed `config.toml`, with load and validation.
//! * [`ConfigDoc`] - comment-preserving edits.
//! * [`Secrets`] - `secrets.toml` with permission enforcement.

mod edit;
mod error;
mod fsutil;
mod keys;
mod notify;
mod paths;
mod redact;
mod schedule;
mod schema;
mod search;
mod secrets;
mod tls;
mod torrents;
mod units;
mod update;
mod video;
mod web;

pub use edit::ConfigDoc;
pub use error::ConfigError;
pub use keys::{
    REDACTED, Value as TomlValue, config_keys, effective_value, lookup, redact_config, render_toml,
    render_value, split_key, top_level_keys,
};
pub use notify::{Hook, Notify, NotifyEvent, Webhook};
pub use paths::{ENV_CONFIG_DIR, Paths};
pub use schedule::{ClockTime, DaySpec, ScheduleAction, ScheduleWindow, Weekday, validate_windows};
pub use schema::{
    Aria2, Config, DEFAULT_CONFIG_TOML, DaemonTarget, ENV_PROFILE, Limits, Profile, Ui,
    expand_tilde,
};
pub use search::{Search, TrackerList};
pub use secrets::Secrets;
pub use tls::{TlsFiles, read_certs, read_key};
pub use torrents::{RemoveUnselected, Torrents};
pub use units::{ByteLimit, DiskThreshold, format_bytes, parse_bytes};
pub use update::Update;
pub use video::{Video, VideoSubtitles};
pub use web::{Web, parse_bind, validate_base_path};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_layout() {
        let p = Paths::with_root("/tmp/x");
        assert_eq!(p.config_file(), std::path::Path::new("/tmp/x/config.toml"));
        assert_eq!(
            p.secrets_file(),
            std::path::Path::new("/tmp/x/secrets.toml")
        );
        assert_eq!(p.log_file(), std::path::Path::new("/tmp/x/state/dman.log"));
        assert_eq!(
            p.aria2_session_file("local"),
            std::path::Path::new("/tmp/x/data/aria2-local.session")
        );
        assert_eq!(
            p.lock_file("local"),
            std::path::Path::new("/tmp/x/state/local.lock")
        );
    }
}
