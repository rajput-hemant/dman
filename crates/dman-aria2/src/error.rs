//! Error types for the aria2 client.

use serde::Deserialize;

/// An error object reported by aria2 itself (`{"code":1,"message":"..."}`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Aria2Error {
    /// aria2 error code.
    pub code: i64,
    /// Human readable message.
    pub message: String,
}

impl std::fmt::Display for Aria2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "aria2 error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for Aria2Error {}

/// Errors returned by [`crate::Aria2Client`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The WebSocket connection could not be established.
    #[error("failed to connect to aria2: {0}")]
    Connect(String),
    /// The connection is closed; the client must be recreated.
    #[error("disconnected from aria2")]
    Disconnected,
    /// The call did not receive a response in time.
    #[error("aria2 call timed out")]
    Timeout,
    /// aria2 answered with an error object.
    #[error("aria2 error {code}: {message}")]
    Rpc {
        /// aria2 error code.
        code: i64,
        /// aria2 error message.
        message: String,
    },
    /// The peer sent something that is not valid aria2 JSON-RPC.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// A value could not be (de)serialized.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<Aria2Error> for Error {
    fn from(e: Aria2Error) -> Self {
        Error::Rpc {
            code: e.code,
            message: e.message,
        }
    }
}

/// Result alias using [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
