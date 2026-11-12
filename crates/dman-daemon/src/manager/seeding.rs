//! Seeding limits enforced by dman itself.
//!
//! aria2 counts `seed-ratio` and `seed-time` per engine session: after a daemon
//! restart a re-added seeding torrent would start from zero again and seed far
//! longer than asked. dman keeps the real figures instead: the uploaded bytes
//! persisted in the record (`uploaded_base` + current session) and the stored
//! start of seeding (`completed_at`, unix seconds, set when the job first
//! entered `Seeding`). A seeding job is stopped by dman when
//!
//! * `uploaded_total / size >= seed_ratio` (`seed_ratio` option, default
//!   [`DEFAULT_SEED_RATIO`] = aria2's own default; `0` = no ratio limit), or
//! * `now - completed_at >= seed_time` minutes (`seed_time` option; `0` stops at
//!   once). The clock is wall time since seeding began, daemon downtime included.
//!
//! Seeding is off by default (`[torrents] seed = false`): see [`seeds`]. A job
//! that does not seed gets aria2's `seed-time=0`, so aria2 completes it without
//! seeding, and any such job found seeding (stored before this default, config
//! reloaded, `jobs.set_options`) is stopped at once as `Complete` with only
//! `Completed` announced. The `[torrents]` ratio/time are the defaults of jobs
//! that seed; per-job `seed_ratio` / `seed_time` win.
//!
//! Whichever comes first ends seeding: the download is removed from aria2, the
//! job becomes `Complete` and `JobFinished { SeedCompleted }` is announced once.
//!
//! aria2 gets the same limits, reduced by what earlier engine sessions already
//! used up (see [`Seeded`]). dman's check only runs once per poll, while aria2
//! checks its own limits continuously; its upload limiter averages over a
//! window of several seconds, so after an idle spell a peer can fetch a burst
//! far above `max-upload-limit` between two polls. Without the reduced limits a
//! restored seeding job would be bounded only by the poll and could overshoot
//! its ratio by a whole burst. When aria2 ends seeding itself the poll sees
//! the download complete and announces `SeedCompleted` the same way.

use dman_aria2::{Gid, Options};
use dman_config::Torrents;
use dman_proto::method::Level;
use dman_proto::{JobOptions, JobState, SourceKind};
use dman_store::{Desired, JobRecord, LiveFigures};

use super::util::{engine_err, now, wait_stopped};
use super::{Manager, finish};

/// Seed time sent to aria2 for a seeding job without a time limit (minutes);
/// dman's own checks end seeding first.
const UNLIMITED_MINUTES: &str = "5256000";

/// Whether a job seeds: an explicit `seed` wins; otherwise `[torrents] seed`,
/// or the job itself asking for a positive seed ratio or time.
pub(crate) fn seeds(o: &JobOptions, cfg: &Torrents) -> bool {
    match o.seed {
        Some(s) => s,
        None => {
            cfg.seed || o.seed_ratio.is_some_and(|r| r > 0.0) || o.seed_time.is_some_and(|t| t > 0)
        }
    }
}

/// Effective ratio limit (`0` = none) and time limit in minutes of a seeding job.
pub(crate) fn limits(o: &JobOptions, cfg: &Torrents) -> (f64, Option<u64>) {
    let ratio = o.seed_ratio.unwrap_or(cfg.seed_ratio);
    let time = o
        .seed_time
        .or((cfg.seed_time_minutes > 0).then_some(cfg.seed_time_minutes));
    (ratio, time)
}

/// Smallest ratio handed to aria2: `0` would mean "no ratio limit" there.
const MIN_ENGINE_RATIO: f64 = 0.0001;

/// What a job already used up of its seeding limits before the aria2
/// download the options are meant for; aria2 counts from zero per download.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Seeded {
    /// Bytes uploaded that aria2's counter for the download will not include.
    pub uploaded: u64,
    /// Size the ratio is measured against (`0` = unknown).
    pub size: u64,
    /// Seconds already spent seeding, when aria2's seed clock starts afresh.
    pub secs: Option<i64>,
}

