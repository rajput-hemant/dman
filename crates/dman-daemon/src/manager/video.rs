//! Video jobs (yt-dlp engine) in the same queue as aria2 jobs (D5).
//!
//! * Start: the scheduler spawns one yt-dlp process per running job; a
//!   forwarder task folds its progress into a shared [`VideoShared`] (read on
//!   every tick) and reports the [`Outcome`] to the actor as a [`VideoMsg`].
//! * Pause / preemption / disk guard / pause window: stop the process (SIGTERM,
//!   then SIGKILL; D9). Resume = queue again; the next spawn passes
//!   `--continue`, so yt-dlp picks up its `.part` file.
//! * Remove: stop and wait for the process; with `delete_files` only the
//!   job's own outputs and their partial files are deleted (`ops` path safety).
//! * Daemon restart: running jobs are stopped on shutdown and come back as
//!   `Waiting`; the scheduler respawns them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dman_proto::method::Level;
use dman_proto::{
    AddSource, Engine, ErrorCode, JobId, JobOptions, JobState, RpcError, SourceKind, VideoOptions,
};
use dman_store::{Desired, JobRecord, LiveFigures};
use dman_ytdlp::{ItemProgress, Outcome, Progress, StopHandle};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::budget;
use crate::ops;
use crate::video::{build_spec, resolve_options, unavailable};

use super::util::now;
use super::{Entry, Manager};

/// `JobRecord.extra` key of the resolved [`VideoOptions`].
pub(crate) const KEY_OPTIONS: &str = "video";
/// `JobRecord.extra` key of every output path yt-dlp reported (for cleanup).
const KEY_PATHS: &str = "video_paths";
/// How long `remove` waits for a stopped yt-dlp to exit.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A message from a video forwarder task to the actor.
#[derive(Debug)]
pub(crate) enum VideoMsg {
    Done {
        id: JobId,
        run: u64,
        outcome: Outcome,
    },
}

/// Progress of one yt-dlp run, written by the forwarder, read on ticks.
#[derive(Debug, Default)]
pub(crate) struct VideoShared {
    /// `(downloaded, total)` per output file of this run.
    files: BTreeMap<String, (u64, Option<u64>)>,
    /// Paths not yet recorded in the job record.
    new_paths: Vec<String>,
    speed: u64,
    item: Option<ItemProgress>,
    current: Option<String>,
    /// Bytes transferred since the last tick (for the lifetime counters).
    pending: u64,
}

impl VideoShared {
    fn apply(&mut self, p: Progress) {
        self.speed = p.speed.map_or(0, |s| s.max(0.0) as u64);
        self.item = p.item.or(self.item);
        if let Some(f) = p.filename {
            match self.files.get_mut(&f) {
                Some(e) => {
                    self.pending = self
                        .pending
                        .saturating_add(p.downloaded.saturating_sub(e.0));
                    *e = (p.downloaded.max(e.0), p.total.or(e.1));
                }
                None => {
                    // The first figure of a file may include bytes of an earlier run (resume).
                    self.files.insert(f.clone(), (p.downloaded, p.total));
                    self.new_paths.push(f.clone());
                }
            }
            self.current = Some(f);
        }
    }

    fn totals(&self) -> (u64, u64) {
        self.files.values().fold((0, 0), |(d, t), (fd, ft)| {
            (d + fd, t + ft.unwrap_or(*fd).max(*fd))
        })
    }
}

/// A running yt-dlp process of a job.
pub(crate) struct VideoRun {
    run: u64,
    stop: StopHandle,
    shared: Arc<Mutex<VideoShared>>,
    task: JoinHandle<()>,
    /// Stop requested; the process may still be exiting.
    pub(crate) stopping: bool,
    /// The `--limit-rate` (global budget share / per-job limit) it was spawned with.
    cap: Option<u64>,
    spawned: std::time::Instant,
}

impl std::fmt::Debug for VideoRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoRun")
            .field("run", &self.run)
            .field("stopping", &self.stopping)
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

