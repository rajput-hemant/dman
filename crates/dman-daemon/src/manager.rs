//! The job manager actor: the single owner of all job state.
//!
//! API handlers talk to it through [`ManagerHandle`] (an mpsc of requests with
//! oneshot replies); it publishes [`Event`]s on a broadcast channel. Requests,
//! ticks, engine notifications and engine state changes are processed one at a
//! time, so no state is shared and no lock is ever held across an await.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use dman_aria2::{
    Aria2Client, Aria2Notification, DownloadStatus, EngineState, Gid, Options, PosHow, Status,
    job_options_to_aria2, requires_pause,
};
use dman_config::Profile;
use dman_proto::method::{
    BatchFailure, BatchResult, EngineStatus, Event, Level, LimitsUpdate, ListParams, RemoveParams,
    ReorderParams, SetOptionsParams,
};
use dman_proto::{
    AddOutcome, AddParams, AddSource, Engine, ErrorCode, GlobalStats, Job, JobId, JobOptions,
    JobState, Limits, RpcError, SourceKind,
};
use dman_store::{Desired, JobRecord, LiveFigures, Store};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::disk::{self, Space};
use crate::ops;
use crate::queue;
use crate::tick::{self, Counters};

/// Persist dirty records at least this often (state transitions are persisted
/// immediately).
const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// A non-finished job whose gid is absent from aria2 for this many ticks is
/// re-added from its stored source.
const MISSING_TICKS: u32 = 3;
/// At most this many detail (`files`/`bittorrent`) lookups per tick.
const MAX_DETAIL_CALLS: usize = 32;
const WAITING_LIMIT: u32 = 10_000;
const STOPPED_LIMIT: u32 = 1_000;

/// A request from an API handler.
#[derive(Debug)]
pub enum Request {
    /// `jobs.add`
    Add(AddParams),
    /// `jobs.list`
    List(ListParams),
    /// `jobs.get`
    Get(JobId),
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
    /// `queue.pause_all`
    PauseAll,
    /// `queue.resume_all`
    ResumeAll,
    /// `limits.get`
    LimitsGet,
    /// `limits.set`
    LimitsSet(LimitsUpdate),
    /// `stats.get`
    Stats,
    /// `engine.status`
    EngineStatus,
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
    /// Initial limits.
    pub limits: Limits,
    /// Poll interval.
    pub tick: Duration,
    /// Event bus.
    pub events: broadcast::Sender<Event>,
    /// Engine state published by the supervisor.
    pub engine: watch::Receiver<EngineState>,
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
    /// Name/file list must (still) be fetched from aria2.
    pub(crate) need_details: bool,
    /// Held back by the disk guard (warning already sent).
    disk_blocked: bool,
    pub(crate) missing: u32,
}

impl Entry {
    pub(crate) fn new(rec: JobRecord) -> Self {
        let need_details = !rec.state.is_stopped()
            && rec.extra.get("details_final").and_then(Value::as_bool) != Some(true);
        Self {
            rec,
            live: LiveFigures::default(),
            aria2: None,
            counters: None,
            emitted: None,
            dirty: false,
            need_details,
            disk_blocked: false,
            missing: 0,
        }
    }

    /// Downloading (or about to: unpaused and waiting in aria2), i.e. using a
    /// slot. Seeding does not count.
    fn uses_slot(&self) -> bool {
        match self.aria2 {
            Some(DownloadStatus::Active) => self.rec.state != JobState::Seeding,
            Some(DownloadStatus::Waiting) => true,
            _ => false,
        }
    }

    fn running_in_engine(&self) -> bool {
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
    limits: Limits,
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
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn to_value<T: Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))
}

pub(crate) fn store_err(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::General, format!("store error: {e}"))
}

/// Maps an aria2 client error to an API error.
pub(crate) fn engine_err(e: dman_aria2::Error) -> RpcError {
    match e {
        dman_aria2::Error::Rpc { message, .. } => {
            RpcError::new(ErrorCode::General, format!("aria2: {message}"))
        }
        other => RpcError::new(
            ErrorCode::EngineUnavailable,
            format!("aria2 unavailable: {other}"),
        ),
    }
}

fn unavailable(reason: &str) -> RpcError {
    RpcError::new(
        ErrorCode::EngineUnavailable,
        format!("aria2 is not available: {reason}"),
    )
}

fn is_rpc_msg(e: &dman_aria2::Error, needle: &str) -> bool {
    matches!(e, dman_aria2::Error::Rpc { message, .. } if message.contains(needle))
}

