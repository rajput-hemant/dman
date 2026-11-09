//! JSON-RPC 2.0 envelopes used on the WebSocket and HTTP transports.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ErrorCode, RpcError};

pub const JSONRPC: &str = "2.0";

/// Standard JSON-RPC protocol error codes.
pub mod protocol {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
}

/// A request from a client. `id` is absent for notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: impl Serialize) -> serde_json::Result<Self> {
        Ok(Self {
            jsonrpc: JSONRPC.into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params: serde_json::to_value(params)?,
        })
    }
}

/// Error object on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl From<RpcError> for WireError {
    fn from(e: RpcError) -> Self {
        Self {
            code: e.code.rpc_code(),
            message: e.message,
            data: e.data,
        }
    }
}

impl From<WireError> for RpcError {
    fn from(e: WireError) -> Self {
        Self {
            code: ErrorCode::from_rpc_code(e.code),
            message: e.message,
            data: e.data,
        }
    }
}

/// A response from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: impl Into<WireError>) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(error.into()),
        }
    }

    /// Converts into the typed result.
    pub fn into_result<T: serde::de::DeserializeOwned>(self) -> Result<T, RpcError> {
        if let Some(e) = self.error {
            return Err(e.into());
        }
        serde_json::from_value(self.result.unwrap_or(Value::Null))
            .map_err(|e| RpcError::new(ErrorCode::General, format!("malformed response: {e}")))
    }
}

/// A server-initiated message (no id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Notification {
    pub fn new(method: &str, params: impl Serialize) -> serde_json::Result<Self> {
        Ok(Self {
            jsonrpc: JSONRPC.into(),
            method: method.into(),
            params: serde_json::to_value(params)?,
        })
    }
}

/// Anything a client can receive on the WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerMessage {
    Response(Response),
    Notification(Notification),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_message_discriminates() {
        let r: ServerMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":true}"#).unwrap();
        assert!(matches!(r, ServerMessage::Response(_)));
        let n: ServerMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"event","params":{}}"#).unwrap();
        assert!(matches!(n, ServerMessage::Notification(_)));
    }

    #[test]
    fn error_round_trip() {
        let resp = Response::err(Value::from(3), RpcError::not_found("job 7"));
        let e = resp.into_result::<()>().unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(e.message, "job 7 not found");
    }
}
