//! Persistence cadence and client views: flushing dirty records, job views,
//! statistics and change events.

use std::collections::HashMap;
use std::time::Duration;

use dman_proto::method::Event;
use dman_proto::{GlobalStats, Job, JobId, JobState};
use dman_store::JobRecord;
use tokio::time::Instant;

use crate::disk;
use crate::queue;

use super::{Entry, Manager};

/// Persist dirty records at least this often (state transitions are persisted
/// immediately).
const FLUSH_EVERY: Duration = Duration::from_secs(5);

impl Manager {
    // ----- persistence -------------------------------------------------------

    /// Writes dirty records and transfer counters: always when `force` or after
    /// a state transition, otherwise at most every [`FLUSH_EVERY`].
    pub(crate) async fn flush(&mut self, force: bool) {
        if !force && !self.urgent && self.last_flush.elapsed() < FLUSH_EVERY {
            return;
        }
        let recs: Vec<JobRecord> = self
            .jobs
            .values()
            .filter(|e| e.dirty)
            .map(|e| e.rec.clone())
            .collect();
        let (down, up) = self.pending_transfer;
        if recs.is_empty() && down == 0 && up == 0 {
            self.urgent = false;
            self.last_flush = Instant::now();
            return;
        }
        let res = self
            .db(move |s| {
                s.put_jobs(&recs)?;
                s.add_transferred(down, up)
            })
            .await;
        match res {
            Ok(()) => {
                for e in self.jobs.values_mut() {
                    e.dirty = false;
                }
                self.totals.0 = self.totals.0.saturating_add(down);
                self.totals.1 = self.totals.1.saturating_add(up);
                self.pending_transfer = (0, 0);
                self.urgent = false;
                self.last_flush = Instant::now();
            }
            Err(e) => tracing::error!("persisting job records failed: {}", e.message),
        }
    }

    pub(crate) fn mark(&mut self, id: JobId, transition: bool) {
        if let Some(e) = self.jobs.get_mut(&id) {
            e.dirty = true;
        }
        if transition {
            self.urgent = true;
        }
    }

    /// Records one speed sample (global and per running job) and drops the
    /// buffers of jobs that stopped. Called once per poll interval.
    pub(crate) fn sample_history(&mut self) {
        let running: Vec<(JobId, u64, u64)> = self
            .jobs
            .values()
            .filter(|e| matches!(e.rec.state, JobState::Active | JobState::Seeding))
            .map(|e| (e.rec.id, e.live.download_speed, e.live.upload_speed))
            .collect();
        let t = super::util::now_ms();
        let speed = (
            self.global_speed.0.saturating_add(self.video_speed()),
            self.global_speed.1,
        );
        self.history.record(t, speed, &running);
        let jobs = &self.jobs;
        self.history
            .retain_jobs(|id| jobs.get(&id).is_some_and(|e| !e.rec.state.is_stopped()));
    }

    // ----- views -------------------------------------------------------------

    pub(super) fn positions(&self) -> HashMap<JobId, u32> {
        let waiting: Vec<(JobId, u64)> = self
            .jobs
            .values()
            .filter(|e| e.rec.state == JobState::Waiting)
            .map(|e| (e.rec.id, e.rec.queue_key))
            .collect();
        queue::queue_positions(&waiting)
    }

    pub(super) fn job_view(e: &Entry, positions: &HashMap<JobId, u32>) -> Job {
        let mut j = e.rec.to_job(Some(&e.live));
        j.queue_position = positions.get(&e.rec.id).copied();
        if let Some((n, m)) = e.video_item {
            // Playlist downloads show their position in the name.
            j.name = format!("{} [{n}/{m}]", j.name);
        }
        j
    }

    pub(super) fn all_jobs(&self) -> Vec<Job> {
        let pos = self.positions();
        let mut v: Vec<Job> = self
            .jobs
            .values()
            .map(|e| Self::job_view(e, &pos))
            .collect();
        v.sort_by_key(|j| j.id);
        v
    }

    pub(super) fn stats(&self) -> GlobalStats {
        let mut s = GlobalStats {
            download_speed: self.global_speed.0.saturating_add(self.video_speed()),
            upload_speed: self.global_speed.1,
            total_downloaded: self.totals.0.saturating_add(self.pending_transfer.0),
            total_uploaded: self.totals.1.saturating_add(self.pending_transfer.1),
            disk_free: disk::space(&self.default_dir).map(|s| s.free),
            schedule: self.timetable.brief(),
            ..GlobalStats::default()
        };
        for e in self.jobs.values() {
            match e.rec.state {
                JobState::Active | JobState::Seeding => s.num_active += 1,
                JobState::Waiting | JobState::Paused => s.num_waiting += 1,
                _ => s.num_stopped += 1,
            }
        }
        s
    }

    /// Sends `JobsUpdated` with every job whose client view changed.
    pub(crate) fn emit_changes(&mut self) {
        let pos = self.positions();
        let mut changed = Vec::new();
        for e in self.jobs.values_mut() {
            let j = Self::job_view(e, &pos);
            if e.emitted.as_ref() != Some(&j) {
                e.emitted = Some(j.clone());
                changed.push(j);
            }
        }
        if !changed.is_empty() {
            changed.sort_by_key(|j| j.id);
            self.emit(Event::JobsUpdated { jobs: changed });
        }
    }
}
