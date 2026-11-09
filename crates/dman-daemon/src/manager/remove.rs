//! Removing jobs (optionally with their files) and purging history.

use std::path::PathBuf;

use dman_aria2::{DownloadStatus, Gid};
use dman_proto::method::{BatchFailure, BatchResult, Event, RemoveParams};
use dman_proto::{ErrorCode, JobId, JobState, RpcError};
use dman_store::{Desired, LiveFigures};
use serde_json::json;

use crate::ops;

use super::Manager;
use super::util::{engine_err, file_paths, is_rpc_msg, recorded_files, unavailable, wait_stopped};

impl Manager {
    // ----- remove / purge ----------------------------------------------------

    pub(super) async fn remove(&mut self, p: RemoveParams) -> BatchResult {
        let mut res = BatchResult {
            ok: Vec::new(),
            failed: Vec::new(),
        };
        for &id in &p.ids {
            match self.remove_one(id, p.delete_files, p.force).await {
                Ok(()) => res.ok.push(id),
                Err(error) => res.failed.push(BatchFailure { id, error }),
            }
        }
        self.flush(true).await;
        res
    }

    pub(super) async fn remove_one(
        &mut self,
        id: JobId,
        delete_files: bool,
        force: bool,
    ) -> Result<(), RpcError> {
        let client = self.client.clone();
        let e = self
            .jobs
            .get(&id)
            .ok_or_else(|| RpcError::not_found(format!("job {id}")))?;
        if e.rec.engine == dman_proto::Engine::YtDlp {
            return self.remove_video(id, delete_files).await;
        }
        let gid = e.rec.gid.clone().map(Gid::new);
        let in_engine = e.aria2.is_some();
        let was_running = e.running_in_engine() || e.aria2 == Some(DownloadStatus::Paused);
        let dir = PathBuf::from(&e.rec.dir);
        let mut files = recorded_files(&e.rec);

        if let (Some(client), Some(gid)) = (&client, &gid)
            && in_engine
        {
            // Capture the file list BEFORE removal.
            if delete_files && let Ok(f) = client.get_files(gid).await {
                let live: Vec<PathBuf> = file_paths(&f).into_iter().map(PathBuf::from).collect();
                if !live.is_empty() {
                    files = live;
                }
            }
            if was_running {
                let r = if force {
                    client.force_remove(gid).await
                } else {
                    match client.remove(gid).await {
                        Ok(g) => Ok(g),
                        Err(_) => client.force_remove(gid).await,
                    }
                };
                if let Err(err) = r
                    && !is_rpc_msg(&err, "not found")
                {
                    return Err(engine_err(err));
                }
                wait_stopped(client, gid).await;
            }
            let _ = client.remove_download_result(gid).await;
        } else if delete_files && client.is_none() && gid.is_some() {
            // aria2 may still own the files; it is not reachable to stop it.
            if self
                .jobs
                .get(&id)
                .is_some_and(|e| !e.rec.state.is_stopped())
            {
                return Err(unavailable(
                    "cannot stop the download to delete its files; try again when aria2 runs",
                ));
            }
        }

        if let Some(e) = self.jobs.get_mut(&id) {
            if e.rec.state != JobState::Removed {
                tracing::info!(%id, "job removed");
            }
            e.rec.state = JobState::Removed;
            e.rec.desired = Desired::Stopped;
            e.live = LiveFigures::default();
            e.aria2 = Some(DownloadStatus::Removed);
            e.need_details = false;
        }
        self.mark(id, true);

        if !delete_files {
            return Ok(());
        }
        if files.is_empty() {
            return Err(RpcError::new(
                ErrorCode::PartialFailure,
                format!(
                    "job {id} was removed but its file list is unknown, so no files were deleted"
                ),
            ));
        }
        let report = tokio::task::spawn_blocking(move || ops::delete_job_files(&dir, &files))
            .await
            .map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))?;
        tracing::info!(
            %id,
            deleted = report.deleted.len(),
            dirs = report.removed_dirs.len(),
            "deleted job files"
        );
        if report.refused.is_empty() && report.failed.is_empty() {
            Ok(())
        } else {
            let lines: Vec<String> = report
                .refused
                .iter()
                .map(|(p, r)| format!("refused {}: {r}", p.display()))
                .chain(
                    report
                        .failed
                        .iter()
                        .map(|(p, r)| format!("failed {}: {r}", p.display())),
                )
                .collect();
            let mut err = RpcError::new(
                ErrorCode::PartialFailure,
                format!(
                    "job {id} was removed; {} of its files were deleted, {} were not",
                    report.deleted.len(),
                    lines.len()
                ),
            );
            err.data = Some(json!(lines));
            Err(err)
        }
    }

    pub(super) async fn purge(&mut self, ids: Vec<JobId>) -> BatchResult {
        let mut res = BatchResult {
            ok: Vec::new(),
            failed: Vec::new(),
        };
        let ids: Vec<JobId> = if ids.is_empty() {
            let mut v: Vec<JobId> = self
                .jobs
                .values()
                .filter(|e| e.rec.state.is_stopped())
                .map(|e| e.rec.id)
                .collect();
            v.sort();
            v
        } else {
            ids
        };
        let mut gone = Vec::new();
        for id in ids {
            let Some(e) = self.jobs.get(&id) else {
                res.failed.push(BatchFailure {
                    id,
                    error: RpcError::not_found(format!("job {id}")),
                });
                continue;
            };
            if !e.rec.state.is_stopped() {
                res.failed.push(BatchFailure {
                    id,
                    error: RpcError::refused(format!("job {id} is not finished; remove it first")),
                });
                continue;
            }
            gone.push(id);
        }
        if gone.is_empty() {
            return res;
        }
        let purge_ids = gone.clone();
        if let Err(error) = self.db(move |s| s.purge(&purge_ids)).await {
            for id in gone {
                res.failed.push(BatchFailure {
                    id,
                    error: error.clone(),
                });
            }
            return res;
        }
        let client = self.client.clone();
        for &id in &gone {
            if let Some(e) = self.jobs.remove(&id)
                && let Some(gid) = e.rec.gid
            {
                self.gids.remove(&gid);
                if let Some(c) = &client {
                    let _ = c.remove_download_result(&Gid::new(gid)).await;
                }
            }
        }
        tracing::info!(count = gone.len(), "purged jobs from history");
        self.emit(Event::JobsRemoved { ids: gone.clone() });
        res.ok = gone;
        res
    }
}
