//! The job manager actor: the single owner of all job state.
//!
//! API handlers talk to it through [`ManagerHandle`] (an mpsc of requests with
//! oneshot replies); it publishes [`Event`]s on a broadcast channel. Requests,
//! ticks, engine notifications and engine state changes are processed one at a
//! time, so no state is shared and no lock is ever held across an await.

mod add;
mod control;
mod detail;
mod events;
pub(crate) mod finish;
mod fs;
mod poll;
mod remove;
mod rename;
mod scheduler;
mod timetable;
mod util;
pub(crate) mod video;

use util::store_err;
pub(crate) use util::{file_paths, now, pause_gid};

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use dman_aria2::{Aria2Client, Aria2Notification, DownloadStatus, EngineState};
use dman_config::Profile;
use dman_proto::method::{
    BatchResult, EngineStatus, Event, FsListParams, Level, LimitsUpdate, ListParams, RemoveParams,
    RenameParams, ReorderParams, SetOptionsParams,
};
use dman_proto::{AddParams, Engine, ErrorCode, Job, JobId, JobState, Limits, RpcError};
use dman_store::{Desired, JobRecord, LiveFigures, Store};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use timetable::Timetable;

use crate::tick::Counters;

/// A request from an API handler.
#[derive(Debug)]
pub enum Request {
    /// `jobs.add`
    Add(AddParams),
    /// `jobs.list`
    List(ListParams),
    /// `jobs.get`
    Get(JobId),
    /// `jobs.detail`
    Detail(JobId),
    /// `jobs.pause`
    Pause(Vec<JobId>),
    /// `jobs.resume`
    Resume(Vec<JobId>),
    /// `jobs.remove`
    Remove(RemoveParams),
    /// `jobs.purge`
    Purge(Vec<JobId>),
    /// `jobs.reorder`
    Reorder(ReorderParams),
    /// `jobs.set_options`
    SetOptions(SetOptionsParams),
    /// `jobs.rename`
    Rename(RenameParams),
    /// `queue.pause_all`
    PauseAll,
    /// `queue.resume_all`
    ResumeAll,
    /// `limits.get`
    LimitsGet,
    /// `limits.set`
    LimitsSet(LimitsUpdate),
    /// `schedule.get`
    ScheduleGet,
    /// `stats.get`
    Stats,
    /// `stats.history`
    StatsHistory,
    /// `engine.status`
    EngineStatus,
    /// `fs.list`
    FsList(FsListParams),
    /// A [`Event::Snapshot`] (for `session.subscribe`).
    Snapshot,
}

enum Msg {
    Call(Box<Request>, oneshot::Sender<Result<Value, RpcError>>),
    Shutdown(oneshot::Sender<()>),
}

/// Cloneable handle to the manager actor.
#[derive(Clone, Debug)]
pub struct ManagerHandle {
    tx: mpsc::Sender<Msg>,
}

fn gone() -> RpcError {
    RpcError::new(ErrorCode::General, "the daemon is shutting down")
}

impl ManagerHandle {
    /// Sends a request and waits for its JSON result.
    pub async fn call(&self, req: Request) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Call(Box::new(req), tx))
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Asks the manager to flush everything to the store and stop. Returns
    /// once done (or immediately when it already stopped).
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// Static inputs of the manager.
pub struct ManagerConfig {
    /// The authoritative store.
    pub store: Store,
    /// The served profile (download dir, disk thresholds).
    pub profile: Profile,
    /// The configured `[limits]` (base limits, outside any schedule window).
    pub limits: Limits,
    /// Schedule windows in priority order (empty = everything runs always).
    pub schedule: Vec<dman_config::ScheduleWindow>,
    /// Time source for the schedule.
    pub clock: std::sync::Arc<dyn crate::schedule::Clock>,
    /// Where base limit changes are saved; `None` = runtime only.
    pub config_paths: Option<dman_config::Paths>,
    /// Poll interval.
    pub tick: Duration,
    /// Event bus.
    pub events: broadcast::Sender<Event>,
    /// Engine state published by the supervisor.
    pub engine: watch::Receiver<EngineState>,
    /// Magnet previews running in aria2; never adopted as jobs.
    pub previews: std::sync::Arc<crate::preview::PreviewRegistry>,
    /// Tracker list attached to new torrents and magnets.
    pub trackers: std::sync::Arc<crate::search::Trackers>,
    /// `[video]` defaults applied to new video jobs.
    pub video: dman_config::Video,
    /// Detected yt-dlp / ffmpeg (re-detected on `engine.restart`).
    pub video_tools: watch::Receiver<dman_ytdlp::Tools>,
}

