//! Async client for the dman daemon API (JSON-RPC 2.0 over WebSocket).
//!
//! * [`Client`] is one authenticated connection with typed methods for every
//!   daemon call, plus [`Client::subscribe`] for pushed [`dman_proto::method::Event`]s.
//! * [`ReconnectingClient`] supervises a connection with exponential backoff for
//!   long-running front ends such as the TUI. Calls made while it is disconnected
//!   fail immediately with [`ClientError::Disconnected`]; they are never queued.
//!
//! One-shot calls (CLI) use the WebSocket too: [`Client::call_once`] connects,
//! says hello, performs one call and closes. No HTTP transport is implemented
//! (that would need either a new dependency or a hand-written HTTP client; the
//! WebSocket path is simpler and needs neither).
//!
//! The URL is generic (`ws://host:port/api/v1/ws`). `wss://` becomes available
//! by enabling a TLS feature of `tokio-tungstenite` in `Cargo.toml`.

mod client;
mod error;
mod reconnect;

pub use client::Client;
pub use error::ClientError;
pub use reconnect::{ConnState, ReconnectingClient};
