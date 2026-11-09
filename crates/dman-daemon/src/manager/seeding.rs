//! Seeding limits enforced by dman itself (D12).
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
//! Whichever comes first ends seeding: the download is removed from aria2, the
//! job becomes `Complete` and `JobFinished { SeedCompleted }` is announced once.
//! aria2's own limits stay set but are never reached before dman's.

use dman_aria2::Gid;
use dman_proto::JobState;
use dman_proto::method::Level;
use dman_store::{Desired, JobRecord, LiveFigures};

use super::util::{engine_err, now, wait_stopped};
use super::{Manager, finish};

/// aria2's default `seed-ratio`.
pub const DEFAULT_SEED_RATIO: f64 = 1.0;

/// Why a seeding job must stop now; `None` = keep seeding.
pub(crate) fn stop_reason(rec: &JobRecord, now: i64) -> Option<String> {
    if rec.state != JobState::Seeding {
        return None;
    }
    let limit = rec.options.seed_ratio.unwrap_or(DEFAULT_SEED_RATIO);
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
    if let (Some(minutes), Some(start)) = (rec.options.seed_time, rec.completed_at) {
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
        let due: Vec<_> = self
            .jobs
            .values()
            .filter(|e| e.aria2.is_some())
            .filter_map(|e| {
                let why = stop_reason(&e.rec, t)?;
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
            e.rec.state = JobState::Complete;
            e.rec.desired = Desired::Stopped;
            e.live = LiveFigures::default();
            e.aria2 = None;
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
        assert_eq!(stop_reason(&r, 2_000), None);
        r.uploaded_session = 500; // 2.0 in total, although the session alone is 0.5
        assert!(stop_reason(&r, 2_000).unwrap().contains("ratio"));
    }

    #[test]
    fn default_ratio_and_zero_means_unlimited() {
        let mut r = rec(None, None);
        r.uploaded_base = 999;
        assert_eq!(stop_reason(&r, 2_000), None);
        r.uploaded_base = 1000;
        assert!(stop_reason(&r, 2_000).is_some());
        let mut r = rec(Some(0.0), None);
        r.uploaded_base = 10_000;
        assert_eq!(stop_reason(&r, 2_000), None);
    }

    #[test]
    fn seed_time_counts_from_the_stored_start() {
        let r = rec(Some(0.0), Some(10));
        assert_eq!(stop_reason(&r, 1_000 + 599), None);
        assert!(stop_reason(&r, 1_000 + 600).unwrap().contains("time"));
        // Only seeding jobs are touched.
        let mut r = rec(Some(0.0), Some(0));
        r.state = JobState::Complete;
        assert_eq!(stop_reason(&r, 9_999_999), None);
    }
}
