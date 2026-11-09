//! Error codes shared by the API and the CLI exit codes.

use serde::{Deserialize, Serialize};

/// Application error codes. JSON-RPC reserves -32768..=-32000, so dman uses
/// small positive numbers that double as CLI exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Anything not covered below.
    General,
    /// Invalid parameters or usage.
    InvalidInput,
    /// The daemon could not be reached.
    Unreachable,
    /// Missing or wrong token.
    Unauthorized,
    /// The referenced job, provider or file does not exist.
    NotFound,
    /// Refused by a safety rule (disk space, rename of an active transfer, ...).
    Refused,
    /// A required engine is missing or unavailable.
    EngineUnavailable,
    /// Some items of a batch failed.
    PartialFailure,
}

impl ErrorCode {
    /// Process exit code used by the CLI for this error.
    pub const fn exit_code(self) -> i32 {
        match self {
            ErrorCode::General => 1,
            ErrorCode::InvalidInput => 2,
            ErrorCode::Unreachable => 3,
            ErrorCode::Unauthorized => 4,
            ErrorCode::NotFound => 5,
            ErrorCode::Refused => 6,
            ErrorCode::EngineUnavailable => 7,
            ErrorCode::PartialFailure => 8,
        }
    }

    /// Numeric code used inside JSON-RPC error objects.
    pub const fn rpc_code(self) -> i64 {
        self.exit_code() as i64
    }

    /// Inverse of [`ErrorCode::rpc_code`]; JSON-RPC protocol errors map to `General`
    /// except "invalid params", which maps to `InvalidInput`.
    pub const fn from_rpc_code(code: i64) -> Self {
        match code {
            2 | -32602 => ErrorCode::InvalidInput,
            3 => ErrorCode::Unreachable,
            4 => ErrorCode::Unauthorized,
            5 => ErrorCode::NotFound,
            6 => ErrorCode::Refused,
            7 => ErrorCode::EngineUnavailable,
            8 => ErrorCode::PartialFailure,
            _ => ErrorCode::General,
        }
    }
}

/// An error returned by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
    /// Optional structured details (e.g. per-item failures of a batch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("{what} not found"))
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Refused, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_codes_round_trip() {
        for c in [
            ErrorCode::General,
            ErrorCode::InvalidInput,
            ErrorCode::Unreachable,
            ErrorCode::Unauthorized,
            ErrorCode::NotFound,
            ErrorCode::Refused,
            ErrorCode::EngineUnavailable,
            ErrorCode::PartialFailure,
        ] {
            assert_eq!(ErrorCode::from_rpc_code(c.rpc_code()), c);
        }
    }
}
