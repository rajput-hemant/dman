//! Store ↔ aria2 reconciliation (on startup and after every engine restart).
//!
//! The store is authoritative. Records are matched to aria2 downloads by gid;
//! unfinished jobs aria2 does not know (it forgets seeding items, and a crash
//! can lose anything not yet in its session file) are re-added **paused** from
//! their stored source and options; downloads aria2 has but the store does not
//! (added through another tool) are adopted as new jobs.

use std::collections::HashMap;

use dman_aria2::{Aria2Client, DownloadStatus, Gid, Status, job_options_to_aria2};
use dman_proto::method::Level;
use dman_proto::{Engine, JobId, JobState, SourceKind};
use dman_store::{Desired, JobRecord};
use serde_json::{Value, json};

use crate::manager::{Entry, Manager, file_paths, now, pause_gid};
use crate::tick::{self, Counters};

const KEYS: &[&str] = &["gid", "status", "following", "belongsTo", "seeder"];

impl Manager {
    async fn all_statuses(&self, client: &Aria2Client) -> dman_aria2::Result<Vec<Status>> {
        let mut v = client.tell_active(KEYS).await?;
        v.extend(client.tell_waiting(0, 10_000, KEYS).await?);
        v.extend(client.tell_stopped(0, 1_000, KEYS).await?);
        Ok(v)
    }

