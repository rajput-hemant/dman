//! Typed aria2 JSON-RPC client over WebSocket.
//!
//! See [`Aria2Client`]. All numeric strings sent by aria2 are parsed into
//! native numbers in the types of [`types`].

mod client;
mod error;
pub mod types;

pub use client::Aria2Client;
pub use error::{Aria2Error, Error, Result};
pub use types::{
    Aria2Event, Aria2Notification, BitTorrent, BtInfo, DownloadStatus, File, FileUri, Gid,
    GlobalStat, Options, Peer, PosHow, Server, ServerGroup, SessionInfo, Status, Version,
};
