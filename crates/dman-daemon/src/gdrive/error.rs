//! Typed errors for Google Drive folder expansion.

use std::fmt;

use dman_proto::{ErrorCode, RpcError};

#[derive(Debug, PartialEq, Eq)]
pub enum GdriveError {
    AuthUnsupported,
    QuotaExceeded { message: String },
    MissingBlob,
    ConfirmTokenNotFound,
    EmptyFolder,
    Network(String),
    Json(String),
}

impl fmt::Display for GdriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthUnsupported => write!(
                f,
                "Google Drive authentication is unsupported: folder must be public or link-shared"
            ),
            Self::QuotaExceeded { message } => {
                write!(f, "Google Drive quota exceeded: {message}")
            }
            Self::MissingBlob => {
                write!(
                    f,
                    "Google Drive folder page missing initial view data (_DRIVE_ivd)"
                )
            }
            Self::ConfirmTokenNotFound => {
                write!(f, "Google Drive confirm token not found")
            }
            Self::EmptyFolder => write!(f, "Google Drive folder contains no files"),
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Json(msg) => write!(f, "JSON error: {msg}"),
        }
    }
}

impl std::error::Error for GdriveError {}

impl From<GdriveError> for RpcError {
    fn from(e: GdriveError) -> Self {
        match e {
            GdriveError::AuthUnsupported => RpcError::invalid(e.to_string()),
            GdriveError::QuotaExceeded { .. } => RpcError::new(ErrorCode::General, e.to_string()),
            GdriveError::MissingBlob => RpcError::invalid(e.to_string()),
            GdriveError::ConfirmTokenNotFound => RpcError::new(ErrorCode::General, e.to_string()),
            GdriveError::EmptyFolder => RpcError::invalid(e.to_string()),
            GdriveError::Network(msg) => RpcError::new(ErrorCode::General, msg),
            GdriveError::Json(msg) => RpcError::new(ErrorCode::General, msg),
        }
    }
}