/// `pause`, retried once after 200 ms on "cannot be paused now", then
/// `forcePause` (spike finding).
pub(crate) async fn pause_gid(client: &Aria2Client, gid: &Gid) -> dman_aria2::Result<()> {
    match client.pause(gid).await {
        Ok(_) => Ok(()),
        Err(e) if is_rpc_msg(&e, "cannot be paused now") => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            match client.pause(gid).await {
                Ok(_) => Ok(()),
                Err(_) => client.force_pause(gid).await.map(drop),
            }
        }
        Err(e) => Err(e),
    }
}

fn gid_of(st: &Status) -> Option<String> {
    st.gid.as_ref().map(|g| g.as_str().to_owned())
}

/// Resolves a requested target directory (tilde-expanded, must be absolute).
fn resolve_dir(requested: Option<&str>, default: &Path) -> Result<PathBuf, RpcError> {
    match requested {
        None => Ok(default.to_owned()),
        Some(d) => {
            let p = dman_config::expand_tilde(d);
            if p.is_absolute() {
                Ok(p)
            } else {
                Err(RpcError::invalid(format!(
                    "dir `{d}` must be an absolute path"
                )))
            }
        }
    }
}

/// Overlays the fields set in `new` onto `base`.
fn merge_options(base: &JobOptions, new: &JobOptions) -> JobOptions {
    let mut m = base.clone();
    macro_rules! opt {
        ($($f:ident),*) => { $( if new.$f.is_some() { m.$f = new.$f.clone(); } )* };
    }
    opt!(
        dir,
        out,
        max_download_limit,
        max_upload_limit,
        select_files,
        seed_ratio,
        seed_time,
        sequential,
        user_agent,
        referer,
        proxy,
        checksum,
        connections_per_server,
        split
    );
    if !new.file_names.is_empty() {
        m.file_names = new.file_names.clone();
    }
    if !new.headers.is_empty() {
        m.headers = new.headers.clone();
    }
    for (k, v) in &new.engine_options {
        m.engine_options.insert(k.clone(), v.clone());
    }
    m
}