    /// Full reconciliation pass.
    pub(crate) async fn reconcile(&mut self, client: &Aria2Client) {
        let statuses = match self.all_statuses(client).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("reconcile: cannot list aria2 downloads: {e}");
                return;
            }
        };
        let by_gid: HashMap<String, &Status> = statuses
            .iter()
            .filter_map(|s| s.gid.as_ref().map(|g| (g.as_str().to_owned(), s)))
            .collect();

        // Rebuild the gid index from the records.
        self.gids = self
            .jobs
            .values()
            .filter_map(|e| e.rec.gid.clone().map(|g| (g, e.rec.id)))
            .collect();

        let mut order: Vec<(u64, JobId)> = self
            .jobs
            .values()
            .map(|e| (e.rec.queue_key, e.rec.id))
            .collect();
        order.sort();
        let mut readd = Vec::new();
        for (_, id) in order {
            let Some(e) = self.jobs.get_mut(&id) else {
                continue;
            };
            e.counters = None; // first observation only sets a baseline
            let st = e.rec.gid.as_deref().and_then(|g| by_gid.get(g));
            match st {
                Some(st) => {
                    e.aria2 = st.status;
                    let running = matches!(
                        st.status,
                        Some(
                            DownloadStatus::Active
                                | DownloadStatus::Waiting
                                | DownloadStatus::Paused
                        )
                    );
                    if e.rec.desired == Desired::Stopped && running {
                        // Removed by dman while aria2 was unreachable.
                        if let Some(gid) = e.rec.gid.clone() {
                            let gid = Gid::new(gid);
                            let _ = client.force_remove(&gid).await;
                            let _ = client.remove_download_result(&gid).await;
                        }
                    }
                }
                None => {
                    if e.rec.engine == Engine::Aria2
                        && !e.rec.state.is_stopped()
                        && e.rec.desired != Desired::Stopped
                    {
                        readd.push(id);
                    }
                }
            }
        }
        let n = readd.len();
        for id in readd {
            self.readd_or_fail(client, id).await;
        }
        if n > 0 {
            tracing::info!(count = n, "re-added jobs aria2 did not know");
        }

        // Adopt downloads added through other tools.
        for st in &statuses {
            let Some(gid) = st.gid.as_ref().map(|g| g.as_str().to_owned()) else {
                continue;
            };
            if self.gids.contains_key(&gid)
                || st.belongs_to.is_some()
                || st
                    .following
                    .as_ref()
                    .is_some_and(|f| self.gids.contains_key(f.as_str()))
            {
                continue;
            }
            if matches!(
                st.status,
                Some(DownloadStatus::Active | DownloadStatus::Waiting | DownloadStatus::Paused)
            ) {
                self.adopt(client, &gid).await;
            }
        }
        self.flush(true).await;
    }

    /// Re-adds `id`; on failure marks it as failed (resume retries it).
    pub(crate) async fn readd_or_fail(&mut self, client: &Aria2Client, id: JobId) {
        if let Err(err) = self.readd(client, id).await {
            let name = self.jobs.get(&id).map(|e| e.rec.name.clone());
            if let Some(e) = self.jobs.get_mut(&id) {
                let prev = e.rec.state;
                e.rec.state = JobState::Error;
                e.rec.error = Some(format!("could not re-add to aria2: {err}"));
                if let Some(o) = crate::manager::finish::detect(&mut e.rec, prev, JobState::Error) {
                    e.finished.push(o);
                }
                e.aria2 = None;
                e.dirty = true;
            }
            self.urgent = true;
            self.message(
                Level::Error,
                format!(
                    "could not restore {} (job {id}): {err}",
                    name.unwrap_or_default()
                ),
            );
        }
    }

    /// Adds job `id` to aria2 again (paused) from its stored source and
    /// options, and points the job at the new gid.
    pub(crate) async fn readd(&mut self, client: &Aria2Client, id: JobId) -> Result<(), String> {
        let rec = self
            .jobs
            .get(&id)
            .map(|e| e.rec.clone())
            .ok_or_else(|| format!("job {id} not found"))?;
        let mut opts = job_options_to_aria2(&rec.options).map_err(|e| e.to_string())?;
        opts.set("dir", rec.dir.clone());
        opts.set("pause", "true");
        if rec.completed_at.is_some()
            && matches!(rec.kind, SourceKind::Torrent | SourceKind::Magnet)
        {
            // A torrent that finished before (it was seeding) has all its data
            // but no `.aria2` control file; without a hash check aria2 refuses
            // to touch the existing files ("control file does not exist").
            opts.set("check-integrity", "true");
        }
        tokio::fs::create_dir_all(&rec.dir)
            .await
            .map_err(|e| format!("cannot create {}: {e}", rec.dir))?;
        let blob = if rec.has_blob {
            Some(
                self.db(move |s| s.get_blob(id))
                    .await
                    .map_err(|e| e.message)?
                    .ok_or("the stored .torrent/.metalink data is missing")?,
            )
        } else {
            None
        };
        let gid: Gid = match (rec.kind, blob) {
            (SourceKind::Torrent, Some(b)) => client
                .add_torrent(&b, &[], &opts, None)
                .await
                .map_err(|e| e.to_string())?,
            (SourceKind::Metalink, Some(b)) => {
                let mut gids = client
                    .add_metalink(&b, &opts, None)
                    .await
                    .map_err(|e| e.to_string())?;
                let idx = rec
                    .extra
                    .get("metalink_index")
                    .and_then(Value::as_u64)
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or(0);
                if idx >= gids.len() {
                    return Err("the metalink no longer describes this file".into());
                }
                let keep = gids.remove(idx);
                for other in gids {
                    let _ = client.force_remove(&other).await;
                    let _ = client.remove_download_result(&other).await;
                }
                keep
            }
            (SourceKind::Torrent | SourceKind::Metalink, None) => {
                return Err("no stored .torrent/.metalink data".into());
            }
            _ => {
                let uris: Vec<String> = rec
                    .extra
                    .get("uris")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .filter(|v: &Vec<String>| !v.is_empty())
                    .unwrap_or_else(|| vec![rec.source.clone()]);
                client
                    .add_uri(&uris, &opts, None)
                    .await
                    .map_err(|e| e.to_string())?
            }
        };
        let was_seeding = rec.state == JobState::Seeding;
        self.move_gid(id, gid.as_str());
        let mut start_now = false;
        if let Some(e) = self.jobs.get_mut(&id) {
            // aria2 resumes partial data: the first reading is a baseline only.
            e.counters = None;
            if was_seeding {
                // Already announced before the restart.
                crate::manager::finish::mark_completed(&mut e.rec);
            }
            e.aria2 = Some(DownloadStatus::Paused);
            e.missing = 0;
            e.rec.state = if e.rec.desired == Desired::Queued {
                JobState::Waiting
            } else {
                JobState::Paused
            };
            // Seeding torrents do not use a download slot: restart at once.
            start_now = was_seeding && e.rec.desired == Desired::Queued;
        }
        if start_now
            && client.unpause(&gid).await.is_ok()
            && let Some(e) = self.jobs.get_mut(&id)
        {
            e.aria2 = Some(DownloadStatus::Waiting);
        }
        self.mark(id, true);
        Ok(())
    }

    /// Adopts an aria2 download unknown to the store as a new job.
    pub(crate) async fn adopt(&mut self, client: &Aria2Client, gid: &str) {
        let g = Gid::new(gid);
        let st = match client.tell_status(&g, &[]).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(gid, "cannot adopt: {e}");
                return;
            }
        };
        if st.dir.as_deref().is_some_and(|d| self.previews.owns_dir(d)) {
            // A magnet preview (e.g. left behind by a crashed daemon): not a job.
            let _ = client.force_remove(&g).await;
            return;
        }
        let Some(status) = st.status else { return };
        if status == DownloadStatus::Waiting && pause_gid(client, &g).await.is_err() {
            return;
        }
        let ids = self
            .db(|s| Ok((s.next_job_id()?, s.next_queue_key()?)))
            .await;
        let Ok((id, key)) = ids else { return };
        let first_uri = st
            .files
            .as_ref()
            .and_then(|f| f.first())
            .and_then(|f| f.uris.first())
            .map(|u| u.uri.clone());
        let (kind, source) = if st.bittorrent.is_some() || st.info_hash.is_some() {
            let hash = st.info_hash.clone().unwrap_or_default();
            (SourceKind::Magnet, format!("magnet:?xt=urn:btih:{hash}"))
        } else {
            let uri = first_uri.unwrap_or_default();
            (tick::kind_of_uri(&uri).unwrap_or(SourceKind::Http), uri)
        };
        let desired = match status {
            DownloadStatus::Paused => Desired::Paused,
            DownloadStatus::Active | DownloadStatus::Waiting => Desired::Queued,
            _ => Desired::Stopped,
        };
        let state = tick::map_state(
            if status == DownloadStatus::Waiting {
                DownloadStatus::Paused
            } else {
                status
            },
            st.seeder.unwrap_or(false),
            desired,
        )
        .unwrap_or(JobState::Waiting);
        let name = tick::name_from_details(&st).unwrap_or_else(|| {
            if source.is_empty() {
                gid.to_owned()
            } else {
                tick::name_of_uri(&source)
            }
        });
        let mut extra = serde_json::Map::new();
        if let Some(files) = &st.files {
            extra.insert("files".into(), json!(file_paths(files)));
        }
        let rec = JobRecord {
            id,
            engine: Engine::Aria2,
            kind,
            name,
            dir: st
                .dir
                .clone()
                .unwrap_or_else(|| self.default_dir.display().to_string()),
            source,
            options: Default::default(),
            desired,
            state,
            gid: Some(gid.to_owned()),
            info_hash: st.info_hash.clone(),
            total_bytes: st.total_length.unwrap_or(0),
            completed_bytes: st.completed_length.unwrap_or(0),
            uploaded_base: 0,
            uploaded_session: st.upload_length.unwrap_or(0),
            added_at: now(),
            completed_at: None,
            queue_key: key,
            error: None,
            has_blob: false,
            extra,
        };
        tracing::info!(%id, gid, "adopted a download added outside dman");
        let mut e = Entry::new(rec);
        e.aria2 = Some(if status == DownloadStatus::Waiting {
            DownloadStatus::Paused
        } else {
            status
        });
        e.counters = Some(Counters {
            gid: gid.to_owned(),
            completed: st.completed_length.unwrap_or(0),
            uploaded: st.upload_length.unwrap_or(0),
        });
        e.dirty = true;
        self.gids.insert(gid.to_owned(), id);
        self.jobs.insert(id, e);
        self.urgent = true;
    }
}
