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
    query(p)
}

#[cfg(unix)]
fn query(p: &Path) -> Option<Space> {
    let st = nix::sys::statvfs::statvfs(p).ok()?;
    let frsize = st.fragment_size() as u64;
    Some(Space {
        free: (st.blocks_available() as u64).saturating_mul(frsize),
        total: (st.blocks() as u64).saturating_mul(frsize),
    })
}

// The free-space query is a Win32 call; std has no portable equivalent.
#[cfg(windows)]
#[allow(unsafe_code)]
fn query(p: &Path) -> Option<Space> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call, and
    // the out-pointers point to live u64s; the last out-parameter may be null.
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    (ok != 0).then_some(Space { free, total })
}

/// Whether `space` is below `threshold` (`min_free` stops jobs, `warn_free`
/// only warns). An unknown free space is never below: neither the guard nor
/// the warning may misfire when the query is unsupported.
pub fn below(threshold: &DiskThreshold, space: Option<Space>) -> bool {
    space.is_some_and(|s| threshold.violated(s.free, s.total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_decision() {
        let s = Some(Space {
            free: 100,
            total: 1000,
        });
        assert!(below(&DiskThreshold::Absolute(101), s));
        assert!(!below(&DiskThreshold::Absolute(100), s));
        assert!(below(&DiskThreshold::Percent(11.0), s));
        assert!(!below(&DiskThreshold::Percent(10.0), s));
        assert!(below(&DiskThreshold::Percent(100.0), s));
        assert!(!below(&DiskThreshold::Absolute(u64::MAX), None));
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
