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
//! aria2's own limits stay set but are never reached before dman's.

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

/// Sets the aria2 seeding options of a torrent/magnet job (`always` = also
/// the unlimited/ratio values of a seeding job, used when switching on).
pub(crate) fn apply_policy(a: &mut Options, o: &JobOptions, cfg: &Torrents, kind: SourceKind) {
    if !matches!(kind, SourceKind::Torrent | SourceKind::Magnet) {
        return;
    }
    if seeds(o, cfg) {
        let (ratio, time) = limits(o, cfg);
        a.set("seed-ratio", ratio.to_string());
        match time {
            Some(m) => a.set("seed-time", m.to_string()),
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
    let size = if rec.completed_bytes > 0 {
        rec.completed_bytes
    } else {
        rec.total_bytes
    };
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
        );
        assert_eq!(a.get("seed-time"), Some("0"));
        let mut a = Options::new();
        apply_policy(
            &mut a,
            &opts(None, Some(2.0), None),
            &Torrents::default(),
            SourceKind::Magnet,
        );
        assert_eq!(a.get("seed-ratio"), Some("2"));
        assert_eq!(a.get("seed-time"), Some(UNLIMITED_MINUTES));
        let mut a = Options::new();
        apply_policy(
            &mut a,
            &opts(None, None, None),
            &Torrents::default(),
            SourceKind::Http,
        );
        assert!(a.is_empty());
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
