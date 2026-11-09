//! Typed aria2 JSON-RPC client over WebSocket.
//!
//! See [`Aria2Client`]. All numeric strings sent by aria2 are parsed into
//! native numbers in the types of [`types`].

mod client;
mod error;
pub mod options;
pub mod supervisor;
pub mod types;

pub use client::Aria2Client;
pub use error::{Aria2Error, Error, Result};
pub use options::{
    CHANGEABLE_WHILE_ACTIVE, OptionError, global_limits, job_options_to_aria2, requires_pause,
};
pub use supervisor::{
    Aria2Settings, Capabilities, EngineInfo, EngineState, Supervisor, SupervisorError, render_conf,
    render_conf_redacted, write_conf,
};
pub use types::{
    Aria2Event, Aria2Notification, BitTorrent, BtInfo, DownloadStatus, File, FileUri, Gid,
    GlobalStat, Options, Peer, PosHow, Server, ServerGroup, SessionInfo, Status, Version,
};
