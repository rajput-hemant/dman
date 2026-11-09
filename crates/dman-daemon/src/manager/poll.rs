//! Tick processing: polling aria2 and applying its statuses to the job state.

use std::collections::{HashMap, HashSet};

use dman_aria2::{Aria2Client, DownloadStatus, Gid, Status};
use dman_proto::method::Event;
use dman_proto::{JobId, JobState, SourceKind};
use dman_store::{Desired, LiveFigures};
use serde_json::{Value, json};

use crate::tick::{self, Counters};

use super::util::{file_paths, gid_of, now};
use super::{Entry, Manager};

/// A non-finished job whose gid is absent from aria2 for this many ticks is
/// re-added from its stored source.
const MISSING_TICKS: u32 = 3;
/// At most this many detail (`files`/`bittorrent`) lookups per tick.
const MAX_DETAIL_CALLS: usize = 32;
const WAITING_LIMIT: u32 = 10_000;
const STOPPED_LIMIT: u32 = 1_000;

impl Manager {
    // ----- tick --------------------------------------------------------------

    /// One poll: refresh every job from aria2, run the scheduler, publish
    /// changes and statistics, persist.
    pub(crate) async fn tick(&mut self) {
        self.evaluate_schedule().await;
        if let Some(client) = self.client.clone() {
            match self.poll(&client).await {
                Ok(()) => self.poll_failing = false,
                Err(e) => {
                    if !self.poll_failing {
                        tracing::warn!("polling aria2 failed: {e}");
                    }
                    self.poll_failing = true;
                }
            }
            self.schedule(&client).await;
        }
        self.emit_changes();
        self.emit(Event::Stats {
            stats: self.stats(),
        });
        self.flush(false).await;
    }

    /// Fetches all aria2 lists (one multicall) and applies them.
    pub(super) async fn poll(&mut self, client: &Aria2Client) -> dman_aria2::Result<()> {
        let detail_gids: Vec<(JobId, String)> = self
            .jobs
            .values()
            .filter(|e| e.need_details)
            .filter_map(|e| e.rec.gid.clone().map(|g| (e.rec.id, g)))
            .take(MAX_DETAIL_CALLS)
            .collect();
        let mut calls = vec![
            (
                "aria2.tellActive".to_owned(),
                vec![json!(tick::STATUS_KEYS)],
            ),
            (
                "aria2.tellWaiting".to_owned(),
                vec![json!(0), json!(WAITING_LIMIT), json!(tick::STATUS_KEYS)],
            ),
            (
                "aria2.tellStopped".to_owned(),
                vec![json!(0), json!(STOPPED_LIMIT), json!(tick::STATUS_KEYS)],
            ),
            ("aria2.getGlobalStat".to_owned(), vec![]),
        ];
        for (_, gid) in &detail_gids {
            calls.push((
                "aria2.tellStatus".to_owned(),
                vec![json!(gid), json!(tick::DETAIL_KEYS)],
            ));
        }
        let results = client.multicall(calls).await?;
        let mut it = results.into_iter();
        let mut statuses: Vec<Status> = Vec::new();
        for _ in 0..3 {
            match it.next() {
                Some(Ok(v)) => statuses.extend(serde_json::from_value::<Vec<Status>>(v)?),
                Some(Err(e)) => return Err(e.into()),
                None => return Err(dman_aria2::Error::Protocol("short multicall".into())),
            }
        }
        if let Some(Ok(v)) = it.next()
            && let Ok(gs) = serde_json::from_value::<dman_aria2::GlobalStat>(v)
        {
            self.global_speed = (gs.download_speed.unwrap_or(0), gs.upload_speed.unwrap_or(0));
        }
        let details: Vec<(JobId, String, Status)> = detail_gids
            .into_iter()
            .zip(it)
            .filter_map(|((id, gid), r)| {
                let st = serde_json::from_value::<Status>(r.ok()?).ok()?;
                Some((id, gid, st))
            })
            .collect();
        self.apply(client, statuses).await;
        self.apply_details(details);
        Ok(())
    }