/// Recorded file paths of a job (`extra.files`).
pub(crate) fn recorded_files(rec: &JobRecord) -> Vec<PathBuf> {
    rec.extra
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Paths of `files` as reported by aria2 (empty paths skipped).
pub(crate) fn file_paths(files: &[dman_aria2::File]) -> Vec<String> {
    files
        .iter()
        .filter_map(|f| f.path.clone())
        .filter(|p| !p.is_empty() && !p.starts_with("[METADATA]"))
        .collect()
}

/// Waits until aria2 has really stopped `gid` (removal is asynchronous for
/// active downloads), up to a few seconds.
async fn wait_stopped(client: &Aria2Client, gid: &Gid) {
    for _ in 0..50 {
        match client.tell_status(gid, &["status"]).await {
            Ok(st) => {
                if matches!(
                    st.status,
                    Some(
                        DownloadStatus::Removed | DownloadStatus::Complete | DownloadStatus::Error
                    )
                ) {
                    return;
                }
            }
            Err(_) => return,
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Manager {
    /// Spawns the actor and returns its handle and join handle.
    pub fn spawn(cfg: ManagerConfig) -> (ManagerHandle, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(64);
        let default_dir = cfg.profile.download_path();
        let mgr = Manager {
            store: cfg.store,
            profile: cfg.profile,
            default_dir,
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
        };
        let task = tokio::spawn(mgr.run(rx, cfg.engine));
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
                for rec in records {
                    if let Some(g) = &rec.gid {
                        self.gids.insert(g.clone(), rec.id);
                    }
                    self.jobs.insert(rec.id, Entry::new(rec));
                }
                tracing::info!(jobs = self.jobs.len(), "loaded job records");
            }
            Err(e) => tracing::error!("cannot load job records: {}", e.message),
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Msg>, mut engine: watch::Receiver<EngineState>) {
        self.load().await;
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
                        self.flush(true).await;
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.flush(true).await;
                        return;
                    }
                },
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
                _ = interval.tick() => self.tick().await,
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
                let limits =
                    dman_aria2::global_limits(self.limits.download, self.limits.upload, None);
                if let Err(e) = client.change_global_option(&limits).await {
                    tracing::warn!("cannot apply global limits: {e}");
                }
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

    // ----- views -------------------------------------------------------------

    fn positions(&self) -> HashMap<JobId, u32> {
        let waiting: Vec<(JobId, u64)> = self
            .jobs
            .values()
            .filter(|e| e.rec.state == JobState::Waiting)
            .map(|e| (e.rec.id, e.rec.queue_key))
            .collect();
        queue::queue_positions(&waiting)
    }

    fn job_view(e: &Entry, positions: &HashMap<JobId, u32>) -> Job {
        let mut j = e.rec.to_job(Some(&e.live));
        j.queue_position = positions.get(&e.rec.id).copied();
        j
    }

    fn all_jobs(&self) -> Vec<Job> {
        let pos = self.positions();
        let mut v: Vec<Job> = self
            .jobs
            .values()
            .map(|e| Self::job_view(e, &pos))
            .collect();
        v.sort_by_key(|j| j.id);
        v
    }

    fn stats(&self) -> GlobalStats {
        let mut s = GlobalStats {
            download_speed: self.global_speed.0,
            upload_speed: self.global_speed.1,
            total_downloaded: self.totals.0.saturating_add(self.pending_transfer.0),
            total_uploaded: self.totals.1.saturating_add(self.pending_transfer.1),
            disk_free: disk::space(&self.default_dir).map(|s| s.free),
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

    // ----- tick --------------------------------------------------------------

    /// One poll: refresh every job from aria2, run the scheduler, publish
    /// changes and statistics, persist.
    pub(crate) async fn tick(&mut self) {
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
    async fn poll(&mut self, client: &Aria2Client) -> dman_aria2::Result<()> {
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
    async fn apply(&mut self, client: &Aria2Client, statuses: Vec<Status>) {
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
                if running && !following_known && st.belongs_to.is_none() {
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

    fn apply_details(&mut self, details: Vec<(JobId, String, Status)>) {
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

    // ----- scheduler ---------------------------------------------------------

    /// Queue scheduler (D5) with disk guard (Q3).
    async fn schedule(&mut self, client: &Aria2Client) {
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
        // Too many running (aria2 resumed downloads by itself): preempt.
        let running: Vec<(JobId, u64)> = self
            .jobs
            .values()
            .filter(|e| e.rec.desired == Desired::Queued && e.uses_slot())
            .filter(|e| !to_pause.iter().any(|(id, _)| *id == e.rec.id))
            .map(|e| (e.rec.id, e.rec.queue_key))
            .collect();
        for id in queue::plan_preempt(self.limits.max_concurrent, &running) {
            to_pause.push((id, None));
        }
        for (id, msg) in to_pause {
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
        let mut candidates: Vec<(u64, JobId, String)> = self
            .jobs
            .values()
            .filter(|e| {
                e.rec.desired == Desired::Queued
                    && !e.rec.state.is_stopped()
                    && e.aria2 == Some(DownloadStatus::Paused)
                    && e.rec.gid.is_some()
            })
            .map(|e| (e.rec.queue_key, e.rec.id, e.rec.dir.clone()))
            .collect();
        candidates.sort();
        let ids: Vec<JobId> = candidates.iter().map(|c| c.1).collect();
        let dirs: HashMap<JobId, String> = candidates.into_iter().map(|c| (c.1, c.2)).collect();
        let plan = queue::plan_starts(self.limits.max_concurrent, running, &ids, |id| {
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
            Request::Stats => to_value(self.stats()),
            Request::EngineStatus => to_value(vec![self.engine.clone()]),
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

    async fn set_limits(&mut self, u: LimitsUpdate) -> Result<Limits, RpcError> {
        if u.max_concurrent == Some(0) {
            return Err(RpcError::invalid("max_concurrent must be at least 1"));
        }
        let mut next = self.limits.clone();
        if let Some(d) = u.download {
            next.download = d;
        }
        if let Some(up) = u.upload {
            next.upload = up;
        }
        if let Some(m) = u.max_concurrent {
            next.max_concurrent = m;
        }
        if (next.download, next.upload) != (self.limits.download, self.limits.upload) {
            let client = self
                .client
                .clone()
                .ok_or_else(|| unavailable(self.engine.reason.as_deref().unwrap_or("down")))?;
            client
                .change_global_option(&dman_aria2::global_limits(next.download, next.upload, None))
                .await
                .map_err(engine_err)?;
        }
        self.limits = next.clone();
        self.emit(Event::Limits {
            limits: next.clone(),
        });
        Ok(next)
    }

    // ----- add ---------------------------------------------------------------

    async fn add(&mut self, p: AddParams) -> Result<Vec<AddOutcome>, RpcError> {
        let client = self
            .client
            .clone()
            .ok_or_else(|| unavailable(self.engine.reason.as_deref().unwrap_or("not running")))?;
        if p.sources.is_empty() {
            return Err(RpcError::invalid("no sources given"));
        }
        let mut aopts =
            job_options_to_aria2(&p.options).map_err(|e| RpcError::invalid(e.to_string()))?;
        let dir = resolve_dir(p.options.dir.as_deref(), &self.default_dir)?;
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            RpcError::new(
                ErrorCode::General,
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
        let dir_s = dir.display().to_string();
        aopts.set("dir", dir_s.clone());
        aopts.set("pause", "true");
        let mut options = p.options.clone();
        options.dir = Some(dir_s.clone());
        let mut out = Vec::with_capacity(p.sources.len());
        for src in p.sources {
            let r = self
                .add_one(&client, src, &aopts, &options, &dir_s, p.paused)
                .await;
            out.push(match r {
                Ok(id) => AddOutcome::Added { id },
                Err(error) => AddOutcome::Failed { error },
            });
        }
        Ok(out)
    }

    async fn add_one(
        &mut self,
        client: &Aria2Client,
        src: AddSource,
        aopts: &Options,
        options: &JobOptions,
        dir: &str,
        paused: bool,
    ) -> Result<JobId, RpcError> {
        let (kind, name, source, uris, blob) = match src {
            AddSource::Uris { uris } => {
                let uris: Vec<String> = uris
                    .into_iter()
                    .map(|u| u.trim().to_owned())
                    .filter(|u| !u.is_empty())
                    .collect();
                let first = uris
                    .first()
                    .cloned()
                    .ok_or_else(|| RpcError::invalid("empty URI list"))?;
                let mut kind = None;
                for u in &uris {
                    if u.contains(['\n', '\r', '\0']) {
                        return Err(RpcError::invalid("URI contains a line break"));
                    }
                    let k = tick::kind_of_uri(u).ok_or_else(|| {
                        RpcError::invalid(format!(
                            "unsupported URI `{u}` (use http, https, ftp, sftp or magnet)"
                        ))
                    })?;
                    if (k == SourceKind::Magnet || kind == Some(SourceKind::Magnet))
                        && uris.len() > 1
                    {
                        return Err(RpcError::invalid(
                            "a magnet link cannot be combined with other URIs",
                        ));
                    }
                    kind.get_or_insert(k);
                }
                let kind = kind.unwrap_or(SourceKind::Http);
                let name = options
                    .out
                    .clone()
                    .unwrap_or_else(|| tick::name_of_uri(&first));
                (kind, name, first, uris, None)
            }
            AddSource::Torrent { name, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64.trim())
                    .map_err(|e| RpcError::invalid(format!("torrent data is not base64: {e}")))?;
                let display = name.strip_suffix(".torrent").unwrap_or(&name).to_owned();
                (SourceKind::Torrent, display, name, Vec::new(), Some(data))
            }
            AddSource::Metalink { name, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64.trim())
                    .map_err(|e| RpcError::invalid(format!("metalink data is not base64: {e}")))?;
                let display = name
                    .strip_suffix(".meta4")
                    .or_else(|| name.strip_suffix(".metalink"))
                    .unwrap_or(&name)
                    .to_owned();
                (SourceKind::Metalink, display, name, Vec::new(), Some(data))
            }
        };

        // Persist first (the store is authoritative): a crash after this point
        // leaves a record without gid, which reconciliation re-adds.
        let (id, key) = self
            .db(|s| Ok((s.next_job_id()?, s.next_queue_key()?)))
            .await?;
        let mut rec = JobRecord {
            id,
            engine: Engine::Aria2,
            kind,
            name,
            dir: dir.to_owned(),
            source,
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
            has_blob: blob.is_some(),
            extra: serde_json::Map::new(),
        };
        if uris.len() > 1 {
            rec.extra.insert("uris".into(), json!(uris));
        }
        {
            let rec = rec.clone();
            let blob = blob.clone();
            self.db(move |s| {
                s.put_job(&rec)?;
                if let Some(b) = blob {
                    s.put_blob(rec.id, &b)?;
                }
                Ok(())
            })
            .await?;
        }

        let added = match (kind, &blob) {
            (SourceKind::Torrent, Some(b)) => client
                .add_torrent(b, &[], aopts, None)
                .await
                .map(|g| vec![g]),
            (SourceKind::Metalink, Some(b)) => client.add_metalink(b, aopts, None).await,
            _ => client.add_uri(&uris, aopts, None).await.map(|g| vec![g]),
        };
        let gids = match added {
            Ok(g) if !g.is_empty() => g,
            Ok(_) => {
                let _ = self.db(move |s| s.purge(&[id])).await;
                return Err(RpcError::new(ErrorCode::General, "aria2 added nothing"));
            }
            Err(e) => {
                let _ = self.db(move |s| s.purge(&[id])).await;
                return Err(match e {
                    dman_aria2::Error::Rpc { message, .. } => {
                        RpcError::invalid(format!("aria2 rejected the source: {message}"))
                    }
                    other => engine_err(other),
                });
            }
        };

        // One job per gid (metalinks may describe several files).
        let mut records = Vec::with_capacity(gids.len());
        for (i, gid) in gids.iter().enumerate() {
            let mut r = rec.clone();
            if i > 0 {
                let (nid, nkey) = self
                    .db(|s| Ok((s.next_job_id()?, s.next_queue_key()?)))
                    .await?;
                r.id = nid;
                r.queue_key = nkey;
            }
            if gids.len() > 1 {
                r.extra.insert("metalink_index".into(), json!(i));
            }
            r.gid = Some(gid.as_str().to_owned());
            records.push(r);
        }
        rec = records[0].clone();
        {
            let recs = records.clone();
            let blob = blob.clone();
            self.db(move |s| {
                s.put_jobs(&recs)?;
                if let Some(b) = blob {
                    for r in recs.iter().skip(1) {
                        s.put_blob(r.id, &b)?;
                    }
                }
                Ok(())
            })
            .await?;
        }
        for r in records {
            let gid = r.gid.clone().unwrap_or_default();
            tracing::info!(id = %r.id, gid = %gid, kind = ?r.kind, "job added");
            let mut e = Entry::new(r);
            e.aria2 = Some(DownloadStatus::Paused);
            e.counters = Some(Counters::fresh(&gid));
            self.gids.insert(gid, e.rec.id);
            self.jobs.insert(e.rec.id, e);
        }
        Ok(rec.id)
    }

    // ----- pause / resume ----------------------------------------------------

    async fn pause(&mut self, ids: &[JobId]) -> BatchResult {
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

    async fn pause_one(&mut self, id: JobId) -> Result<(), RpcError> {
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

    async fn resume(&mut self, ids: &[JobId]) -> BatchResult {
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

    async fn resume_one(&mut self, id: JobId) -> Result<(), RpcError> {
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
            JobState::Error => {
                // Retry: re-add from the stored source (aria2 continues partial files).
                let client = self.client.clone().ok_or_else(|| {
                    unavailable(self.engine.reason.as_deref().unwrap_or("not running"))
                })?;
                let prev = (e.rec.desired, e.rec.state, e.rec.error.take());
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

    // ----- remove / purge ----------------------------------------------------

    async fn remove(&mut self, p: RemoveParams) -> BatchResult {
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

    async fn remove_one(
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

    async fn purge(&mut self, ids: Vec<JobId>) -> BatchResult {
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

    // ----- reorder / options -------------------------------------------------

    async fn reorder(&mut self, p: ReorderParams) -> Result<(), RpcError> {
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

    async fn set_options(&mut self, p: SetOptionsParams) -> Result<(), RpcError> {
        let e = self
            .jobs
            .get(&p.id)
            .ok_or_else(|| RpcError::not_found(format!("job {}", p.id)))?;
        if e.rec.state.is_stopped() {
            return Err(RpcError::refused(format!("job {} is finished", p.id)));
        }
        let mut delta = p.options.clone();
        if let Some(d) = &delta.dir {
            let resolved = resolve_dir(Some(d), &self.default_dir)?;
            delta.dir = Some(resolved.display().to_string());
        }
        let merged = merge_options(&e.rec.options, &delta);
        job_options_to_aria2(&merged).map_err(|e| RpcError::invalid(e.to_string()))?;
        let changes = job_options_to_aria2(&delta).map_err(|e| RpcError::invalid(e.to_string()))?;
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

/// Applies one aria2 status to an entry. Returns `(downloaded, uploaded,
/// state_transition)`.
fn update_entry(e: &mut Entry, st: &Status, gid: &str, now: i64) -> (u64, u64, bool) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_merge_overlays_set_fields() {
        let base = JobOptions {
            max_download_limit: Some(5),
            out: Some("a".into()),
            ..Default::default()
        };
        let new = JobOptions {
            max_download_limit: Some(9),
            ..Default::default()
        };
        let m = merge_options(&base, &new);
        assert_eq!(m.max_download_limit, Some(9));
        assert_eq!(m.out.as_deref(), Some("a"));
    }

    #[test]
    fn dirs_must_be_absolute() {
        let d = Path::new("/dl");
        assert_eq!(resolve_dir(None, d).unwrap(), PathBuf::from("/dl"));
        assert_eq!(resolve_dir(Some("/x/y"), d).unwrap(), PathBuf::from("/x/y"));
        assert!(resolve_dir(Some("rel"), d).is_err());
    }
}
