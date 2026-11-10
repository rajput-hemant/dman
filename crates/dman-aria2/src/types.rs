//! Typed aria2 RPC data structures.
//!
//! aria2 encodes almost every number and boolean as a string; the
//! deserializers here accept both the string and the native JSON form.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

/// An aria2 download identifier (16 hex characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Gid(String);

impl Gid {
    /// Wrap a string as a gid without validation.
    pub fn new(s: impl Into<String>) -> Self {
        Gid(s.into())
    }

    /// The gid as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this looks like a well-formed gid (16 hex digits).
    pub fn is_valid(&self) -> bool {
        self.0.len() == 16 && self.0.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

impl fmt::Display for Gid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Gid {
    fn from(s: &str) -> Self {
        Gid(s.to_owned())
    }
}

impl From<String> for Gid {
    fn from(s: String) -> Self {
        Gid(s)
    }
}

impl AsRef<str> for Gid {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// aria2 options: always string-valued key/value pairs.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Options(pub BTreeMap<String, String>);

/// Options whose values can carry credentials (passwords, auth headers,
/// proxy URLs with `user:pass@`).
fn is_sensitive(key: &str) -> bool {
    key.ends_with("passwd") || key.ends_with("proxy") || key == "header"
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut m = f.debug_map();
        for (k, v) in &self.0 {
            if is_sensitive(k) {
                m.entry(k, &"<redacted>");
            } else {
                m.entry(k, v);
            }
        }
        m.finish()
    }
}

impl Options {
    /// Empty option set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// Insert an option, replacing any previous value.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    /// Look an option up.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Whether no option is set.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Options {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Options(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

/// How [`crate::Aria2Client::change_position`] interprets its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosHow {
    /// Absolute position from the queue start.
    Set,
    /// Relative to the current position.
    Cur,
    /// Relative to the queue end.
    End,
}

impl PosHow {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PosHow::Set => "POS_SET",
            PosHow::Cur => "POS_CUR",
            PosHow::End => "POS_END",
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StrOr<T> {
    Val(T),
    Str(String),
}

/// Deserialize an optional number that aria2 may send as a string.
pub(crate) fn de_opt_num<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr + Deserialize<'de>,
    T::Err: fmt::Display,
{
    match Option::<StrOr<T>>::deserialize(d)? {
        None => Ok(None),
        Some(StrOr::Val(v)) => Ok(Some(v)),
        Some(StrOr::Str(s)) if s.is_empty() => Ok(None),
        Some(StrOr::Str(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// Deserialize an optional bool that aria2 sends as `"true"`/`"false"`.
pub(crate) fn de_opt_bool<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<StrOr<bool>>::deserialize(d)? {
        None => Ok(None),
        Some(StrOr::Val(v)) => Ok(Some(v)),
        Some(StrOr::Str(s)) => match s.as_str() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            "" => Ok(None),
            other => Err(serde::de::Error::custom(format!("invalid bool {other:?}"))),
        },
    }
}

/// Download state as reported in `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    /// Currently downloading/seeding.
    Active,
    /// In the queue.
    Waiting,
    /// Paused.
    Paused,
    /// Stopped because of an error.
    Error,
    /// Finished.
    Complete,
    /// Removed by the user.
    Removed,
    /// A state this client does not know.
    #[serde(other)]
    Unknown,
}

/// A URI entry of a file (`uris` in `getFiles`, result of `getUris`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FileUri {
    /// The URI.
    #[serde(default)]
    pub uri: String,
    /// `used` or `waiting`.
    #[serde(default)]
    pub status: Option<String>,
}

/// A file of a download.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    /// 1-based file index.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub index: Option<u64>,
    /// Path on disk.
    #[serde(default)]
    pub path: Option<String>,
    /// Total length in bytes.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub length: Option<u64>,
    /// Completed length in bytes.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub completed_length: Option<u64>,
    /// Whether the file is selected for download.
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub selected: Option<bool>,
    /// URIs of the file.
    #[serde(default)]
    pub uris: Vec<FileUri>,
}

/// BitTorrent info dictionary excerpt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BtInfo {
    /// Torrent name.
    #[serde(default)]
    pub name: Option<String>,
}

/// BitTorrent metadata of a download.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BitTorrent {
    /// Tracker tiers.
    #[serde(default)]
    pub announce_list: Vec<Vec<String>>,
    /// Torrent comment.
    #[serde(default)]
    pub comment: Option<String>,
    /// Creation time (epoch seconds).
    #[serde(default, deserialize_with = "de_opt_num")]
    pub creation_date: Option<u64>,
    /// `single` or `multi`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Info dictionary excerpt.
    #[serde(default)]
    pub info: Option<BtInfo>,
}

