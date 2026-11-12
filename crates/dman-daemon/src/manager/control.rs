//! Job and queue control: pause, resume, reorder, options and limits.

use dman_aria2::{DownloadStatus, Gid, PosHow, job_options_to_aria2, requires_pause};
use dman_proto::method::{BatchFailure, BatchResult, ReorderParams, SetOptionsParams};
use dman_proto::{Engine, ErrorCode, JobId, JobState, RpcError};
use dman_store::{Desired, LiveFigures};

use crate::queue;

use super::util::{engine_err, merge_options, pause_gid, resolve_dir, unavailable};
use super::{Entry, Manager};

impl Manager {
    // ----- pause / resume ----------------------------------------------------

    pub(super) async fn pause(&mut self, ids: &[JobId]) -> BatchResult {
        let mut res = BatchResult {
            ok: Vec::new(),
            failed: Vec::new(),
        };
        for &id in ids {
            match self.pause_one(id).await {
                Ok(()) => res.ok.push(id),
                Err(error) => res.failed.push(BatchFailure { id, error }),
            }
        }
        self.flush(true).await;
        res
    }

    pub(super) async fn pause_one(&mut self, id: JobId) -> Result<(), RpcError> {
        let client = self.client.clone();
        let e = self
            .jobs
            .get_mut(&id)
            .ok_or_else(|| RpcError::not_found(format!("job {id}")))?;
        if e.rec.state.is_stopped() {
            return Err(RpcError::refused(format!(
                "job {id} is finished and cannot be paused"
            )));
        }
        if e.rec.engine == Engine::YtDlp {
            // pause = stop yt-dlp; resume respawns it with --continue.
            e.rec.desired = Desired::Paused;
            e.dirty = true;
            self.urgent = true;
            self.stop_video(id, JobState::Paused);
            return Ok(());
        }
        e.rec.desired = Desired::Paused;
        e.dirty = true;
        self.urgent = true;
        let running = e.running_in_engine();
        let gid = e.rec.gid.clone();
        if running && let (Some(client), Some(gid)) = (client, gid) {
            pause_gid(&client, &Gid::new(gid))
                .await
                .map_err(engine_err)?;
        }
        if let Some(e) = self.jobs.get_mut(&id) {
            if running {
                e.aria2 = Some(DownloadStatus::Paused);
            }
            e.live = LiveFigures::default();
            e.rec.state = JobState::Paused;
        }
        Ok(())
    }

    pub(super) async fn resume(&mut self, ids: &[JobId]) -> BatchResult {
        let mut res = BatchResult {
            ok: Vec::new(),
            failed: Vec::new(),
        };
        for &id in ids {
            match self.resume_one(id).await {
                Ok(()) => res.ok.push(id),
                Err(error) => res.failed.push(BatchFailure { id, error }),
            }
        }
        self.flush(true).await;
        res
    }

    pub(super) async fn resume_one(&mut self, id: JobId) -> Result<(), RpcError> {
        let e = self
            .jobs
            .get_mut(&id)
            .ok_or_else(|| RpcError::not_found(format!("job {id}")))?;
        match e.rec.state {
            JobState::Complete | JobState::Removed => {
                return Err(RpcError::refused(format!(
                    "job {id} is finished and cannot be resumed"
                )));
            }
            JobState::Error if e.rec.engine == Engine::YtDlp => {
                // Retry: queue again; yt-dlp continues partial files.
                super::finish::clear_failed(&mut e.rec);
                e.rec.error = None;
                e.rec.desired = Desired::Queued;
                e.rec.state = JobState::Waiting;
                e.disk_blocked = false;
            }
            JobState::Error => {
                // Retry: re-add from the stored source (aria2 continues partial files).
                let client = self.client.clone().ok_or_else(|| {
                    unavailable(self.engine.reason.as_deref().unwrap_or("not running"))
                })?;
                let prev = (e.rec.desired, e.rec.state, e.rec.error.take());
                super::finish::clear_failed(&mut e.rec);
                super::restart::allow_again(&mut e.rec);
                e.rec.desired = Desired::Queued;
                e.rec.state = JobState::Waiting;
                if let Err(err) = self.readd(&client, id).await {
                    if let Some(e) = self.jobs.get_mut(&id) {
                        (e.rec.desired, e.rec.state, e.rec.error) = prev;
                    }
                    return Err(RpcError::new(ErrorCode::General, err));
                }
            }
            _ => {
                e.rec.desired = Desired::Queued;
                if e.rec.state == JobState::Paused {
                    e.rec.state = JobState::Waiting;
                }
                e.disk_blocked = false;
            }
        }
        self.mark(id, true);
        Ok(())
    }

