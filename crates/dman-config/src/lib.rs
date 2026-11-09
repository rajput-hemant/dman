//! Configuration, filesystem layout and secrets for dman.
//!
//! * [`Paths`] - where files live (XDG layout on Linux and macOS, decision D16).
//! * [`Config`] - the typed `config.toml`, with load and validation.
//! * [`ConfigDoc`] - comment-preserving edits (decision Q5).
//! * [`Secrets`] - `secrets.toml` with permission enforcement (decision D21).

mod edit;
mod error;
mod fsutil;
mod paths;
mod schema;
mod secrets;
mod units;

pub use edit::ConfigDoc;
pub use error::ConfigError;
pub use paths::{ENV_CONFIG_DIR, Paths};
pub use schema::{
    Aria2, Config, DEFAULT_CONFIG_TOML, DaemonTarget, ENV_PROFILE, Limits, Profile, Ui,
    expand_tilde,
};
pub use secrets::Secrets;
pub use units::{ByteLimit, DiskThreshold, format_bytes, parse_bytes};

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
