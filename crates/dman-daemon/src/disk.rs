//! Free-space queries for the disk guard.

use std::path::Path;

use dman_config::DiskThreshold;

/// Free and total bytes of a volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    /// Bytes available to unprivileged users.
    pub free: u64,
    /// Capacity in bytes.
    pub total: u64,
}

/// Space of the volume holding `path`. A path that does not exist yet is
/// resolved to its nearest existing ancestor. `None` if nothing can be queried.
pub fn space(path: &Path) -> Option<Space> {
    let mut p = path;
    loop {
        if p.exists() {
            break;
        }
        p = p.parent()?;
    }
    let st = nix::sys::statvfs::statvfs(p).ok()?;
    let frsize = st.fragment_size() as u64;
    Some(Space {
        free: (st.blocks_available() as u64).saturating_mul(frsize),
        total: (st.blocks() as u64).saturating_mul(frsize),
    })
}

/// Whether a job may not run because `space` is below `min_free`. An unknown
/// free space never blocks (the guard must not stall everything when the
/// query is unsupported).
pub fn blocks(min_free: &DiskThreshold, space: Option<Space>) -> bool {
    space.is_some_and(|s| min_free.violated(s.free, s.total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_decision() {
        let s = Some(Space {
            free: 100,
            total: 1000,
        });
        assert!(blocks(&DiskThreshold::Absolute(101), s));
        assert!(!blocks(&DiskThreshold::Absolute(100), s));
        assert!(blocks(&DiskThreshold::Percent(11.0), s));
        assert!(!blocks(&DiskThreshold::Percent(10.0), s));
        assert!(blocks(&DiskThreshold::Percent(100.0), s));
        assert!(!blocks(&DiskThreshold::Absolute(u64::MAX), None));
    }

    #[test]
    fn queries_existing_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let s = space(dir.path()).unwrap();
        assert!(s.total > 0 && s.free <= s.total);
        let deep = dir.path().join("a/b/c");
        assert_eq!(space(&deep).map(|s| s.total), Some(s.total));
    }
}
