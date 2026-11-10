use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;
use etcetera::base_strategy::Xdg;

use crate::ConfigError;

/// Environment variable overriding the whole dman directory layout.
pub const ENV_CONFIG_DIR: &str = "DMAN_CONFIG_DIR";

/// Resolved filesystem locations used by dman.
///
/// By default the XDG layout is used on both Linux and macOS:
/// `$XDG_CONFIG_HOME/dman`, `$XDG_DATA_HOME/dman`, `$XDG_STATE_HOME/dman`
/// (falling back to `~/.config`, `~/.local/share`, `~/.local/state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
}

impl Paths {
    /// Discover paths from the environment. `DMAN_CONFIG_DIR`, if set and
    /// non-empty, acts like [`Paths::with_root`] so that several instances can
    /// live side by side without sharing any state.
    pub fn discover() -> Result<Self, ConfigError> {
        if let Some(root) = std::env::var_os(ENV_CONFIG_DIR).filter(|v| !v.is_empty()) {
            return Ok(Self::with_root(root));
        }
        let xdg = Xdg::new().map_err(|_| ConfigError::NoHome)?;
        let state = xdg
            .state_dir()
            .unwrap_or_else(|| xdg.data_dir().join("state"));
        Ok(Self {
            config_dir: xdg.config_dir().join("dman"),
            data_dir: xdg.data_dir().join("dman"),
            state_dir: state.join("dman"),
        })
    }

    /// Build a layout entirely below `root`: config in `root` itself, data in
    /// `root/data`, state in `root/state`. Used for tests and `DMAN_CONFIG_DIR`.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            config_dir: root,
        }
    }

    /// Directory holding `config.toml` and `secrets.toml`.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Path of `config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Path of `secrets.toml`.
    pub fn secrets_file(&self) -> PathBuf {
        self.config_dir.join("secrets.toml")
    }

    /// Persistent data directory (aria2 sessions, store).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// State/runtime directory (logs, lock files).
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Daemon log file.
    pub fn log_file(&self) -> PathBuf {
        self.state_dir.join("dman.log")
    }

    /// aria2 session file for `profile`.
    pub fn aria2_session_file(&self, profile: &str) -> PathBuf {
        self.data_dir.join(format!("aria2-{profile}.session"))
    }

    /// Single-instance lock file for `profile`.
    pub fn lock_file(&self, profile: &str) -> PathBuf {
        self.state_dir.join(format!("{profile}.lock"))
    }
}
