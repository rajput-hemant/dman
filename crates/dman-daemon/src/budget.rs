//! Global download budget shared by aria2 and yt-dlp.
//!
//! aria2 enforces `max-overall-download-limit` itself, but yt-dlp processes are
//! separate and only know their own `--limit-rate`. With a global limit `L > 0`
//! the budget is split into equal *shares*: every running video job is one
//! share and aria2 as a whole is one share while it has running downloads.
//!
//! * a video job's cap is `min(per-job limit, L / shares)`;
//! * aria2's overall limit is what the video jobs leave over:
//!   `L - sum(video caps)` (at least [`MIN_ARIA2`], because 0 means unlimited
//!   to aria2). Video jobs below their share (small per-job limits) therefore
//!   hand their slack to aria2, and with no video job aria2 gets all of `L`.
//!
//! `L = 0` means unlimited: video jobs only get their per-job limit and aria2
//! is not limited by us either.
//!
//! aria2's remainder is computed from the *ideal* caps (aria2 always keeps its
//! share while video jobs run), so a new aria2 download is never starved; the
//! video jobs may overshoot until they are respawned.
//!
//! `--limit-rate` cannot change on a running yt-dlp, so a running job is only
//! respawned (stop + `--continue`) when its cap would change by more than 25 %
//! and at most once per 30 s ([`should_respawn`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Smallest limit handed to aria2 (bytes/s): 0 would mean "unlimited".
pub const MIN_ARIA2: u64 = 1024;
/// A running video job is respawned at most this often (and only when the cap
/// changed by more than [`RESPAWN_RATIO`]).
pub const RESPAWN_MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Relative change of the cap (25 %) that justifies a respawn.
pub const RESPAWN_RATIO: f64 = 0.25;

/// Override of [`RESPAWN_MIN_INTERVAL`] in milliseconds (0 = none); read when
/// a daemon starts. A test hook, not a setting.
#[doc(hidden)]
pub static RESPAWN_OVERRIDE_MS: AtomicU64 = AtomicU64::new(0);

/// The respawn interval in effect.
pub fn respawn_interval() -> Duration {
    match RESPAWN_OVERRIDE_MS.load(Ordering::Relaxed) {
        0 => RESPAWN_MIN_INTERVAL,
        ms => Duration::from_millis(ms),
    }
}

/// Caps of the running video jobs (same order as `per_job`: each entry is the
/// job's own limit, `None`/0 = none) under global limit `global`.
/// `aria2_active`: aria2 has running downloads (counts as one share).
pub fn video_caps(global: u64, per_job: &[Option<u64>], aria2_active: bool) -> Vec<Option<u64>> {
    let own = |l: &Option<u64>| l.filter(|l| *l > 0);
    if global == 0 {
        return per_job.iter().map(own).collect();
    }
    let shares = per_job.len() as u64 + u64::from(aria2_active);
    let share = (global / shares.max(1)).max(1);
    per_job
        .iter()
        .map(|l| Some(own(l).map_or(share, |l| l.min(share))))
        .collect()
}

/// aria2's `max-overall-download-limit` given the global limit and the caps
/// video jobs are running with (0 = unlimited).
pub fn aria2_limit(global: u64, caps: &[Option<u64>]) -> u64 {
    if global == 0 {
        return 0;
    }
    let used: u64 = caps.iter().map(|c| c.unwrap_or(0)).sum();
    global.saturating_sub(used).max(MIN_ARIA2).min(global)
}

/// Whether a running job with `current` cap should be respawned for `wanted`
/// `since_spawn` after its last spawn (`min_interval` = [`respawn_interval`]).
pub fn should_respawn(
    current: Option<u64>,
    wanted: Option<u64>,
    since_spawn: Duration,
    min_interval: Duration,
) -> bool {
    if since_spawn < min_interval {
        return false;
    }
    match (current, wanted) {
        (None, None) => false,
        (Some(_), None) | (None, Some(_)) => true,
        (Some(a), Some(b)) => (a as f64 - b as f64).abs() > a as f64 * RESPAWN_RATIO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_uses_per_job_limits_only() {
        let caps = video_caps(0, &[None, Some(0), Some(500)], true);
        assert_eq!(caps, vec![None, None, Some(500)]);
        assert_eq!(aria2_limit(0, &caps), 0);
    }

    #[test]
    fn equal_shares_with_and_without_aria2() {
        assert_eq!(video_caps(900, &[None], false), vec![Some(900)]);
        assert_eq!(video_caps(900, &[None], true), vec![Some(450)]);
        assert_eq!(
            video_caps(900, &[None, None], true),
            vec![Some(300), Some(300)]
        );
        assert_eq!(video_caps(900, &[], true), Vec::<Option<u64>>::new());
    }

    #[test]
    fn per_job_limit_below_share_wins_and_slack_goes_to_aria2() {
        let caps = video_caps(1_000_000, &[Some(100_000), None], true);
        assert_eq!(caps, vec![Some(100_000), Some(333_333)]);
        assert_eq!(aria2_limit(1_000_000, &caps), 566_667);
        // Per-job limit above the share is capped by the share.
        assert_eq!(
            video_caps(1_000_000, &[Some(9_999_999)], true),
            vec![Some(500_000)]
        );
    }

    #[test]
    fn aria2_gets_everything_without_video_and_never_zero() {
        assert_eq!(aria2_limit(5000, &[]), 5000);
        assert_eq!(aria2_limit(2000, &[Some(2000)]), MIN_ARIA2);
        assert_eq!(aria2_limit(500, &[Some(500)]), 500.min(MIN_ARIA2));
    }

    #[test]
    fn tiny_budget_still_positive() {
        let caps = video_caps(2, &[None, None, None], true);
        assert!(caps.iter().all(|c| *c == Some(1)));
    }

    #[test]
    fn total_stays_within_global() {
        for n in 0..6usize {
            for aria in [false, true] {
                let caps = video_caps(10_000, &vec![None; n], aria);
                let total: u64 = caps.iter().map(|c| c.unwrap()).sum();
                assert!(total <= 10_000);
                if aria || n == 0 {
                    assert!(total + aria2_limit(10_000, &caps) <= 10_000 + MIN_ARIA2);
                }
            }
        }
    }

    #[test]
    fn respawn_needs_time_and_a_big_change() {
        let m = RESPAWN_MIN_INTERVAL;
        let early = Duration::from_secs(5);
        let r = |a, b, t| should_respawn(a, b, t, m);
        assert!(!r(Some(1000), Some(100), early));
        assert!(!r(Some(1000), Some(1200), m));
        assert!(!r(Some(1000), Some(800), m));
        assert!(r(Some(1000), Some(700), m));
        assert!(r(Some(1000), Some(1300), m));
        assert!(r(None, Some(100), m));
        assert!(r(Some(100), None, m));
        assert!(!r(None, None, m));
    }
}
