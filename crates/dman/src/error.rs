//! CLI error type: a message plus the process exit code.

use dman_client::ClientError;
use dman_config::ConfigError;
use dman_proto::ErrorCode;

/// Exit status for Ctrl-C.
pub const EXIT_INTERRUPTED: i32 = 130;

/// Exit status when `dman wait --timeout` (or `add --wait --timeout`) expires; the
/// same value as coreutils `timeout`.
pub const EXIT_TIMEOUT: i32 = 124;
/// Exit status of `add` when every source was already in the downloads, so
/// scripts can tell "nothing to do" from a failure.
pub const EXIT_DUPLICATE: i32 = 9;

/// An error that ends the process with a message and an exit code.
#[derive(Debug, Clone)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
    pub exit: i32,
    /// The next step for the user, printed on its own line after the message.
    pub hint: Option<String>,
}

impl CliError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit: code.exit_code(),
            hint: None,
        }
    }

    /// Attach the concrete next step (kept when one is already set).
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        if self.hint.is_none() {
            self.hint = Some(hint.into());
        }
        self
    }

    pub fn general(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::General, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Refused, message)
    }

    pub fn unreachable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unreachable, message)
    }

    /// Ctrl-C: exits with 130 and prints nothing.
    pub fn interrupted() -> Self {
        Self {
            code: ErrorCode::General,
            message: String::new(),
            exit: EXIT_INTERRUPTED,
            hint: None,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The next step for a failed daemon call, where the error alone tells which.
fn client_hint(e: &ClientError) -> Option<String> {
    Some(match e {
        ClientError::Connect(_) => "start the daemon with `dman daemon start`".to_owned(),
        ClientError::Disconnected | ClientError::Timeout => {
            "check the daemon with `dman daemon status` (restart: `dman daemon restart`), then retry"
                .to_owned()
        }
        ClientError::Unauthorized => "the token does not match this daemon: check the secret \
             named by the profile in secrets.toml, or restart a local daemon with \
             `dman daemon restart`"
            .to_owned(),
        ClientError::IncompatibleVersion { .. } => {
            "use the same dman version on both sides (`dman update`)".to_owned()
        }
        ClientError::Rpc(r) => return rpc_hint(r),
        ClientError::Protocol(_) => {
            "the peer is not a dman daemon; check the profile's URL (`dman config show`)".to_owned()
        }
    })
}

/// The next step for an error the daemon returned, recognised by its code and text.
pub fn rpc_hint(r: &dman_proto::RpcError) -> Option<String> {
    let m = r.message.as_str();
    match r.code {
        ErrorCode::NotFound if m.starts_with("job ") => Some("see `dman list`".to_owned()),
        ErrorCode::NotFound if m.contains("provider") => {
            Some("see `dman providers list`".to_owned())
        }
        ErrorCode::EngineUnavailable if m.contains("aria2") => Some(
            "install aria2 (apt install aria2 / brew install aria2), then run \
             `dman daemon engine-restart`"
                .to_owned(),
        ),
        ErrorCode::EngineUnavailable if m.contains("yt-dlp") && !m.contains("install yt-dlp") => {
            Some(
                "install yt-dlp and ffmpeg (pipx install yt-dlp / brew install yt-dlp), then \
                 run `dman daemon engine-restart`"
                    .to_owned(),
            )
        }
        ErrorCode::Refused if m.contains("provider") && m.contains("disabled") => {
            Some("enable it with `dman providers enable NAME`".to_owned())
        }
        ErrorCode::Refused if m.contains("is not removed") => {
            Some("only removed jobs can be restored; see `dman list`".to_owned())
        }
        ErrorCode::Refused if m.contains("not finished; remove it first") => {
            Some("run `dman remove ID` first".to_owned())
        }
        ErrorCode::Refused if m.contains("cannot be paused") => {
            Some("see which jobs are still running with `dman list`".to_owned())
        }
        ErrorCode::Unauthorized => client_hint(&ClientError::Unauthorized),
        ErrorCode::InvalidInput => {
            Some("check the value; `dman <command> --help` lists what is accepted".to_owned())
        }
        _ => None,
    }
}

impl From<ClientError> for CliError {
    fn from(e: ClientError) -> Self {
        let code = match &e {
            ClientError::Connect(_) | ClientError::Disconnected | ClientError::Timeout => {
                ErrorCode::Unreachable
            }
            ClientError::Unauthorized => ErrorCode::Unauthorized,
            ClientError::Rpc(r) => r.code,
            ClientError::IncompatibleVersion { .. } | ClientError::Protocol(_) => {
                ErrorCode::General
            }
        };
        let mut err = Self::new(code, e.to_string());
        err.hint = client_hint(&e);
        err
    }
}

impl From<ConfigError> for CliError {
    fn from(e: ConfigError) -> Self {
        let hint = match &e {
            ConfigError::Parse { .. } | ConfigError::Invalid(_) => {
                Some("fix it with `dman config edit` (`dman config path` shows the file)")
            }
            ConfigError::UnknownProfile(_) => {
                Some("profiles are listed under [profiles] in `dman config show`")
            }
            ConfigError::NoHome => Some("set DMAN_CONFIG_DIR to a directory dman may use"),
            ConfigError::BadPath(_) => Some("see the key names with `dman config show`"),
            ConfigError::InsecureSecrets { .. }
            | ConfigError::Io { .. }
            | ConfigError::Random(_) => None,
        };
        let err = match e {
            ConfigError::UnknownProfile(_) => Self::invalid(e.to_string()),
            other => Self::general(other.to_string()),
        };
        match hint {
            Some(h) => err.with_hint(h),
            None => err,
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            // The reader of our output went away (`dman ... | head`): a normal end.
            return Self {
                code: ErrorCode::General,
                message: String::new(),
                exit: 0,
                hint: None,
            };
        }
        Self::general(e.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        Self::general(e.to_string())
    }
}

/// Process exit code for a batch where `failed` of `total` items failed, given the
/// code of the first failure: 0 if none, 8 if some, the failure's own code if all.
pub fn batch_exit(total: usize, failed: usize, first: Option<ErrorCode>) -> i32 {
    match (failed, first) {
        (0, _) | (_, None) => 0,
        (f, Some(code)) if f >= total => code.exit_code(),
        _ => ErrorCode::PartialFailure.exit_code(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_exit_codes() {
        assert_eq!(batch_exit(3, 0, None), 0);
        assert_eq!(batch_exit(3, 1, Some(ErrorCode::NotFound)), 8);
        assert_eq!(batch_exit(2, 2, Some(ErrorCode::NotFound)), 5);
    }

    #[test]
    fn help_documents_every_exit_code() {
        use dman_proto::ErrorCode::*;
        let help = crate::cli::LONG_ABOUT;
        for code in [
            General,
            InvalidInput,
            Unreachable,
            Unauthorized,
            NotFound,
            Refused,
            EngineUnavailable,
            PartialFailure,
        ] {
            let line = format!("  {}  ", code.exit_code());
            assert!(
                help.contains(&line),
                "exit code {} missing from --help",
                code.exit_code()
            );
        }
        assert!(help.contains(&format!("  {}  ", EXIT_INTERRUPTED)));
        assert!(help.contains(&format!("  {}  ", EXIT_TIMEOUT)));
        assert!(help.contains(&format!("  {}    ", EXIT_DUPLICATE)));
    }

    #[test]
    fn daemon_errors_end_with_a_next_step() {
        let e = CliError::from(ClientError::Connect("refused".into()));
        assert!(e.hint.unwrap().contains("dman daemon start"));
        let nf = ClientError::Rpc(dman_proto::RpcError::not_found("job 12"));
        let e = CliError::from(nf);
        assert_eq!(e.exit, 5);
        assert!(e.hint.unwrap().contains("dman list"));
        let aria = dman_proto::RpcError::new(ErrorCode::EngineUnavailable, "aria2 is not running");
        assert!(rpc_hint(&aria).unwrap().contains("apt install aria2"));
        let p = dman_proto::RpcError::refused("provider `x` is disabled");
        assert!(rpc_hint(&p).unwrap().contains("providers enable"));
        let e = CliError::from(ClientError::Unauthorized);
        assert!(e.hint.unwrap().contains("secrets.toml"));
    }

    #[test]
    fn with_hint_keeps_the_first_hint() {
        let e = CliError::general("x").with_hint("a").with_hint("b");
        assert_eq!(e.hint.as_deref(), Some("a"));
    }

    #[test]
    fn client_errors_map_to_exit_codes() {
        assert_eq!(CliError::from(ClientError::Disconnected).exit, 3);
        assert_eq!(CliError::from(ClientError::Unauthorized).exit, 4);
    }
}