    /// Applies one full set of aria2 statuses.
    pub(super) async fn apply(&mut self, client: &Aria2Client, statuses: Vec<Status>) {
        let by_gid: HashMap<String, &Status> = statuses
            .iter()
            .filter_map(|s| gid_of(s).map(|g| (g, s)))
            .collect();

        // 1. Follow-ups (magnet metadata -> torrent, .torrent over HTTP): the
        //    job moves to the new gid and keeps its id; the pseudo item is dropped.
        let mut cleanup: Vec<String> = Vec::new();
        for st in &statuses {
            let (Some(gid), Some(parent)) = (gid_of(st), st.following.as_ref()) else {
                continue;
            };
            if self.gids.contains_key(&gid) {
                continue;
            }
            let p = parent.as_str().to_owned();
            let Some(&id) = self.gids.get(&p) else {
                continue;
            };
            self.move_gid(id, &gid);
            cleanup.push(p);
        }
        for st in &statuses {
            let (Some(gid), Some(followers)) = (gid_of(st), st.followed_by.as_ref()) else {
                continue;
            };
            let Some(first) = followers.first() else {
                continue;
            };
            if let Some(&id) = self.gids.get(&gid)
                && !self.gids.contains_key(first.as_str())
            {
                self.move_gid(id, first.as_str());
                cleanup.push(gid);
            }
        }
        for old in &cleanup {
            self.gids.remove(old);
            let done = by_gid.get(old).is_some_and(|s| {
                matches!(
                    s.status,
                    Some(DownloadStatus::Complete | DownloadStatus::Error)
                )
            });
            if done {
                let _ = client.remove_download_result(&Gid::new(old.clone())).await;
            }
        }

        // 2. Update known jobs.
        let now = now();
        let mut seen: HashSet<JobId> = HashSet::new();
        let mut adopt: Vec<String> = Vec::new();
        let mut down = 0u64;
        let mut up = 0u64;
        for st in &statuses {
            let Some(gid) = gid_of(st) else { continue };
            if cleanup.contains(&gid) {
                continue;
            }
            let Some(&id) = self.gids.get(&gid) else {
                let following_known = st
                    .following
                    .as_ref()
                    .is_some_and(|f| self.gids.contains_key(f.as_str()));
                let running = matches!(
                    st.status,
                    Some(DownloadStatus::Active | DownloadStatus::Waiting | DownloadStatus::Paused)
                );
                if running
                    && !following_known
                    && st.belongs_to.is_none()
                    && !self.previews.is_preview_gid(&gid)
                {
                    adopt.push(gid);
                }
                continue;
            };
            seen.insert(id);
            let Some(e) = self.jobs.get_mut(&id) else {
                continue;
            };
            if e.rec.gid.as_deref() != Some(gid.as_str()) {
                continue;
            }
            let (d, u, transition) = update_entry(e, st, &gid, now);
            down += d;
            up += u;
            if transition || e.dirty {
                e.dirty = true;
            }
            if transition {
                self.urgent = true;
            }
        }
        self.pending_transfer.0 = self.pending_transfer.0.saturating_add(down);
        self.pending_transfer.1 = self.pending_transfer.1.saturating_add(up);

        // 3. Jobs that vanished from aria2.
        let mut readd: Vec<JobId> = Vec::new();
        for e in self.jobs.values_mut() {
            if seen.contains(&e.rec.id) {
                e.missing = 0;
                continue;
            }
            if e.rec.state.is_stopped() || e.rec.desired == Desired::Stopped {
                e.live = LiveFigures::default();
                e.aria2 = None;
                continue;
            }
            e.aria2 = None;
            e.live = LiveFigures::default();
            e.missing += 1;
            if e.missing >= MISSING_TICKS {
                e.missing = 0;
                readd.push(e.rec.id);
            }
        }
        for id in readd {
            tracing::info!(%id, "job missing from aria2; re-adding from its stored source");
            self.readd_or_fail(client, id).await;
        }
        for gid in adopt {
            self.adopt(client, &gid).await;
        }
    }

    pub(super) fn apply_details(&mut self, details: Vec<(JobId, String, Status)>) {
        for (id, gid, st) in details {
            let Some(e) = self.jobs.get_mut(&id) else {
                continue;
            };
            if e.rec.gid.as_deref() != Some(gid.as_str()) {
                continue;
            }
            let mut changed = false;
            if let Some(name) = tick::name_from_details(&st)
                && name != e.rec.name
            {
                e.rec.name = name;
                changed = true;
            }
            if let Some(files) = &st.files {
                let paths = file_paths(files);
                if !paths.is_empty() {
                    let v = json!(paths);
                    if e.rec.extra.get("files") != Some(&v) {
                        e.rec.extra.insert("files".into(), v);
                        changed = true;
                    }
                }
            }
            if tick::details_final(e.rec.kind, &st) || e.rec.state.is_stopped() {
                // Refresh once more on the next state transition (final paths).
                e.need_details = false;
                if e.rec.extra.get("details_final") != Some(&Value::Bool(true)) {
                    e.rec
                        .extra
                        .insert("details_final".into(), Value::Bool(true));
                    changed = true;
                }
            }
            if changed {
                e.dirty = true;
            }
        }
    }