/// In-memory state of one job.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) rec: JobRecord,
    pub(crate) live: LiveFigures,
    /// Last aria2 status seen for the current gid.
    pub(crate) aria2: Option<DownloadStatus>,
    pub(crate) counters: Option<Counters>,
    emitted: Option<Job>,
    pub(crate) dirty: bool,
    /// Outcomes to announce with the next `JobFinished` events.
    pub(crate) finished: Vec<dman_proto::method::Outcome>,
    /// Name/file list must (still) be fetched from aria2.
    pub(crate) need_details: bool,
    /// Held back by the disk guard (warning already sent).
    disk_blocked: bool,
    pub(crate) missing: u32,
    /// The running yt-dlp process of a video job.
    pub(crate) video: Option<video::VideoRun>,
    /// Playlist position `(n, m)` of a running video job.
    pub(crate) video_item: Option<(u32, u32)>,
}

impl Entry {
    pub(crate) fn new(rec: JobRecord) -> Self {
        let need_details = rec.engine == Engine::Aria2
            && !rec.state.is_stopped()
            && rec.extra.get("details_final").and_then(Value::as_bool) != Some(true);
        Self {
            rec,
            live: LiveFigures::default(),
            aria2: None,
            counters: None,
            emitted: None,
            dirty: false,
            finished: Vec::new(),
            need_details,
            disk_blocked: false,
            missing: 0,
            video: None,
            video_item: None,
        }
    }

    /// Downloading (or about to: unpaused and waiting in aria2), i.e. using a
    /// slot. Seeding does not count.
    fn uses_slot(&self) -> bool {
        if let Some(v) = &self.video {
            return !v.stopping;
        }
        match self.aria2 {
            Some(DownloadStatus::Active) => self.rec.state != JobState::Seeding,
            Some(DownloadStatus::Waiting) => true,
            _ => false,
        }
    }

    fn running_in_engine(&self) -> bool {
        if let Some(v) = &self.video {
            return !v.stopping;
        }
        matches!(
            self.aria2,
            Some(DownloadStatus::Active | DownloadStatus::Waiting)
        )
    }
}

/// The actor.
pub struct Manager {
    pub(crate) store: Store,
    pub(crate) profile: Profile,
    pub(crate) default_dir: PathBuf,
    /// Limits in effect (base, window and temporary overrides applied).
    pub(crate) limits: Limits,
    pub(crate) timetable: Timetable,
    tick_every: Duration,
    events: broadcast::Sender<Event>,
    pub(crate) jobs: HashMap<JobId, Entry>,
    pub(crate) gids: HashMap<String, JobId>,
    pub(crate) client: Option<Aria2Client>,
    engine: EngineStatus,
    engine_was_down: bool,
    global_speed: (u64, u64),
    totals: (u64, u64),
    pending_transfer: (u64, u64),
    last_flush: Instant,
    /// A state transition happened: flush at the end of the current step.
    pub(crate) urgent: bool,
    poll_failing: bool,
    pub(crate) history: crate::history::History,
    pub(crate) previews: std::sync::Arc<crate::preview::PreviewRegistry>,
    pub(crate) trackers: std::sync::Arc<crate::search::Trackers>,
    pub(crate) video_cfg: dman_config::Video,
    /// yt-dlp / ffmpeg as detected now.
    pub(crate) tools: dman_ytdlp::Tools,
    pub(crate) video_tx: video::VideoTx,
    /// Counter identifying yt-dlp runs (stale completions are ignored).
    pub(crate) video_runs: u64,
    /// Minimum time between rate-share respawns of one video job.
    pub(crate) respawn_min: Duration,
}