impl Seeded {
    /// Usage of `rec` for a new aria2 download of it (a re-add): every byte
    /// uploaded so far, and the time since seeding first began.
    pub(crate) fn before_readd(rec: &JobRecord, now: i64) -> Self {
        Self {
            uploaded: rec.uploaded_total(),
            size: ratio_size(rec),
            secs: rec.completed_at.map(|t| now.saturating_sub(t).max(0)),
        }
    }

    /// Usage of `rec` outside its current aria2 download (option changes on
    /// a live download, whose own counters aria2 keeps).
    pub(crate) fn outside_current(rec: &JobRecord) -> Self {
        Self {
            uploaded: rec.uploaded_base,
            size: ratio_size(rec),
            secs: None,
        }
    }
}

/// Size a job's ratio is measured against.
fn ratio_size(rec: &JobRecord) -> u64 {
    if rec.completed_bytes > 0 {
        rec.completed_bytes
    } else {
        rec.total_bytes
    }
}

/// Sets the aria2 seeding options of a torrent/magnet job. A seeding job's
/// ratio and time are reduced by what `done` already used up, so aria2 stops
/// at the job's real limits rather than restarting them from zero.
pub(crate) fn apply_policy(
    a: &mut Options,
    o: &JobOptions,
    cfg: &Torrents,
    kind: SourceKind,
    done: Seeded,
) {
    if !matches!(kind, SourceKind::Torrent | SourceKind::Magnet) {
        return;
    }
    if seeds(o, cfg) {
        let (ratio, time) = limits(o, cfg);
        let ratio = if ratio > 0.0 && done.size > 0 {
            (ratio - done.uploaded as f64 / done.size as f64).max(MIN_ENGINE_RATIO)
        } else {
            ratio
        };
        a.set("seed-ratio", ratio.to_string());
        match time {
            Some(m) => {
                let left = match done.secs {
                    // Whole minutes, at least one: dman's own check ends seeding
                    // on time, and `0` would make aria2 skip seeding altogether.
                    Some(s) => {
                        let left = i64::try_from(m.saturating_mul(60))
                            .unwrap_or(i64::MAX)
                            .saturating_sub(s);
                        u64::try_from(left).unwrap_or(0).div_ceil(60).max(1)
                    }
                    None => m,
                };
                a.set("seed-time", left.to_string());
            }
            None => a.set("seed-time", UNLIMITED_MINUTES),
        }
    } else {
        a.set("seed-time", "0");
    }
}

/// Why a seeding job must stop now; `None` = keep seeding.
pub(crate) fn stop_reason(rec: &JobRecord, now: i64, cfg: &Torrents) -> Option<String> {
    if rec.state != JobState::Seeding {
        return None;
    }
    if !seeds(&rec.options, cfg) {
        return Some("seeding is off".into());
    }
    let (limit, time) = limits(&rec.options, cfg);
    let size = ratio_size(rec);
    if limit > 0.0 && size > 0 {
        let ratio = rec.uploaded_total() as f64 / size as f64;
        if ratio >= limit {
            return Some(format!("seed ratio {limit} reached ({ratio:.2})"));
        }
    }
    if let (Some(minutes), Some(start)) = (time, rec.completed_at) {
        let secs = i64::try_from(minutes.saturating_mul(60)).unwrap_or(i64::MAX);
        if now.saturating_sub(start) >= secs {
            return Some(format!("seed time of {minutes} min reached"));
        }
    }
    None
}

