//! The `[torrents]` section.

use serde::{Deserialize, Serialize};

/// What to do with the partial files of deselected torrent files once the
/// torrent completes (never automatic, always confirmed).
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Torrents {
    /// `"never"` (default) or `"ask"`: when a torrent with deselected files
    /// completes, flag it so a client can ask to delete their partial files.
    pub remove_unselected: RemoveUnselected,
    /// Keep seeding after a torrent completes (default `false`: a finished
    /// torrent is complete at once). A download can override this, and setting
    /// a seed ratio/time on one download implies seeding for it.
    pub seed: bool,
    /// Stop seeding at this upload/download ratio (`0` = no ratio limit).
    /// Only used when seeding is on.
    pub seed_ratio: f64,
    /// Stop seeding after this many minutes (`0` = no time limit). Only used
    /// when seeding is on.
    pub seed_time_minutes: u64,
}

impl Default for Torrents {
    fn default() -> Self {
        Self {
            remove_unselected: RemoveUnselected::Never,
            seed: false,
            seed_ratio: 1.0,
            seed_time_minutes: 0,
        }
    }
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

    #[test]
    fn seeding_is_off_by_default() {
        let t: Torrents = toml::from_str("").unwrap();
        assert!(!t.seed);
        assert_eq!(t.seed_ratio, 1.0);
        assert_eq!(t.seed_time_minutes, 0);
        assert_eq!(t, Torrents::default());
        let t: Torrents =
            toml::from_str("seed = true\nseed_ratio = 2.5\nseed_time_minutes = 90").unwrap();
        assert!(t.seed);
        assert_eq!(t.seed_ratio, 2.5);
        assert_eq!(t.seed_time_minutes, 90);
    }
}