fn to_value<T: Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))
}

impl Manager {
    /// Spawns the actor and returns its handle and join handle.
    pub fn spawn(cfg: ManagerConfig) -> (ManagerHandle, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(64);
        let (video_tx, video_rx) = mpsc::unbounded_channel();
        let tools = cfg.video_tools.borrow().clone();
        let default_dir = cfg.profile.download_path();
        let mgr = Manager {
            store: cfg.store,
            profile: cfg.profile,
            default_dir,
            timetable: Timetable::new(
                cfg.schedule,
                cfg.clock,
                cfg.config_paths,
                cfg.limits.clone(),
            ),
            limits: cfg.limits,
            tick_every: cfg.tick,
            events: cfg.events,
            jobs: HashMap::new(),
            gids: HashMap::new(),
            client: None,
            engine: EngineStatus {
                engine: Engine::Aria2,
                available: false,
                version: None,
                features: Vec::new(),
                reason: Some("starting".into()),
            },
            engine_was_down: false,
            global_speed: (0, 0),
            totals: (0, 0),
            pending_transfer: (0, 0),
            last_flush: Instant::now(),
            urgent: false,
            poll_failing: false,
            history: crate::history::History::default(),
            previews: cfg.previews,
            trackers: cfg.trackers,
            video_cfg: cfg.video,
            tools,
            video_tx,
            video_runs: 0,
            respawn_min: crate::budget::respawn_interval(),
        };
        let task = tokio::spawn(mgr.run(rx, cfg.engine, video_rx, cfg.video_tools));
        (ManagerHandle { tx }, task)
    }

    pub(crate) async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> dman_store::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let s = self.store.clone();
        tokio::task::spawn_blocking(move || f(&s))
            .await
            .map_err(store_err)?
            .map_err(store_err)
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    pub(crate) fn message(&self, level: Level, text: impl Into<String>) {
        let text = text.into();
        match level {
            Level::Error => tracing::error!("{text}"),
            Level::Warn => tracing::warn!("{text}"),
            Level::Info => tracing::info!("{text}"),
        }
        self.emit(Event::Message { level, text });
    }

