//! Queue scheduler with disk guard: applying the decisions of
//! `queue` and `disk` to aria2.

use std::collections::HashMap;
use std::path::PathBuf;

use dman_aria2::{Aria2Client, DownloadStatus, Gid};
use dman_proto::method::Level;
use dman_proto::{Engine, JobId, JobState};
use dman_store::{Desired, LiveFigures};

use crate::disk::{self, Space};
use crate::queue;

use super::Manager;
use super::util::pause_gid;

impl Manager {
    // ----- scheduler ---------------------------------------------------------

    /// Queue scheduler with disk guard, for aria2 and video jobs
    /// alike. aria2 jobs are only touched while aria2 runs (`client`).
    pub(super) async fn schedule(&mut self, client: Option<&Aria2Client>) {
        let min_free = self.profile.min_free;
        let mut space_cache: HashMap<PathBuf, Option<Space>> = HashMap::new();
        let mut space_of = |dir: &str| -> Option<Space> {
            let p = PathBuf::from(dir);
            *space_cache
                .entry(p.clone())
                .or_insert_with(|| disk::space(&p))
        };

        // Jobs the user paused must not run (e.g. a follow-up aria2 started).
        let mut to_pause: Vec<(JobId, Option<String>)> = Vec::new();
        for e in self.jobs.values() {
            if e.rec.desired == Desired::Paused && e.running_in_engine() {
                to_pause.push((e.rec.id, None));
            }
        }
        // Disk guard for running downloads.
        for e in self.jobs.values() {
            if e.rec.desired == Desired::Queued
                && e.uses_slot()
                && disk::blocks(&min_free, space_of(&e.rec.dir))
            {
                to_pause.push((
                    e.rec.id,
                    Some(format!(
                        "paused {}: free space in {} is below {min_free}; it resumes when space is available",
                        e.rec.name, e.rec.dir
                    )),
                ));
            }
        }
        // A pause window holds everything back: no slots at all.
        let max_concurrent = if self.timetable.paused() {
            0
        } else {
            self.limits.max_concurrent
        };
        // Too many running (aria2 resumed downloads by itself): preempt.
        let running: Vec<(JobId, u64)> = self
            .jobs
            .values()
            .filter(|e| e.rec.desired == Desired::Queued && e.uses_slot())
            .filter(|e| !to_pause.iter().any(|(id, _)| *id == e.rec.id))
            .map(|e| (e.rec.id, e.rec.queue_key))
            .collect();
        for id in queue::plan_preempt(max_concurrent, &running) {
            to_pause.push((id, None));
        }
        for (id, msg) in to_pause {
            if self
                .jobs
                .get(&id)
                .is_some_and(|e| e.rec.engine == Engine::YtDlp)
            {
                let st = match self.jobs.get(&id).map(|e| e.rec.desired) {
                    Some(Desired::Queued) => JobState::Waiting,
                    _ => JobState::Paused,
                };
                self.stop_video(id, st);
                if let Some(text) = msg {
                    if let Some(e) = self.jobs.get_mut(&id) {
                        e.disk_blocked = true;
                    }
                    self.message(Level::Warn, text);
                }
                continue;
            }
            let Some(client) = client else { continue };
            let Some(gid) = self.jobs.get(&id).and_then(|e| e.rec.gid.clone()) else {
                continue;
            };
            match pause_gid(client, &Gid::new(gid)).await {
                Ok(()) => {
                    if let Some(e) = self.jobs.get_mut(&id) {
                        e.aria2 = Some(DownloadStatus::Paused);
                        e.live = LiveFigures::default();
                        let st = if e.rec.desired == Desired::Queued {
                            JobState::Waiting
                        } else {
                            JobState::Paused
                        };
                        if e.rec.state != st {
                            e.rec.state = st;
                            e.dirty = true;
                            self.urgent = true;
                        }
                        if msg.is_some() {
                            e.disk_blocked = true;
                        }
                    }
                    if let Some(text) = msg {
                        self.message(Level::Warn, text);
                    }
                }
                Err(e) => tracing::warn!(%id, "cannot pause: {e}"),
            }
        }

        // Start queued jobs.
        let running = self.jobs.values().filter(|e| e.uses_slot()).count();
        let running = u32::try_from(running).unwrap_or(u32::MAX);
        let aria2_up = client.is_some();
        let ytdlp_up = self.tools.ytdlp.is_some();
        let mut candidates: Vec<(u64, JobId, String)> = self
            .jobs
            .values()
            .filter(|e| {
                e.rec.desired == Desired::Queued
                    && !e.rec.state.is_stopped()
                    && match e.rec.engine {
                        Engine::Aria2 => {
                            aria2_up
                                && e.aria2 == Some(DownloadStatus::Paused)
                                && e.rec.gid.is_some()
                        }
                        // Not while a stopped process is still exiting.
                        Engine::YtDlp => ytdlp_up && e.video.is_none(),
                    }
            })
            .map(|e| (e.rec.queue_key, e.rec.id, e.rec.dir.clone()))
            .collect();
        candidates.sort();
        let ids: Vec<JobId> = candidates.iter().map(|c| c.1).collect();
        let dirs: HashMap<JobId, String> = candidates.into_iter().map(|c| (c.1, c.2)).collect();
        let plan = queue::plan_starts(max_concurrent, running, &ids, |id| {
            dirs.get(&id)
                .is_none_or(|d| !disk::blocks(&min_free, space_of(d)))
        });
        for id in plan.blocked {
            let Some(e) = self.jobs.get_mut(&id) else {
                continue;
            };
            if !e.disk_blocked {
                e.disk_blocked = true;
                let text = format!(
                    "not starting {}: free space in {} is below {min_free}",
                    e.rec.name, e.rec.dir
                );
                self.message(Level::Warn, text);
            }
        }
        for id in plan.start {
            if self
                .jobs
                .get(&id)
                .is_some_and(|e| e.rec.engine == Engine::YtDlp)
            {
                self.start_video(id);
                continue;
            }
            let Some(client) = client else { continue };
            let Some(gid) = self.jobs.get(&id).and_then(|e| e.rec.gid.clone()) else {
                continue;
            };
            match client.unpause(&Gid::new(gid)).await {
                Ok(_) => {
                    if let Some(e) = self.jobs.get_mut(&id) {
                        tracing::debug!(%id, "starting job");
                        e.aria2 = Some(DownloadStatus::Waiting);
                        e.disk_blocked = false;
                    }
                }
                Err(e) => tracing::warn!(%id, "cannot start: {e}"),
            }
        }
    }
}