/// Download status (`tellStatus` and friends). Every field is optional because
/// the `keys` parameter can filter them out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Download id.
    #[serde(default)]
    pub gid: Option<Gid>,
    /// Download state.
    #[serde(default)]
    pub status: Option<DownloadStatus>,
    /// Total length in bytes.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub total_length: Option<u64>,
    /// Completed length in bytes.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub completed_length: Option<u64>,
    /// Uploaded length in bytes.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub upload_length: Option<u64>,
    /// Download speed in bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub download_speed: Option<u64>,
    /// Upload speed in bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub upload_speed: Option<u64>,
    /// Number of connections.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub connections: Option<u64>,
    /// Number of connected seeders.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub num_seeders: Option<u64>,
    /// Whether the local client is seeding.
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub seeder: Option<bool>,
    /// Piece length in bytes.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub piece_length: Option<u64>,
    /// Number of pieces.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub num_pieces: Option<u64>,
    /// Hex-encoded piece bitfield.
    #[serde(default)]
    pub bitfield: Option<String>,
    /// BitTorrent info hash.
    #[serde(default)]
    pub info_hash: Option<String>,
    /// Error code of a failed download.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub error_code: Option<u64>,
    /// Error message of a failed download.
    #[serde(default)]
    pub error_message: Option<String>,
    /// Gids this download was replaced by (e.g. metadata -> content).
    #[serde(default)]
    pub followed_by: Option<Vec<Gid>>,
    /// Gid this download follows.
    #[serde(default)]
    pub following: Option<Gid>,
    /// Parent gid.
    #[serde(default)]
    pub belongs_to: Option<Gid>,
    /// Download directory.
    #[serde(default)]
    pub dir: Option<String>,
    /// Files of the download.
    #[serde(default)]
    pub files: Option<Vec<File>>,
    /// BitTorrent metadata.
    #[serde(default)]
    pub bittorrent: Option<BitTorrent>,
    /// Verified length while hash checking.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub verified_length: Option<u64>,
    /// Whether a hash check is queued.
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub verify_integrity_pending: Option<bool>,
}

/// A BitTorrent peer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    /// Percent-encoded peer id.
    #[serde(default)]
    pub peer_id: Option<String>,
    /// Peer IP address.
    #[serde(default)]
    pub ip: Option<String>,
    /// Peer port.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub port: Option<u16>,
    /// Hex-encoded bitfield of the peer.
    #[serde(default)]
    pub bitfield: Option<String>,
    /// Whether we choke the peer.
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub am_choking: Option<bool>,
    /// Whether the peer chokes us.
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub peer_choking: Option<bool>,
    /// Download speed from the peer, bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub download_speed: Option<u64>,
    /// Upload speed to the peer, bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub upload_speed: Option<u64>,
    /// Whether the peer is a seeder.
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub seeder: Option<bool>,
}

/// One server of a file (`getServers`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    /// Original URI.
    #[serde(default)]
    pub uri: Option<String>,
    /// URI currently in use (after redirects).
    #[serde(default)]
    pub current_uri: Option<String>,
    /// Download speed, bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub download_speed: Option<u64>,
}

/// Servers grouped by file index (`getServers`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ServerGroup {
    /// 1-based file index.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub index: Option<u64>,
    /// Servers in use for that file.
    #[serde(default)]
    pub servers: Vec<Server>,
}

/// Global statistics (`getGlobalStat`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalStat {
    /// Overall download speed, bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub download_speed: Option<u64>,
    /// Overall upload speed, bytes/s.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub upload_speed: Option<u64>,
    /// Number of active downloads.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub num_active: Option<u64>,
    /// Number of waiting downloads.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub num_waiting: Option<u64>,
    /// Number of stopped downloads kept in memory.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub num_stopped: Option<u64>,
    /// Number of stopped downloads including those evicted.
    #[serde(default, deserialize_with = "de_opt_num")]
    pub num_stopped_total: Option<u64>,
}

/// aria2 version information.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    /// Version string, e.g. `1.37.0`.
    #[serde(default)]
    pub version: String,
    /// Compiled-in features.
    #[serde(default)]
    pub enabled_features: Vec<String>,
}

/// Session information (`getSessionInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Session id.
    #[serde(default)]
    pub session_id: String,
}

/// Kind of a server notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Aria2Event {
    /// `aria2.onDownloadStart`
    DownloadStart,
    /// `aria2.onDownloadPause`
    DownloadPause,
    /// `aria2.onDownloadStop`
    DownloadStop,
    /// `aria2.onDownloadComplete`
    DownloadComplete,
    /// `aria2.onDownloadError`
    DownloadError,
    /// `aria2.onBtDownloadComplete`
    BtDownloadComplete,
}

impl Aria2Event {
    pub(crate) fn from_method(method: &str) -> Option<Self> {
        Some(match method {
            "aria2.onDownloadStart" => Self::DownloadStart,
            "aria2.onDownloadPause" => Self::DownloadPause,
            "aria2.onDownloadStop" => Self::DownloadStop,
            "aria2.onDownloadComplete" => Self::DownloadComplete,
            "aria2.onDownloadError" => Self::DownloadError,
            "aria2.onBtDownloadComplete" => Self::BtDownloadComplete,
            _ => return None,
        })
    }
}

/// A notification pushed by aria2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aria2Notification {
    /// What happened.
    pub event: Aria2Event,
    /// Which download it happened to.
    pub gid: Gid,
}