    async fn load(&mut self) {
        match self.db(|s| Ok((s.list_jobs()?, s.totals()?))).await {
            Ok((records, totals)) => {
                self.totals = totals;
                for mut rec in records {
                    let dirty = rec.engine == Engine::YtDlp && rec.state == JobState::Active;
                    if dirty {
                        // yt-dlp was stopped with the daemon: queue it again
                        // (the respawn resumes its .part files).
                        rec.state = JobState::Waiting;
                        self.urgent = true;
                    }
                    if let Some(g) = &rec.gid {
                        self.gids.insert(g.clone(), rec.id);
                    }
                    let mut e = Entry::new(rec);
                    e.dirty = dirty;
                    self.jobs.insert(e.rec.id, e);
                }
                tracing::info!(jobs = self.jobs.len(), "loaded job records");
            }
            Err(e) => tracing::error!("cannot load job records: {}", e.message),
        }
    }

    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Msg>,
        mut engine: watch::Receiver<EngineState>,
        mut video_rx: mpsc::UnboundedReceiver<video::VideoMsg>,
        mut tools_rx: watch::Receiver<dman_ytdlp::Tools>,
    ) {
        self.load().await;
        self.tools = tools_rx.borrow_and_update().clone();
        self.emit(Event::Engine {
            status: crate::video::engine_status(&self.tools),
        });
        let mut tools_open = true;
        let initial = engine.borrow_and_update().clone();
        let mut notif = self.on_engine_state(initial).await;
        let mut engine_open = true;
        let mut interval = tokio::time::interval(self.tick_every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                msg = rx.recv() => match msg {
                    Some(Msg::Call(req, reply)) => {
                        let res = self.handle(*req).await;
                        let _ = reply.send(res);
                        self.flush(false).await;
                    }
                    Some(Msg::Shutdown(done)) => {
                        self.shutdown_videos().await;
                        self.flush(true).await;
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.shutdown_videos().await;
                        self.flush(true).await;
                        return;
                    }
                },
                Some(m) = video_rx.recv() => {
                    self.on_video_msg(m).await;
                }
                changed = tools_rx.changed(), if tools_open => {
                    if changed.is_err() {
                        tools_open = false;
                    } else {
                        self.tools = tools_rx.borrow_and_update().clone();
                        self.emit(Event::Engine {
                            status: crate::video::engine_status(&self.tools),
                        });
                        self.tick().await;
                    }
                }
                changed = engine.changed(), if engine_open => {
                    if changed.is_err() {
                        engine_open = false;
                    } else {
                        let st = engine.borrow_and_update().clone();
                        notif = self.on_engine_state(st).await;
                    }
                }
                n = recv_notification(&mut notif) => {
                    if n.is_none() {
                        notif = None;
                    } else if let Some(rx) = notif.as_mut() {
                        // Coalesce bursts into one tick.
                        while rx.try_recv().is_ok() {}
                    }
                    self.tick().await;
                }
                _ = interval.tick() => {
                    self.tick().await;
                    self.sample_history();
                }
            }
        }
    }

    async fn on_engine_state(
        &mut self,
        st: EngineState,
    ) -> Option<broadcast::Receiver<Aria2Notification>> {
        match st {
            EngineState::Running {
                client,
                info,
                attached,
            } => {
                tracing::info!(version = %info.version, attached, "aria2 running");
                self.client = Some(client.clone());
                let notif = client.subscribe();
                self.engine = EngineStatus {
                    engine: Engine::Aria2,
                    available: true,
                    version: Some(info.version.clone()),
                    features: info.features.clone(),
                    reason: None,
                };
                self.emit(Event::Engine {
                    status: self.engine.clone(),
                });
                if self.engine_was_down {
                    self.message(Level::Info, "aria2 is running again");
                }
                self.engine_was_down = false;
                self.limits_not_applied();
                self.sync_global_limits().await;
                self.reconcile(&client).await;
                self.tick().await;
                Some(notif)
            }
            other => {
                self.client = None;
                for e in self.jobs.values_mut() {
                    e.live = LiveFigures::default();
                    e.aria2 = None;
                }
                self.global_speed = (0, 0);
                let (reason, msg) = match other {
                    EngineState::Restarting { attempt, reason } => (
                        format!("restarting (attempt {attempt}): {reason}"),
                        Some((
                            Level::Warn,
                            format!("aria2 stopped ({reason}); restarting (attempt {attempt})"),
                        )),
                    ),
                    EngineState::Failed { reason } => (
                        reason.clone(),
                        Some((
                            Level::Error,
                            format!("aria2 failed and will not be restarted: {reason}"),
                        )),
                    ),
                    EngineState::Stopped => ("stopped".to_owned(), None),
                    EngineState::Starting | EngineState::Running { .. } => {
                        ("starting".to_owned(), None)
                    }
                };
                self.engine = EngineStatus {
                    engine: Engine::Aria2,
                    available: false,
                    version: None,
                    features: Vec::new(),
                    reason: Some(reason),
                };
                self.engine_was_down = true;
                self.emit(Event::Engine {
                    status: self.engine.clone(),
                });
                if let Some((level, text)) = msg {
                    self.message(level, text);
                }
                self.emit_changes();
                None
            }
        }
    }

    // ----- requests ----------------------------------------------------------

    async fn handle(&mut self, req: Request) -> Result<Value, RpcError> {
        match req {
            Request::Add(p) => {
                let r = self.add(p).await?;
                self.tick().await;
                to_value(r)
            }
            Request::List(p) => {
                let jobs: Vec<Job> = self
                    .all_jobs()
                    .into_iter()
                    .filter(|j| p.states.is_empty() || p.states.contains(&j.state))
                    .collect();
                to_value(jobs)
            }
            Request::Get(id) => {
                let e = self
                    .jobs
                    .get(&id)
                    .ok_or_else(|| RpcError::not_found(format!("job {id}")))?;
                to_value(Self::job_view(e, &self.positions()))
            }
            Request::Detail(id) => to_value(self.detail(id).await?),
            Request::Pause(ids) => {
                let r = self.pause(&ids).await;
                self.emit_changes();
                to_value(r)
            }
            Request::Resume(ids) => {
                let r = self.resume(&ids).await;
                self.tick().await;
                to_value(r)
            }
            Request::Remove(p) => {
                let r = self.remove(p).await;
                self.emit_changes();
                to_value(r)
            }
            Request::Purge(ids) => to_value(self.purge(ids).await),
            Request::Reorder(p) => {
                self.reorder(p).await?;
                self.emit_changes();
                Ok(Value::Null)
            }
            Request::SetOptions(p) => {
                self.set_options(p).await?;
                self.emit_changes();
                Ok(Value::Null)
            }
            Request::Rename(p) => {
                self.rename(p).await?;
                self.emit_changes();
                Ok(Value::Null)
            }
            Request::PauseAll => {
                let ids: Vec<JobId> = self
                    .jobs
                    .values()
                    .filter(|e| !e.rec.state.is_stopped() && e.rec.desired != Desired::Paused)
                    .map(|e| e.rec.id)
                    .collect();
                let r = self.pause(&ids).await;
                self.emit_changes();
                batch_to_unit(r)
            }
            Request::ResumeAll => {
                let ids: Vec<JobId> = self
                    .jobs
                    .values()
                    .filter(|e| !e.rec.state.is_stopped() && e.rec.desired == Desired::Paused)
                    .map(|e| e.rec.id)
                    .collect();
                let r = self.resume(&ids).await;
                self.tick().await;
                batch_to_unit(r)
            }
            Request::LimitsGet => to_value(self.limits.clone()),
            Request::LimitsSet(u) => {
                let l = self.set_limits(u).await?;
                self.tick().await;
                to_value(l)
            }
            Request::ScheduleGet => to_value(self.schedule_status()),
            Request::Stats => to_value(self.stats()),
            Request::StatsHistory => to_value(self.history.global()),
            Request::EngineStatus => to_value(vec![
                self.engine.clone(),
                crate::video::engine_status(&self.tools),
            ]),
            Request::FsList(p) => to_value(fs::list(&self.profile, p).await?),
            Request::Snapshot => {
                // Bring every subscriber's baseline in line with what was sent.
                self.emit_changes();
                to_value(Event::Snapshot {
                    jobs: self.all_jobs(),
                    stats: self.stats(),
                    limits: self.limits.clone(),
                })
            }
        }
    }
}

fn batch_to_unit(r: BatchResult) -> Result<Value, RpcError> {
    if r.failed.is_empty() {
        return Ok(Value::Null);
    }
    let mut err = RpcError::new(
        ErrorCode::PartialFailure,
        format!("{} job(s) could not be changed", r.failed.len()),
    );
    err.data = serde_json::to_value(&r.failed).ok();
    Err(err)
}

async fn recv_notification(rx: &mut Option<broadcast::Receiver<Aria2Notification>>) -> Option<()> {
    match rx {
        Some(r) => match r.recv().await {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => Some(()),
            Err(broadcast::error::RecvError::Closed) => None,
        },
        None => std::future::pending().await,
    }
}