    // ----- reorder / options -------------------------------------------------

    pub(super) async fn reorder(&mut self, p: ReorderParams) -> Result<(), RpcError> {
        let e = self
            .jobs
            .get(&p.id)
            .ok_or_else(|| RpcError::not_found(format!("job {}", p.id)))?;
        let queued = |e: &Entry| matches!(e.rec.state, JobState::Waiting | JobState::Paused);
        if !queued(e) {
            return Err(RpcError::refused(format!(
                "job {} is not in the queue (only waiting or paused jobs can be moved)",
                p.id
            )));
        }
        let q: Vec<(JobId, u64)> = self
            .jobs
            .values()
            .filter(|e| queued(e))
            .map(|e| (e.rec.id, e.rec.queue_key))
            .collect();
        let Some((changed, pos)) = queue::reorder(&q, p.id, p.to) else {
            return Err(RpcError::not_found(format!("job {}", p.id)));
        };
        for (id, key) in changed {
            if let Some(e) = self.jobs.get_mut(&id) {
                e.rec.queue_key = key;
                e.dirty = true;
            }
        }
        self.flush(true).await;
        // Mirror to aria2's own queue, best effort (dman's order is what counts).
        if let (Some(c), Some(gid)) = (
            self.client.clone(),
            self.jobs.get(&p.id).and_then(|e| e.rec.gid.clone()),
        ) {
            let _ = c
                .change_position(&Gid::new(gid), pos as i64, PosHow::Set)
                .await;
        }
        Ok(())
    }

    pub(super) async fn set_options(&mut self, p: SetOptionsParams) -> Result<(), RpcError> {
        let e = self
            .jobs
            .get(&p.id)
            .ok_or_else(|| RpcError::not_found(format!("job {}", p.id)))?;
        if e.rec.state.is_stopped() {
            return Err(RpcError::refused(format!("job {} is finished", p.id)));
        }
        if e.rec.engine == Engine::YtDlp && (e.video.is_some() || e.rec.state == JobState::Active) {
            return Err(RpcError::refused(format!(
                "job {} is a running video download; pause it first (yt-dlp takes options when it starts)",
                p.id
            )));
        }
        let mut delta = p.options.clone();
        super::util::check_cookie_file(&mut delta, self.config_dir.as_deref())?;
        if let Some(d) = &delta.dir {
            let resolved = resolve_dir(Some(d), &self.default_dir)?;
            delta.dir = Some(resolved.display().to_string());
        }
        let merged = merge_options(&e.rec.options, &delta);
        job_options_to_aria2(&merged).map_err(|e| RpcError::invalid(e.to_string()))?;
        let mut changes =
            job_options_to_aria2(&delta).map_err(|e| RpcError::invalid(e.to_string()))?;
        let kind = e.rec.kind;
        if super::seeding::seeds(&e.rec.options, &self.torrents)
            != super::seeding::seeds(&merged, &self.torrents)
            || delta.seed_ratio.is_some()
            || delta.seed_time.is_some()
        {
            // Switching seeding on/off (or its limits) reaches aria2 too; a seeding
            // job that no longer seeds is stopped by the next poll.
            let done = super::seeding::Seeded::outside_current(&e.rec);
            super::seeding::apply_policy(&mut changes, &merged, &self.torrents, kind, done);
        }
        let active =
            e.running_in_engine() || matches!(e.rec.state, JobState::Active | JobState::Seeding);
        if active && let Some(k) = changes.0.keys().find(|k| requires_pause(k)) {
            return Err(RpcError::refused(format!(
                "`{k}` cannot be changed while the download runs; pause the download first"
            )));
        }
        let gid = e.rec.gid.clone();
        if let Some(d) = &delta.dir {
            tokio::fs::create_dir_all(d).await.map_err(|err| {
                RpcError::new(ErrorCode::General, format!("cannot create {d}: {err}"))
            })?;
        }
        if !changes.is_empty()
            && let (Some(c), Some(gid)) = (self.client.clone(), gid)
            && self.jobs.get(&p.id).is_some_and(|e| e.aria2.is_some())
        {
            c.change_option(&Gid::new(gid), &changes)
                .await
                .map_err(engine_err)?;
        }
        if let Some(e) = self.jobs.get_mut(&p.id) {
            if let Some(d) = &delta.dir {
                e.rec.dir = d.clone();
            }
            if let Some(o) = &delta.out {
                e.rec.name = o.clone();
            }
            e.rec.options = merged;
        }
        self.mark(p.id, true);
        self.flush(true).await;
        Ok(())
    }
}