    /// Points job `id` at `new_gid` (follow-up or re-add).
    pub(crate) fn move_gid(&mut self, id: JobId, new_gid: &str) {
        let Some(e) = self.jobs.get_mut(&id) else {
            return;
        };
        if let Some(old) = e.rec.gid.take() {
            self.gids.remove(&old);
        }
        tracing::debug!(%id, gid = new_gid, "job now tracked under a new gid");
        tick::fold_upload(&mut e.rec);
        e.rec.gid = Some(new_gid.to_owned());
        if e.rec.kind == SourceKind::Http {
            // A .torrent fetched over HTTP turned into a torrent download.
            e.rec.kind = SourceKind::Torrent;
        }
        e.counters = Some(Counters::fresh(new_gid));
        e.need_details = true;
        e.dirty = true;
        self.gids.insert(new_gid.to_owned(), id);
        self.urgent = true;
    }
}

/// Applies one aria2 status to an entry. Returns `(downloaded, uploaded,
/// state_transition)`.
pub(super) fn update_entry(e: &mut Entry, st: &Status, gid: &str, now: i64) -> (u64, u64, bool) {
    let rec = &mut e.rec;
    let completed = st.completed_length.unwrap_or(rec.completed_bytes);
    let uploaded = st.upload_length.unwrap_or(rec.uploaded_session);
    let (d, u) = tick::delta(e.counters.as_ref(), gid, completed, uploaded);
    e.counters = Some(Counters {
        gid: gid.to_owned(),
        completed,
        uploaded,
    });
    let before = (rec.total_bytes, rec.completed_bytes, rec.uploaded_total());
    if let Some(t) = st.total_length {
        rec.total_bytes = t;
    }
    rec.completed_bytes = completed;
    tick::apply_upload(rec, uploaded);
    if let Some(h) = &st.info_hash
        && rec.info_hash.as_deref() != Some(h.as_str())
    {
        rec.info_hash = Some(h.clone());
        e.dirty = true;
    }
    if before != (rec.total_bytes, rec.completed_bytes, rec.uploaded_total()) {
        e.dirty = true;
    }
    let torrent = matches!(rec.kind, SourceKind::Magnet | SourceKind::Torrent);
    e.live = LiveFigures {
        download_speed: st.download_speed.unwrap_or(0),
        upload_speed: st.upload_speed.unwrap_or(0),
        connections: u32::try_from(st.connections.unwrap_or(0)).unwrap_or(u32::MAX),
        seeders: if torrent {
            st.num_seeders.map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        } else {
            None
        },
    };
    e.aria2 = st.status;
    let mut transition = false;
    if let Some(status) = st.status
        && let Some(state) = tick::map_state(status, st.seeder.unwrap_or(false), rec.desired)
        && state != rec.state
    {
        // A job dman removed stays Removed even if aria2 reports otherwise briefly.
        if !(rec.state == JobState::Removed && rec.desired == Desired::Stopped) {
            transition = true;
            rec.state = state;
            match state {
                JobState::Complete => {
                    rec.completed_at.get_or_insert(now);
                    rec.desired = Desired::Stopped;
                    rec.error = None;
                }
                JobState::Error => {
                    rec.desired = Desired::Stopped;
                    rec.error = Some(
                        st.error_message
                            .clone()
                            .filter(|m| !m.is_empty())
                            .unwrap_or_else(|| {
                                format!("aria2 error {}", st.error_code.unwrap_or(0))
                            }),
                    );
                }
                JobState::Removed => rec.desired = Desired::Stopped,
                JobState::Seeding => {
                    rec.completed_at.get_or_insert(now);
                    rec.error = None;
                }
                JobState::Active => rec.error = None,
                JobState::Waiting | JobState::Paused => {}
            }
            if matches!(
                state,
                JobState::Complete | JobState::Seeding | JobState::Error
            ) {
                // Fetch the final file list/name once more.
                e.need_details = true;
            }
        }
    }
    if transition {
        e.dirty = true;
    }
    if !e.running_in_engine() {
        e.live.download_speed = 0;
        e.live.upload_speed = 0;
    }
    (d, u, transition)
}