impl Manager {
    /// Stops every seeding job that reached its ratio or time limit.
    pub(super) async fn enforce_seeding(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let t = now();
        let torrents = self.torrents.clone();
        let due: Vec<_> = self
            .jobs
            .values()
            .filter(|e| e.aria2.is_some())
            .filter_map(|e| {
                let why = stop_reason(&e.rec, t, &torrents)?;
                Some((e.rec.id, e.rec.gid.clone()?, why))
            })
            .collect();
        for (id, gid, why) in due {
            let gid = Gid::new(gid);
            let r = match client.remove(&gid).await {
                Ok(g) => Ok(g),
                Err(_) => client.force_remove(&gid).await,
            };
            if let Err(e) = r {
                // Already gone (aria2 stopped it first) is fine; the poll sees the result.
                tracing::debug!(%id, "cannot stop seeding: {}", engine_err(e).message);
                continue;
            }
            wait_stopped(&client, &gid).await;
            let _ = client.remove_download_result(&gid).await;
            let Some(e) = self.jobs.get_mut(&id) else {
                continue;
            };
            let prev = e.rec.state;
            if !seeds(&e.rec.options, &torrents) {
                // Completed was announced when seeding began; no SeedCompleted.
                finish::mark_seed_completed(&mut e.rec);
            }
            e.rec.state = JobState::Complete;
            e.rec.desired = Desired::Stopped;
            e.live = LiveFigures::default();
            e.aria2 = None;
            e.artefacts_due = Some(super::artefacts::SETTLE_TICKS);
            e.dirty = true;
            if let Some(o) = finish::detect(&mut e.rec, prev, JobState::Complete) {
                e.finished.push(o);
            }
            let name = e.rec.name.clone();
            self.urgent = true;
            self.message(Level::Info, format!("{name}: seeding finished: {why}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::{Engine, JobId, JobOptions, SourceKind};

    fn on() -> Torrents {
        Torrents {
            seed: true,
            ..Default::default()
        }
    }

    fn opts(seed: Option<bool>, ratio: Option<f64>, time: Option<u64>) -> JobOptions {
        JobOptions {
            seed,
            seed_ratio: ratio,
            seed_time: time,
            ..Default::default()
        }
    }

    #[test]
    fn decision_table() {
        let off = Torrents::default();
        let on = on();
        for cfg in [&off, &on] {
            // Explicit switch wins over config and over ratio/time.
            assert!(seeds(&opts(Some(true), None, None), cfg));
            assert!(seeds(&opts(Some(true), Some(0.0), Some(0)), cfg));
            assert!(!seeds(&opts(Some(false), None, None), cfg));
            assert!(!seeds(&opts(Some(false), Some(2.0), Some(60)), cfg));
        }
        // No explicit switch: config decides...
        assert!(!seeds(&opts(None, None, None), &off));
        assert!(seeds(&opts(None, None, None), &on));
        // ...unless the job sets a positive ratio or time.
        assert!(seeds(&opts(None, Some(1.5), None), &off));
        assert!(seeds(&opts(None, None, Some(30)), &off));
        // Zero does not imply seeding.
        assert!(!seeds(&opts(None, Some(0.0), None), &off));
        assert!(!seeds(&opts(None, None, Some(0)), &off));
        assert!(!seeds(&opts(None, Some(0.0), Some(0)), &off));
    }

    #[test]
    fn limits_fall_back_to_config() {
        let cfg = Torrents {
            seed: true,
            seed_ratio: 3.0,
            seed_time_minutes: 45,
            ..Default::default()
        };
        assert_eq!(limits(&opts(None, None, None), &cfg), (3.0, Some(45)));
        assert_eq!(
            limits(&opts(None, Some(1.0), Some(5)), &cfg),
            (1.0, Some(5))
        );
        assert_eq!(limits(&opts(None, None, None), &on()), (1.0, None));
    }

    #[test]
    fn policy_options_for_aria2() {
        let mut a = Options::new();
        apply_policy(
            &mut a,
            &opts(None, None, None),
            &Torrents::default(),
            SourceKind::Torrent,
            Seeded::default(),
        );
        assert_eq!(a.get("seed-time"), Some("0"));
        let mut a = Options::new();
        apply_policy(
            &mut a,
            &opts(None, Some(2.0), None),
            &Torrents::default(),
            SourceKind::Magnet,
            Seeded::default(),
        );
        assert_eq!(a.get("seed-ratio"), Some("2"));
        assert_eq!(a.get("seed-time"), Some(UNLIMITED_MINUTES));
        let mut a = Options::new();
        apply_policy(
            &mut a,
            &opts(None, None, None),
            &Torrents::default(),
            SourceKind::Http,
            Seeded::default(),
        );
        assert!(a.is_empty());
    }

    #[test]
    fn aria2_gets_what_is_left_of_the_limits() {
        let mut r = rec(Some(0.8), Some(60));
        r.uploaded_base = 300;
        r.uploaded_session = 190;
        // A re-add counts from zero in aria2: everything so far is used up.
        let mut a = Options::new();
        let done = Seeded::before_readd(&r, 1_000 + 25 * 60 + 1);
        apply_policy(&mut a, &r.options, &on(), SourceKind::Torrent, done);
        let ratio: f64 = a.get("seed-ratio").unwrap().parse().unwrap();
        assert!((ratio - 0.31).abs() < 1e-9, "{ratio}");
        assert_eq!(a.get("seed-time"), Some("35"));
        // A live download keeps its own counter: only the earlier sessions count.
        let mut a = Options::new();
        let done = Seeded::outside_current(&r);
        apply_policy(&mut a, &r.options, &on(), SourceKind::Torrent, done);
        let ratio: f64 = a.get("seed-ratio").unwrap().parse().unwrap();
        assert!((ratio - 0.5).abs() < 1e-9, "{ratio}");
        assert_eq!(a.get("seed-time"), Some("60"));
        // Used up: aria2 still gets a positive ratio and time (0 means unlimited
        // ratio or no seeding at all there); dman stops the job on its next poll.
        r.uploaded_session = 900;
        let mut a = Options::new();
        let done = Seeded::before_readd(&r, 1_000 + 120 * 60);
        apply_policy(&mut a, &r.options, &on(), SourceKind::Torrent, done);
        assert_eq!(a.get("seed-ratio"), Some("0.0001"));
        assert_eq!(a.get("seed-time"), Some("1"));
        // No ratio limit stays unlimited.
        let r = rec(Some(0.0), None);
        let mut a = Options::new();
        apply_policy(
            &mut a,
            &r.options,
            &on(),
            SourceKind::Torrent,
            Seeded::before_readd(&r, 5_000),
        );
        assert_eq!(a.get("seed-ratio"), Some("0"));
        assert_eq!(a.get("seed-time"), Some(UNLIMITED_MINUTES));
    }

    #[test]
    fn jobs_that_do_not_seed_stop_at_once() {
        let r = rec(None, None);
        assert_eq!(
            stop_reason(&r, 1_001, &Torrents::default()).as_deref(),
            Some("seeding is off")
        );
        assert_eq!(stop_reason(&r, 1_001, &on()), None);
    }

    fn rec(ratio: Option<f64>, time: Option<u64>) -> JobRecord {
        JobRecord {
            id: JobId(1),
            engine: Engine::Aria2,
            kind: SourceKind::Torrent,
            name: "n".into(),
            dir: "/d".into(),
            source: String::new(),
            options: JobOptions {
                seed_ratio: ratio,
                seed_time: time,
                ..Default::default()
            },
            desired: Desired::Queued,
            state: JobState::Seeding,
            gid: Some("g".into()),
            info_hash: None,
            total_bytes: 1000,
            completed_bytes: 1000,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: Some(1_000),
            queue_key: 0,
            error: None,
            has_blob: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn ratio_counts_the_persisted_base_not_just_the_session() {
        let mut r = rec(Some(2.0), None);
        r.uploaded_base = 1500;
        r.uploaded_session = 100; // 1.6 so far
        assert_eq!(stop_reason(&r, 2_000, &on()), None);
        r.uploaded_session = 500; // 2.0 in total, although the session alone is 0.5
        assert!(stop_reason(&r, 2_000, &on()).unwrap().contains("ratio"));
    }

    #[test]
    fn default_ratio_and_zero_means_unlimited() {
        let mut r = rec(None, None);
        r.uploaded_base = 999;
        assert_eq!(stop_reason(&r, 2_000, &on()), None);
        r.uploaded_base = 1000;
        assert!(stop_reason(&r, 2_000, &on()).is_some());
        let mut r = rec(Some(0.0), None);
        r.uploaded_base = 10_000;
        assert_eq!(stop_reason(&r, 2_000, &on()), None);
    }

    #[test]
    fn seed_time_counts_from_the_stored_start() {
        let r = rec(Some(0.0), Some(10));
        assert_eq!(stop_reason(&r, 1_000 + 599, &on()), None);
        assert!(
            stop_reason(&r, 1_000 + 600, &on())
                .unwrap()
                .contains("time")
        );
        // Only seeding jobs are touched.
        let mut r = rec(Some(0.0), Some(0));
        r.state = JobState::Complete;
        assert_eq!(stop_reason(&r, 9_999_999, &on()), None);
    }
}
