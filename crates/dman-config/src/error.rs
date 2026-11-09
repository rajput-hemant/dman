use std::ops::Range;
use std::path::PathBuf;

/// Errors produced while locating, reading, validating or writing configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// An I/O operation on `path` failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A TOML file could not be parsed or did not match the schema.
    #[error("invalid configuration in {}:\n{message}", path.display())]
    Parse {
        /// File that failed to parse.
        path: PathBuf,
        /// Human readable message (includes line and a source excerpt).
        message: String,
        /// Byte span of the offending text, when known.
        span: Option<Range<usize>>,
    },
    /// The configuration parsed but is semantically invalid.
    #[error("invalid configuration: {0}")]
    Invalid(String),
    /// The secrets file is readable by group or others.
    #[error(
        "{} has insecure permissions {mode:04o}; run `chmod 600 {}`",
        path.display(),
        path.display()
    )]
    InsecureSecrets {
        /// Secrets file path.
        path: PathBuf,
        /// Offending permission bits.
        mode: u32,
    },
    /// The requested profile does not exist.
    #[error("unknown profile `{0}`")]
    UnknownProfile(String),
    /// No home directory could be determined.
    #[error("cannot determine home directory; set DMAN_CONFIG_DIR or HOME")]
    NoHome,
    /// The operating system could not provide random bytes.
    #[error("cannot generate random token: {0}")]
    Random(String),
    /// A path passed to `ConfigDoc::set` is unusable.
    #[error("cannot set `{0}`: path is empty or crosses a non-table value")]
    BadPath(String),
}

impl ConfigError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
