//! Persistent, authoritative record of dman's jobs, backed by `redb`.
//!
//! aria2 forgets completed and seeding items, so dman keeps its own record of every
//! job (source, options, `.torrent` bytes, gid, desired state, upload base), plus
//! cumulative transfer counters and a log of renames. The API is synchronous; the
//! daemon calls it through `spawn_blocking`. The store never touches downloaded files.

mod record;
mod store;

pub use record::{Desired, JobRecord, LiveFigures};
pub use store::{RenameEntry, SCHEMA_VERSION, SearchHistoryRecord, Store, StoreError};

/// Result alias for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;
