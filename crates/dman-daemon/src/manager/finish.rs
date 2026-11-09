//! Detection of `JobFinished` notifications: exactly one per transition.
//!
//! A `notified` list in `JobRecord.extra` remembers which outcomes were
//! already announced, so re-adding a job after a daemon restart (which walks a
//! seeding job through Waiting -> Seeding again) does not announce it twice.

use dman_proto::JobState;
use dman_proto::method::{Event, Outcome};
use dman_store::JobRecord;
use serde_json::Value;

use super::Manager;

const KEY: &str = "notified";

fn marked(rec: &JobRecord, what: &str) -> bool {
    rec.extra
        .get(KEY)
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(what)))
}

fn mark(rec: &mut JobRecord, what: &str) {
    if marked(rec, what) {
        return;
    }
    let v = rec
        .extra
        .entry(KEY.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(a) = v {
        a.push(Value::String(what.to_owned()));
    }
}

fn unmark(rec: &mut JobRecord, what: &str) {
    if let Some(Value::Array(a)) = rec.extra.get_mut(KEY) {
        a.retain(|v| v.as_str() != Some(what));
    }
}

/// Records that a seeding job was already announced as completed.
pub(crate) fn mark_completed(rec: &mut JobRecord) {
    mark(rec, "completed");
}

/// A retry of a failed job may fail (and be announced) again.
pub(crate) fn clear_failed(rec: &mut JobRecord) {
    unmark(rec, "failed");
}

/// Decides what to announce for the transition `prev -> new` of `rec`
/// (already updated) and records it. `None` = nothing to announce.
pub(crate) fn detect(rec: &mut JobRecord, prev: JobState, new: JobState) -> Option<Outcome> {
    if prev == new {
        return None;
    }
    if new != JobState::Error {
        clear_failed(rec);
    }
    match new {
        JobState::Seeding if !marked(rec, "completed") => {
            mark(rec, "completed");
            Some(Outcome::Completed)
        }
        JobState::Complete => {
            if prev == JobState::Seeding || marked(rec, "completed") {
                if marked(rec, "seed_completed") {
                    return None;
                }
                mark(rec, "completed");
                mark(rec, "seed_completed");
                Some(Outcome::SeedCompleted)
            } else {
                mark(rec, "completed");
                Some(Outcome::Completed)
            }
        }
        JobState::Error if !marked(rec, "failed") => {
            mark(rec, "failed");
            Some(Outcome::Failed {
                error: rec.error.clone().unwrap_or_else(|| "unknown error".into()),
            })
        }
        _ => None,
    }
}

impl Manager {
    /// Emits `JobFinished` for every outcome collected since the last call.
    pub(crate) fn emit_finished(&mut self) {
        let mut out = Vec::new();
        for e in self.jobs.values_mut() {
            for outcome in e.finished.drain(..) {
                let path = std::path::Path::new(&e.rec.dir)
                    .join(&e.rec.name)
                    .to_string_lossy()
                    .into_owned();
                out.push((e.rec.id, e.rec.name.clone(), path, outcome));
            }
        }
        out.sort_by_key(|o| o.0);
        for (id, name, path, outcome) in out {
            self.emit(Event::JobFinished {
                id,
                name,
                path,
                outcome,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::JobState as S;
    use dman_proto::{Engine, JobId, JobOptions, SourceKind};
    use dman_store::Desired;

    fn rec() -> JobRecord {
        JobRecord {
            id: JobId(1),
            engine: Engine::Aria2,
            kind: SourceKind::Torrent,
            name: "n".into(),
            dir: "/d".into(),
            source: String::new(),
            options: JobOptions::default(),
            desired: Desired::Queued,
            state: S::Active,
            gid: None,
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: None,
            queue_key: 0,
            error: Some("boom".into()),
            has_blob: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn plain_completion_once() {
        let mut r = rec();
        assert_eq!(
            detect(&mut r, S::Active, S::Complete),
            Some(Outcome::Completed)
        );
        assert_eq!(detect(&mut r, S::Complete, S::Complete), None);
    }

    #[test]
    fn seeding_flow_and_no_duplicates_after_readd() {
        let mut r = rec();
        assert_eq!(
            detect(&mut r, S::Active, S::Seeding),
            Some(Outcome::Completed)
        );
        // Restart: re-add walks Seeding -> Waiting -> Active? -> Seeding again.
        assert_eq!(detect(&mut r, S::Seeding, S::Waiting), None);
        assert_eq!(detect(&mut r, S::Waiting, S::Seeding), None);
        assert_eq!(
            detect(&mut r, S::Seeding, S::Complete),
            Some(Outcome::SeedCompleted)
        );
        assert_eq!(detect(&mut r, S::Seeding, S::Complete), None);
    }

    #[test]
    fn failure_once_and_again_after_retry() {
        let mut r = rec();
        assert_eq!(
            detect(&mut r, S::Active, S::Error),
            Some(Outcome::Failed {
                error: "boom".into()
            })
        );
        assert_eq!(detect(&mut r, S::Waiting, S::Error), None);
        clear_failed(&mut r);
        assert!(detect(&mut r, S::Active, S::Error).is_some());
    }

    #[test]
    fn legacy_seeding_marked_on_readd_stays_quiet() {
        let mut r = rec();
        mark_completed(&mut r);
        assert_eq!(detect(&mut r, S::Waiting, S::Seeding), None);
    }
}
