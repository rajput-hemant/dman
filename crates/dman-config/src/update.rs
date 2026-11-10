//! The `[update]` section: the opt-in update check.

use serde::{Deserialize, Serialize};

/// Update settings. The check is off by default; with it off dman never
/// contacts the network for updates (`dman update --check` always works).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Update {
    /// Look for a newer release at most once a day and print a notice.
    pub check: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_and_strict() {
        assert!(!Update::default().check);
        let u: Update = toml::from_str("check = true").unwrap();
        assert!(u.check);
        assert!(toml::from_str::<Update>("chek = true").is_err());
    }
}
