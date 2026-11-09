//! Error types.

use thiserror::Error;

/// A provider definition is invalid (bad URL template, bad selector, ...).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider `{provider}`: {field}: {message}")]
pub struct DefError {
    pub provider: String,
    pub field: String,
    pub message: String,
}

impl DefError {
    pub fn new(provider: &str, field: &str, message: impl Into<String>) -> Self {
        Self {
            provider: provider.to_string(),
            field: field.to_string(),
            message: message.into(),
        }
    }
}

/// A search or probe against one provider failed.
#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    #[error("HTTP {status} from {url}")]
    Http { status: u16, url: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("timed out after {0}s")]
    Timeout(u64),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid provider definition: {0}")]
    Definition(String),
    #[error("provider reported an error: {0}")]
    Remote(String),
    #[error("missing secret `{0}` for provider API key")]
    MissingSecret(String),
    #[error("unknown or disabled provider `{0}`")]
    Unknown(String),
}

impl From<DefError> for ProviderError {
    fn from(e: DefError) -> Self {
        Self::Definition(e.to_string())
    }
}
