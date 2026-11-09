//! The `[torrents]` section.

use serde::{Deserialize, Serialize};

/// What to do with the partial files of deselected torrent files once the
/// torrent completes (decision D28: never automatic, always confirmed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveUnselected {
    /// Leave them alone (default).
    #[default]
    Never,
    /// Tell the user and ask before deleting anything.
    Ask,
}

/// Torrent settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Torrents {
    /// `"never"` (default) or `"ask"`: when a torrent with deselected files
    /// completes, flag it so a client can ask to delete their partial files.
    pub remove_unselected: RemoveUnselected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_defaults() {
        let t: Torrents = toml::from_str("").unwrap();
        assert_eq!(t.remove_unselected, RemoveUnselected::Never);
        let t: Torrents = toml::from_str("remove_unselected = \"ask\"").unwrap();
        assert_eq!(t.remove_unselected, RemoveUnselected::Ask);
        assert!(toml::from_str::<Torrents>("remove_unselected = \"always\"").is_err());
    }
}
