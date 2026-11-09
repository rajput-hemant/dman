//! `GET {base}/events`: Server-Sent Events for the htmx SSE extension.
//!
//! Named events, all carrying ready-to-swap HTML:
//!
//! * `stats`: inner HTML of the stats line (only when it changed).
//! * `job-<id>`: the new `<tr>` of one download (outer swap); only changed rows.
//! * `jobs-changed`: a job appeared, vanished or moved to another section; the
//!   table re-fetches itself. Sent instead of rows when the layout changed.
//! * `message`: a status line or a finished-download notice.
//! * `search`: provider chips and the cumulative result table of the watched search.
//!
//! Daemon events are coalesced into batches of at most one per 500 ms (two updates
//! per second per client). `?rows=1` subscribes to job rows, `?search=<id>` to a search.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{RawQuery, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use dman_proto::method::{self, Event, Level, ListParams, Outcome};
use dman_proto::search::{self, ProviderStatus, SearchResultDto, SearchSnapshot};
use dman_proto::{GlobalStats, Job, JobId, JobState};
use futures_util::Stream;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::Instant;

use crate::AppState;
use crate::views::{self, SearchView, Section};

/// Minimum spacing between pushes to one client.
const WINDOW: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Batch {
    jobs: BTreeMap<JobId, Job>,
    removed: Vec<JobId>,
    stats: Option<GlobalStats>,
    messages: Vec<(Level, String)>,
    search_changed: bool,
    resync: bool,
}

struct Feed {
    st: Arc<AppState>,
    rx: Receiver<Event>,
    rows: bool,
    search: Option<u64>,
    known: HashMap<JobId, Section>,
    last_stats: String,
    providers: BTreeMap<String, ProviderStatus>,
    results: Vec<SearchResultDto>,
    finished: bool,
    queue: VecDeque<SseEvent>,
    started: bool,
    closed: bool,
}

fn named(name: &str, html: maud::Markup) -> SseEvent {
    SseEvent::default().event(name).data(html.into_string())
}

impl Feed {
    async fn init(&mut self) {
        if let Ok(s) = self.st.call::<GlobalStats>(method::STATS_GET, ()).await {
            self.push_stats(&s);
        }
        if self.rows {
            self.resync_jobs(true).await;
        }
        if let Some(id) = self.search
            && let Ok(snap) = self
                .st
                .call::<SearchSnapshot>(
                    search::SEARCH_RESULTS,
                    search::SearchIdParams { search_id: id },
                )
                .await
        {
            self.providers = snap.providers.into_iter().collect();
            self.results = snap.results;
            self.finished = snap.finished;
            self.push_search();
        }
    }

    /// Re-reads the job list: pushes live rows and announces layout changes.
    async fn resync_jobs(&mut self, initial: bool) {
        let Ok(jobs) = self
            .st
            .call::<Vec<Job>>(method::JOBS_LIST, ListParams::default())
            .await
        else {
            return;
        };
        let now: HashMap<JobId, Section> = jobs
            .iter()
            .filter_map(|j| views::section_of(j).map(|s| (j.id, s)))
            .collect();
        let changed = !initial && now != self.known;
        self.known = now;
        if changed {
            self.queue
                .push_back(SseEvent::default().event("jobs-changed").data("1"));
            return;
        }
        for j in jobs.iter().filter(|j| {
            matches!(
                j.state,
                JobState::Active | JobState::Seeding | JobState::Waiting
            )
        }) {
            self.queue.push_back(named(
                &format!("job-{}", j.id.0),
                views::job_row(&self.st.base, j),
            ));
        }
    }

    fn push_stats(&mut self, s: &GlobalStats) {
        let html = views::stats_inner(s).into_string();
        if html != self.last_stats {
            self.queue
                .push_back(SseEvent::default().event("stats").data(&html));
            self.last_stats = html;
        }
    }

    fn push_search(&mut self) {
        let providers: Vec<(String, ProviderStatus)> = self
            .providers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let view = SearchView {
            search_id: self.search,
            finished: self.finished,
            providers: &providers,
            results: &self.results,
        };
        self.queue
            .push_back(named("search", views::search_results(&self.st.base, &view)));
    }

