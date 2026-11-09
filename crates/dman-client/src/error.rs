//! Client error type.

use dman_proto::{ErrorCode, RpcError};

/// Everything that can go wrong when talking to the daemon.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    /// The connection could not be established.
    #[error("cannot connect to daemon: {0}")]
    Connect(String),
    /// The connection is closed or was lost.
    #[error("disconnected from daemon")]
    Disconnected,
    /// The daemon did not answer within the per-call timeout.
    #[error("request timed out")]
    Timeout,
    /// The daemon rejected the token.
    #[error("unauthorized: wrong or missing token")]
    Unauthorized,
    /// Daemon and client speak different major API versions.
    #[error("incompatible API version (daemon {daemon}, client {client})")]
    IncompatibleVersion {
        /// Version announced by the daemon.
        daemon: u32,
        /// Version this client implements.
        client: u32,
    },
    /// The daemon answered with an error.
    #[error("{0}")]
    Rpc(RpcError),
    /// The peer violated the protocol (bad JSON, malformed result, ...).
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl ClientError {
    /// Process exit code for the CLI, consistent with [`ErrorCode::exit_code`].
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Connect(_) | Self::Disconnected | Self::Timeout => {
                ErrorCode::Unreachable.exit_code()
            }
            Self::Unauthorized => ErrorCode::Unauthorized.exit_code(),
            Self::Rpc(e) => e.code.exit_code(),
            Self::IncompatibleVersion { .. } | Self::Protocol(_) => ErrorCode::General.exit_code(),
        }
    }

    /// Whether retrying can never help (bad token, incompatible daemon).
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Unauthorized | Self::IncompatibleVersion { .. })
    }
}

impl From<RpcError> for ClientError {
    fn from(e: RpcError) -> Self {
        Self::Rpc(e)
    }
}
