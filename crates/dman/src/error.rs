//! CLI error type: a message plus the process exit code.

use dman_client::ClientError;
use dman_config::ConfigError;
use dman_proto::ErrorCode;

/// Exit status for Ctrl-C.
pub const EXIT_INTERRUPTED: i32 = 130;

/// An error that ends the process with a message and an exit code.
#[derive(Debug, Clone)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
    pub exit: i32,
}

impl CliError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit: code.exit_code(),
        }
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
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
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
        Self::new(code, e.to_string())
    }
}

impl From<ConfigError> for CliError {
    fn from(e: ConfigError) -> Self {
        match e {
            ConfigError::UnknownProfile(_) => Self::invalid(e.to_string()),
            other => Self::general(other.to_string()),
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
    fn client_errors_map_to_exit_codes() {
        assert_eq!(CliError::from(ClientError::Disconnected).exit, 3);
        assert_eq!(CliError::from(ClientError::Unauthorized).exit, 4);
    }
}