    fn absorb(b: &mut Batch, ev: Event) {
        match ev {
            Event::Snapshot { .. } => b.resync = true,
            Event::JobsUpdated { jobs } => {
                for j in jobs {
                    b.removed.retain(|i| *i != j.id);
                    b.jobs.insert(j.id, j);
                }
            }
            Event::JobsRemoved { ids } => {
                for id in ids {
                    b.jobs.remove(&id);
                    b.removed.push(id);
                }
            }
            Event::Stats { stats } => b.stats = Some(stats),
            Event::Message { level, text } => b.messages.push((level, text)),
            Event::JobFinished { name, outcome, .. } => {
                let (level, text) = match outcome {
                    Outcome::Completed => (Level::Info, format!("Completed: {name}")),
                    Outcome::SeedCompleted => (Level::Info, format!("Seeding finished: {name}")),
                    Outcome::Failed { error } => (Level::Error, format!("Failed: {name}: {error}")),
                };
                b.messages.push((level, text));
            }
            _ => {}
        }
    }

    async fn next_batch(&mut self) {
        let mut b = Batch::default();
        match self.rx.recv().await {
            Ok(ev) => self.take(&mut b, ev),
            Err(RecvError::Lagged(_)) => b.resync = true,
            Err(RecvError::Closed) => {
                self.closed = true;
                return;
            }
        }
        let deadline = Instant::now() + WINDOW;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Ok(ev)) => self.take(&mut b, ev),
                Ok(Err(RecvError::Lagged(_))) => b.resync = true,
                Ok(Err(RecvError::Closed)) => {
                    self.closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        self.apply(b).await;
    }

    /// Folds one event into the batch; search events update the feed's search state.
    fn take(&mut self, b: &mut Batch, ev: Event) {
        match ev {
            Event::SearchUpdate {
                search_id,
                provider,
                status,
                results,
            } if Some(search_id) == self.search => {
                self.providers.insert(provider, status);
                self.results = results;
                b.search_changed = true;
            }
            Event::SearchFinished { search_id } if Some(search_id) == self.search => {
                self.finished = true;
                b.search_changed = true;
            }
            other => Self::absorb(b, other),
        }
    }

    async fn apply(&mut self, b: Batch) {
        if let Some(s) = &b.stats {
            self.push_stats(s);
        }
        for (level, text) in &b.messages {
            let l = match level {
                Level::Info => "info",
                Level::Warn => "warn",
                Level::Error => "error",
            };
            self.queue
                .push_back(named("message", views::toast(l, text)));
        }
        if b.search_changed {
            self.push_search();
        }
        if !self.rows {
            return;
        }
        if b.resync {
            self.resync_jobs(false).await;
            return;
        }
        let mut layout = false;
        for id in &b.removed {
            layout |= self.known.remove(id).is_some();
        }
        let mut rows = Vec::new();
        for (id, job) in &b.jobs {
            match (self.known.get(id).copied(), views::section_of(job)) {
                (old, None) => layout |= old.is_some() && self.known.remove(id).is_some(),
                (Some(old), Some(new)) if old == new => rows.push(job),
                (_, Some(new)) => {
                    self.known.insert(*id, new);
                    layout = true;
                }
            }
        }
        if layout {
            self.queue
                .push_back(SseEvent::default().event("jobs-changed").data("1"));
        } else {
            for job in rows {
                self.queue.push_back(named(
                    &format!("job-{}", job.id.0),
                    views::job_row(&self.st.base, job),
                ));
            }
        }
    }
}

pub async fn events(
    State(st): State<Arc<AppState>>,
    RawQuery(raw): RawQuery,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let (mut rows, mut search) = (false, None);
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes()) {
        match k.as_ref() {
            "rows" => rows = v == "1",
            "search" => search = v.parse().ok(),
            _ => {}
        }
    }
    // Subscribe before reading initial state so no update is lost in between.
    let rx = st.backend.subscribe();
    let feed = Feed {
        st,
        rx,
        rows,
        search,
        known: HashMap::new(),
        last_stats: String::new(),
        providers: BTreeMap::new(),
        results: Vec::new(),
        finished: false,
        queue: VecDeque::new(),
        started: false,
        closed: false,
    };
    let stream = futures_util::stream::unfold(feed, |mut f| async move {
        loop {
            if let Some(e) = f.queue.pop_front() {
                return Some((Ok(e), f));
            }
            if f.closed {
                return None;
            }
            if f.started {
                f.next_batch().await;
            } else {
                f.started = true;
                f.init().await;
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new())
}