/// The stored options of a video job.
pub(crate) fn stored_options(rec: &JobRecord) -> VideoOptions {
    rec.extra
        .get(KEY_OPTIONS)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn job_tag(id: JobId) -> String {
    format!("job{id}")
}

/// Display name from a yt-dlp output path: the file name without the
/// `.fNNN` format tag of a not-yet-merged stream.
pub(crate) fn display_name(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 3 {
        let tag = parts[parts.len() - 2];
        if tag.len() > 1
            && tag.starts_with('f')
            && tag[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && tag[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            let mut keep: Vec<&str> = parts[..parts.len() - 2].to_vec();
            keep.push(parts[parts.len() - 1]);
            return keep.join(".");
        }
    }
    name
}

/// Every file a video job may have left in its directory: recorded final
/// paths, every path yt-dlp reported, their `.part`/`.ytdl`/fragment files and
/// dman's marker file. Only existing entries are returned.
pub(crate) fn cleanup_candidates(rec: &JobRecord) -> Vec<PathBuf> {
    let dir = PathBuf::from(&rec.dir);
    let mut known: Vec<PathBuf> = super::util::recorded_files(rec);
    if let Some(Value::Array(a)) = rec.extra.get(KEY_PATHS) {
        known.extend(a.iter().filter_map(Value::as_str).map(PathBuf::from));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for p in &known {
        let p = if p.is_absolute() {
            p.clone()
        } else {
            dir.join(p)
        };
        out.push(p.clone());
        let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
            continue;
        };
        let name = name.to_string_lossy().into_owned();
        let Ok(rd) = std::fs::read_dir(parent) else {
            continue;
        };
        for entry in rd.flatten() {
            let n = entry.file_name().to_string_lossy().into_owned();
            if n.strip_prefix(&name)
                .is_some_and(|rest| rest.starts_with(".part") || rest == ".ytdl")
            {
                out.push(entry.path());
            }
        }
    }
    out.push(dir.join(format!(".dman-{}.final", job_tag(rec.id))));
    out.retain(|p| std::fs::symlink_metadata(p).is_ok());
    out.sort();
    out.dedup();
    out
}

impl Manager {
    /// `jobs.add` of one `AddSource::Video`.
    pub(super) async fn add_video(
        &mut self,
        src: AddSource,
        options: &JobOptions,
        dir: &str,
        paused: bool,
    ) -> Result<Vec<JobId>, RpcError> {
        let AddSource::Video { url, video } = src else {
            return Err(RpcError::invalid("not a video source"));
        };
        if self.tools.ytdlp.is_none() {
            return Err(unavailable());
        }
        let url = url.trim().to_owned();
        let resolved = resolve_options(*video, &self.video_cfg);
        let spec = build_spec(
            &url,
            Path::new(dir),
            &resolved,
            options.max_download_limit,
            &self.tools,
        )
        .map_err(RpcError::invalid)?;
        let (id, key) = self
            .db(|s| Ok((s.next_job_id()?, s.next_queue_key()?)))
            .await?;
        let mut rec = JobRecord {
            id,
            engine: Engine::YtDlp,
            kind: SourceKind::Video,
            name: crate::tick::name_of_uri(spec.url.as_str()),
            dir: dir.to_owned(),
            source: spec.url.to_string(),
            options: options.clone(),
            desired: if paused {
                Desired::Paused
            } else {
                Desired::Queued
            },
            state: if paused {
                JobState::Paused
            } else {
                JobState::Waiting
            },
            gid: None,
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: now(),
            completed_at: None,
            queue_key: key,
            error: None,
            has_blob: false,
            extra: serde_json::Map::new(),
        };
        rec.extra.insert(
            KEY_OPTIONS.into(),
            serde_json::to_value(&resolved).unwrap_or(Value::Null),
        );
        {
            let rec = rec.clone();
            self.db(move |s| s.put_job(&rec)).await?;
        }
        tracing::info!(%id, "video job added");
        self.jobs.insert(id, Entry::new(rec));
        Ok(vec![id])
    }

    /// Spawns yt-dlp for job `id` (scheduler decision). A spawn failure fails the job.
    pub(super) fn start_video(&mut self, id: JobId) {
        let Some(e) = self.jobs.get(&id) else { return };
        let opts = stored_options(&e.rec);
        let cap = self.video_cap_for(id);
        let spec = build_spec(
            &e.rec.source,
            Path::new(&e.rec.dir),
            &opts,
            cap,
            &self.tools,
        );
        let started = spec.and_then(|spec| {
            dman_ytdlp::spawn(&spec, &self.tools, &job_tag(id)).map_err(|e| e.to_string())
        });
        self.video_runs += 1;
        let run = self.video_runs;
        let tx = self.video_tx.clone();
        let Some(e) = self.jobs.get_mut(&id) else {
            return;
        };
        let prev = e.rec.state;
        match started {
            Ok(mut running) => {
                tracing::debug!(%id, run, "starting yt-dlp");
                let stop = running.stop_handle();
                let shared: Arc<Mutex<VideoShared>> = Arc::default();
                let s2 = shared.clone();
                let task = tokio::spawn(async move {
                    while let Some(p) = running.next_progress().await {
                        if let Ok(mut s) = s2.lock() {
                            s.apply(p);
                        }
                    }
                    let outcome = running.wait().await;
                    let _ = tx.send(VideoMsg::Done { id, run, outcome });
                });
                e.video = Some(VideoRun {
                    run,
                    stop,
                    shared,
                    task,
                    stopping: false,
                    cap,
                    spawned: std::time::Instant::now(),
                });
                e.disk_blocked = false;
                e.rec.state = JobState::Active;
                e.rec.error = None;
                e.video_item = None;
            }
            Err(err) => {
                e.rec.state = JobState::Error;
                e.rec.desired = Desired::Stopped;
                e.rec.error = Some(format!("cannot start yt-dlp: {err}"));
                if let Some(o) = super::finish::detect(&mut e.rec, prev, JobState::Error) {
                    e.finished.push(o);
                }
            }
        }
        e.live = LiveFigures::default();
        e.dirty = true;
        self.urgent = true;
    }

    /// Running (not stopping) video jobs with their own rate limit.
    fn running_videos(&self) -> Vec<(JobId, Option<u64>)> {
        let mut v: Vec<(JobId, Option<u64>)> = self
            .jobs
            .values()
            .filter(|e| e.video.as_ref().is_some_and(|v| !v.stopping))
            .map(|e| (e.rec.id, e.rec.options.max_download_limit))
            .collect();
        v.sort();
        v
    }

    /// aria2 has running (or about to run) downloads: it holds one budget share.
    fn aria2_active(&self) -> bool {
        self.jobs
            .values()
            .any(|e| e.rec.engine == Engine::Aria2 && e.uses_slot())
    }

    /// The `--limit-rate` for video job `id` when it (re)starts now: its share
    /// of the global limit (see [`crate::budget`]) or just its own limit.
    fn video_cap_for(&self, id: JobId) -> Option<u64> {
        let mut jobs = self.running_videos();
        if !jobs.iter().any(|j| j.0 == id) {
            let own = self
                .jobs
                .get(&id)
                .and_then(|e| e.rec.options.max_download_limit);
            jobs.push((id, own));
        }
        let limits: Vec<Option<u64>> = jobs.iter().map(|j| j.1).collect();
        let caps = budget::video_caps(self.limits.download, &limits, self.aria2_active());
        jobs.iter().position(|j| j.0 == id).and_then(|i| caps[i])
    }

    /// aria2's `max-overall-download-limit` under global limit `global`: what
    /// the running video jobs leave of it (0 = unlimited).
    pub(super) fn aria2_download_limit(&self, global: u64) -> u64 {
        let limits: Vec<Option<u64>> = self.running_videos().into_iter().map(|j| j.1).collect();
        let caps = budget::video_caps(global, &limits, true);
        budget::aria2_limit(global, &caps)
    }

    /// Keeps the global download budget: respawns running video jobs whose
    /// share changed materially (hysteresis in [`budget::should_respawn`]) and
    /// pushes aria2's share. Every tick, after the scheduler.
    pub(super) async fn balance_limits(&mut self) {
        let global = self.limits.download;
        if global > 0
            || self
                .jobs
                .values()
                .any(|e| e.video.as_ref().is_some_and(|v| v.cap.is_some()))
        {
            let running = self.running_videos();
            let limits: Vec<Option<u64>> = running.iter().map(|j| j.1).collect();
            let wanted = budget::video_caps(global, &limits, self.aria2_active());
            let min = self.respawn_min;
            let mut respawn = Vec::new();
            for ((id, _), want) in running.iter().zip(wanted) {
                let Some(v) = self.jobs.get(id).and_then(|e| e.video.as_ref()) else {
                    continue;
                };
                if budget::should_respawn(v.cap, want, v.spawned.elapsed(), min) {
                    respawn.push(*id);
                }
            }
            for id in respawn {
                tracing::debug!(%id, "respawning yt-dlp for a new rate share");
                self.stop_video(id, JobState::Waiting);
            }
        }
        self.sync_global_limits().await;
    }

    /// Asks a running yt-dlp to stop; the job goes to `state` now (the
    /// process may still be exiting; it is not started again before it did).
    pub(super) fn stop_video(&mut self, id: JobId, state: JobState) {
        let Some(e) = self.jobs.get_mut(&id) else {
            return;
        };
        if let Some(v) = e.video.as_mut()
            && !v.stopping
        {
            v.stop.stop();
            v.stopping = true;
        }
        e.live = LiveFigures::default();
        e.video_item = None;
        if e.rec.state != state && !e.rec.state.is_stopped() {
            e.rec.state = state;
            e.dirty = true;
            self.urgent = true;
        }
    }

    /// Folds the progress of running video jobs into their records (every tick).
    pub(super) fn pump_videos(&mut self) {
        let mut transferred = 0u64;
        for e in self.jobs.values_mut() {
            let Some(v) = &e.video else { continue };
            let Ok(mut s) = v.shared.lock() else { continue };
            transferred = transferred.saturating_add(std::mem::take(&mut s.pending));
            let new_paths = std::mem::take(&mut s.new_paths);
            if v.stopping {
                continue;
            }
            let (done, total) = s.totals();
            if !s.files.is_empty() && (done, total) != (e.rec.completed_bytes, e.rec.total_bytes) {
                e.rec.completed_bytes = done;
                e.rec.total_bytes = total;
                e.dirty = true;
            }
            e.live.download_speed = s.speed;
            e.video_item = s.item.map(|i| (i.index, i.count));
            if let Some(cur) = &s.current {
                let name = display_name(cur);
                if !name.is_empty() && name != e.rec.name {
                    e.rec.name = name;
                    e.dirty = true;
                }
            }
            drop(s);
            if !new_paths.is_empty() {
                let list = e
                    .rec
                    .extra
                    .entry(KEY_PATHS.to_owned())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(a) = list {
                    for p in new_paths {
                        if !a.iter().any(|x| x.as_str() == Some(p.as_str())) {
                            a.push(Value::String(p));
                        }
                    }
                }
                e.dirty = true;
            }
        }
        self.pending_transfer.0 = self.pending_transfer.0.saturating_add(transferred);
    }

    /// Sum of the download speeds of running video jobs.
    pub(super) fn video_speed(&self) -> u64 {
        self.jobs
            .values()
            .filter(|e| e.video.is_some())
            .map(|e| e.live.download_speed)
            .sum()
    }

    /// A yt-dlp process ended.
    pub(super) async fn on_video_msg(&mut self, msg: VideoMsg) {
        let VideoMsg::Done { id, run, outcome } = msg;
        self.pump_videos();
        let Some(e) = self.jobs.get_mut(&id) else {
            return;
        };
        if e.video.as_ref().is_none_or(|v| v.run != run) {
            return; // removed meanwhile (remove waits for the process itself)
        }
        let v = e.video.take();
        if let Some(v) = &v
            && let Ok(mut s) = v.shared.lock()
        {
            self.pending_transfer.0 = self.pending_transfer.0.saturating_add(s.pending);
            s.pending = 0;
        }
        e.live = LiveFigures::default();
        e.video_item = None;
        let prev = e.rec.state;
        let allow_split = stored_options(&e.rec).allow_separate_streams;
        let mut warn = None;
        let new_state = match outcome {
            Outcome::Stopped => {
                tracing::debug!(%id, "yt-dlp stopped");
                None
            }
            Outcome::Finished { paths } => {
                record_outputs(&mut e.rec, &paths);
                if paths.len() == 1 {
                    e.rec.name = display_name(&paths[0].to_string_lossy());
                }
                Some(JobState::Complete)
            }
            Outcome::Split { files, finished } => {
                let mut all = finished.clone();
                all.extend(files.iter().cloned());
                record_outputs(&mut e.rec, &all);
                let names: Vec<String> = files
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                if allow_split {
                    warn = Some(format!(
                        "{}: video and audio were saved as separate files because ffmpeg is \
                         missing (not merged): {}",
                        e.rec.name,
                        names.join(", ")
                    ));
                    Some(JobState::Complete)
                } else {
                    e.rec.error = Some(format!(
                        "ffmpeg is missing, so the video and audio streams could not be merged; \
                         they were left as separate files ({}). Install ffmpeg and retry, or \
                         allow separate streams",
                        names.join(", ")
                    ));
                    Some(JobState::Error)
                }
            }
            Outcome::Failed { message, .. } => {
                e.rec.error = Some(message);
                Some(JobState::Error)
            }
        };
        if let Some(state) = new_state
            && e.rec.state != JobState::Removed
        {
            e.rec.state = state;
            e.rec.desired = Desired::Stopped;
            if state == JobState::Complete {
                e.rec.completed_at.get_or_insert(now());
                e.rec.error = None;
            }
            if let Some(o) = super::finish::detect(&mut e.rec, prev, state) {
                e.finished.push(o);
            }
            tracing::info!(%id, ?state, "video job finished");
        }
        e.dirty = true;
        self.urgent = true;
        if let Some(text) = warn {
            self.message(Level::Warn, text);
        }
        self.tick().await;
    }

    /// Stops a video job's process (if any) and waits until it exited.
    pub(super) async fn halt_video(&mut self, id: JobId) {
        let Some(v) = self.jobs.get_mut(&id).and_then(|e| e.video.take()) else {
            return;
        };
        v.stop.stop();
        let abort = v.task.abort_handle();
        if tokio::time::timeout(STOP_WAIT, v.task).await.is_err() {
            abort.abort();
        }
        if let Ok(mut s) = v.shared.lock() {
            self.pending_transfer.0 = self.pending_transfer.0.saturating_add(s.pending);
            s.pending = 0;
        }
    }

    /// `jobs.remove` of a video job.
    pub(super) async fn remove_video(
        &mut self,
        id: JobId,
        delete_files: bool,
    ) -> Result<(), RpcError> {
        self.halt_video(id).await;
        let Some(e) = self.jobs.get_mut(&id) else {
            return Err(RpcError::not_found(format!("job {id}")));
        };
        if e.rec.state != JobState::Removed {
            tracing::info!(%id, "video job removed");
        }
        e.rec.state = JobState::Removed;
        e.rec.desired = Desired::Stopped;
        e.live = LiveFigures::default();
        e.video_item = None;
        let rec = e.rec.clone();
        self.mark(id, true);
        if !delete_files {
            return Ok(());
        }
        let dir = PathBuf::from(&rec.dir);
        let report = tokio::task::spawn_blocking(move || {
            let files = cleanup_candidates(&rec);
            ops::delete_job_files(&dir, &files)
        })
        .await
        .map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))?;
        tracing::info!(%id, deleted = report.deleted.len(), "deleted video job files");
        if report.refused.is_empty() && report.failed.is_empty() {
            return Ok(());
        }
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

    /// Daemon shutdown: stop every yt-dlp (partial files stay for `--continue`).
    /// Records keep their state; `load` turns `Active` video jobs into `Waiting`.
    pub(super) async fn shutdown_videos(&mut self) {
        let ids: Vec<JobId> = self
            .jobs
            .values()
            .filter(|e| e.video.is_some())
            .map(|e| e.rec.id)
            .collect();
        let mut waits = Vec::new();
        for id in ids {
            if let Some(v) = self.jobs.get_mut(&id).and_then(|e| e.video.take()) {
                v.stop.stop();
                waits.push(v.task);
            }
        }
        for t in waits {
            let abort = t.abort_handle();
            if tokio::time::timeout(STOP_WAIT, t).await.is_err() {
                abort.abort();
            }
        }
    }
}

/// Records the final output paths and sizes of a finished video job.
fn record_outputs(rec: &mut JobRecord, paths: &[PathBuf]) {
    let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let size: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    if size > 0 {
        rec.total_bytes = size;
        rec.completed_bytes = size;
    }
    rec.extra.insert("files".into(), json!(list));
    rec.extra.insert("details_final".into(), Value::Bool(true));
}

/// The progress channel of a forwarder; only used to type the manager field.
pub(crate) type VideoTx = mpsc::UnboundedSender<VideoMsg>;

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(dir: &Path) -> JobRecord {
        JobRecord {
            id: JobId(7),
            engine: Engine::YtDlp,
            kind: SourceKind::Video,
            name: "v".into(),
            dir: dir.display().to_string(),
            source: "https://e.com/v".into(),
            options: JobOptions::default(),
            desired: Desired::Queued,
            state: JobState::Waiting,
            gid: None,
            info_hash: None,
            total_bytes: 0,
            completed_bytes: 0,
            uploaded_base: 0,
            uploaded_session: 0,
            added_at: 0,
            completed_at: None,
            queue_key: 0,
            error: None,
            has_blob: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn display_names_drop_format_tags() {
        assert_eq!(display_name("/d/A [x].f137.mp4"), "A [x].mp4");
        assert_eq!(display_name("/d/A [x].f251-drc.webm"), "A [x].webm");
        assert_eq!(display_name("/d/A [x].mp4"), "A [x].mp4");
        assert_eq!(display_name("/d/file.final.mp4"), "file.final.mp4");
    }

    #[test]
    fn options_persist_in_record_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = rec(dir.path());
        let o = VideoOptions {
            quality: Some("720p".into()),
            playlist_items: Some("1-3".into()),
            allow_separate_streams: true,
            ..Default::default()
        };
        r.extra
            .insert(KEY_OPTIONS.into(), serde_json::to_value(&o).unwrap());
        let store = dman_store::Store::open(dir.path().join("s.redb")).unwrap();
        store.put_job(&r).unwrap();
        let back = store.list_jobs().unwrap().pop().unwrap();
        assert_eq!(back.engine, Engine::YtDlp);
        assert_eq!(stored_options(&back), o);
    }

    #[test]
    fn shared_progress_sums_streams_and_counts_transfer() {
        let mut s = VideoShared::default();
        let p = |f: &str, d: u64, t: u64| Progress {
            downloaded: d,
            total: Some(t),
            speed: Some(10.0),
            eta: None,
            status: dman_ytdlp::Status::Downloading,
            filename: Some(f.into()),
            fragment_index: None,
            fragment_count: None,
            item: None,
        };
        s.apply(p("/d/a.f1.mp4", 100, 1000)); // resumed at 100: not counted
        s.apply(p("/d/a.f1.mp4", 400, 1000));
        s.apply(p("/d/a.f2.m4a", 0, 200));
        s.apply(p("/d/a.f2.m4a", 50, 200));
        assert_eq!(s.totals(), (450, 1200));
        assert_eq!(s.pending, 350);
        assert_eq!(s.new_paths.len(), 2);
    }

    #[test]
    fn cleanup_only_touches_the_jobs_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for f in [
            "A [x].mp4",
            "A [x].f1.mp4.part",
            "A [x].f1.mp4.part-Frag3",
            "A [x].f1.mp4.ytdl",
            "A [x].f2.m4a",
            "other.mp4",
            "other.mp4.part",
            "A [x].mp4.bak",
            ".dman-job7.final",
            ".dman-job8.final",
        ] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let mut r = rec(d);
        r.extra.insert(
            "files".into(),
            json!([d.join("A [x].mp4").display().to_string()]),
        );
        r.extra.insert(
            KEY_PATHS.into(),
            json!([
                d.join("A [x].f1.mp4").display().to_string(),
                d.join("A [x].f2.m4a").display().to_string()
            ]),
        );
        let c = cleanup_candidates(&r);
        let names: Vec<String> = c
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                ".dman-job7.final",
                "A [x].f1.mp4.part",
                "A [x].f1.mp4.part-Frag3",
                "A [x].f1.mp4.ytdl",
                "A [x].f2.m4a",
                "A [x].mp4"
            ]
        );
        let report = ops::delete_job_files(d, &c);
        assert_eq!(report.deleted.len(), 6, "{report:?}");
        for keep in [
            "other.mp4",
            "other.mp4.part",
            "A [x].mp4.bak",
            ".dman-job8.final",
        ] {
            assert!(d.join(keep).exists(), "{keep}");
        }
    }
}
