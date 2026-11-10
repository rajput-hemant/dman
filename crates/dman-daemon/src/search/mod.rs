//! Search service: provider management, background searches and the tracker
//! list. It owns a [`Searcher`] and runs entirely outside the job manager
//! actor, so a slow or hanging provider can never delay a download.
//!
//! A search is a background task. It publishes [`Event::SearchUpdate`] per
//! provider status change (throttled to one batch per 250 ms, each carrying
//! the cumulative merged list) and ends with [`Event::SearchFinished`]. The
//! latest results of the last five searches stay in memory
//! (`search.results`); the query history is persisted in the store.

mod providers;
pub mod trackers;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use dman_config::{Config, Paths, Secrets};
use dman_proto::method::Event;
use dman_proto::search::{
    self as api, MAX_RESULTS, ProviderStatus, SearchHistoryEntry, SearchResultDto, SearchSnapshot,
    SearchStartParams, SearchStarted,
};
use dman_proto::{ErrorCode, RpcError};
use dman_search::{Aggregator, ProviderError, Query, SearchResult, Searcher, SearcherConfig};
use dman_store::{SearchHistoryRecord, Store};
use futures_util::StreamExt as _;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast};
use tokio::time::Instant;

pub use trackers::{Trackers, merge_trackers};

/// Minimum time between two batches of `search.update` events of one search.
pub const THROTTLE: Duration = Duration::from_millis(250);
/// Searches whose results are kept in memory.
const KEEP_SEARCHES: usize = 5;

struct SearchState {
    id: u64,
    query: String,
    category: Option<String>,
    finished: bool,
    providers: Vec<(String, ProviderStatus)>,
    results: Vec<SearchResultDto>,
    cancel: Arc<Notify>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    searches: VecDeque<SearchState>,
}

/// The search service shared by the API handlers.
pub struct SearchService {
    /// Replaced as a whole when `config.reload` changes the proxy.
    searcher: RwLock<Searcher>,
    paths: Paths,
    /// Released by [`SearchService::close`] so the database can be reopened
    /// right after the daemon returned.
    store: Mutex<Option<Store>>,
    events: broadcast::Sender<Event>,
    history_size: AtomicUsize,
    /// The running tracker-list refresher.
    tracker_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    state: Mutex<State>,
    /// Every running search task (also those evicted from the kept list), so
    /// shutdown can cancel and await them.
    running: Mutex<Vec<(Arc<Notify>, tokio::task::JoinHandle<()>)>>,
    /// Serialises read-modify-write cycles of `config.toml`.
    config_lock: tokio::sync::Mutex<()>,
    trackers: Arc<Trackers>,
}

/// A validated search reload, see [`SearchService::prepare_reload`].
pub struct PreparedSearch {
    searcher: Searcher,
    history_size: usize,
    tracker_list: dman_config::TrackerList,
}

/// Whether `method` is served by [`SearchService::call`].
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        api::SEARCH_START
            | api::SEARCH_CANCEL
            | api::SEARCH_RESULTS
            | api::SEARCH_RESOLVE
            | api::SEARCH_HISTORY
            | api::SEARCH_HISTORY_CLEAR
            | api::PROVIDERS_LIST
            | api::PROVIDERS_SET
            | api::PROVIDERS_TEST
            | api::PROVIDERS_IMPORT
            | api::PROVIDERS_EXPORT
            | api::PROVIDERS_REMOVE
            | api::TRACKERS_STATUS
    )
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| RpcError::invalid(format!("invalid params: {e}")))
}

fn value<T: Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))
}

fn provider_err(e: &ProviderError) -> RpcError {
    let code = match e {
        ProviderError::Unknown(_) => ErrorCode::NotFound,
        ProviderError::Definition(_) | ProviderError::MissingSecret(_) => ErrorCode::InvalidInput,
        _ => ErrorCode::General,
    };
    RpcError::new(code, e.to_string())
}

pub(crate) fn dto(r: SearchResult) -> SearchResultDto {
    SearchResultDto {
        title: r.title,
        size: r.size,
        seeders: r.seeders,
        leechers: r.leechers,
        date: r.date,
        magnet: r.magnet,
        torrent_url: r.torrent_url,
        detail_url: r.detail_url,
        info_hash: r.info_hash,
        sources: r.sources,
        category: r.category,
    }
}

impl SearchService {
    /// Builds the searcher from `config.search` (providers, proxy) and hands it
    /// the API keys named by `api_key_ref` from `secrets`.
    ///
    /// # Errors
    /// Invalid provider definitions or proxy.
    pub fn new(
        paths: Paths,
        store: Store,
        events: broadcast::Sender<Event>,
        config: &Config,
        secrets: &Secrets,
    ) -> anyhow::Result<Arc<Self>> {
        let searcher = Searcher::new(SearcherConfig {
            proxy: config.search.proxy.clone(),
            user_agent: None,
        })
        .map_err(|e| anyhow::anyhow!("search: {e}"))?;
        let defs = config
            .search
            .effective_providers()
            .map_err(|e| anyhow::anyhow!(e))?;
        providers::apply_secrets(&searcher, secrets, &defs);
        searcher.set_providers(defs);
        Ok(Arc::new(Self {
            searcher: RwLock::new(searcher),
            paths,
            store: Mutex::new(Some(store)),
            events,
            history_size: AtomicUsize::new(config.search.history_size),
            tracker_task: Mutex::new(None),
            state: Mutex::new(State::default()),
            running: Mutex::new(Vec::new()),
            config_lock: tokio::sync::Mutex::new(()),
            trackers: Arc::new(Trackers::new(config.search.tracker_list.clone())),
        }))
    }

    /// The tracker list shared with the job manager.
    pub fn trackers(&self) -> Arc<Trackers> {
        self.trackers.clone()
    }

    /// The current searcher (cheap clone).
    pub(crate) fn searcher(&self) -> Searcher {
        self.searcher
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn history_size(&self) -> usize {
        self.history_size.load(Ordering::Relaxed)
    }

    /// Starts fetching the tracker list (nothing when it is disabled); the
    /// service owns the task and stops it on [`SearchService::close`].
    pub fn spawn_tracker_refresher(&self) {
        let task = self
            .trackers
            .refresher(self.searcher(), self.paths.data_dir().join("trackers.txt"));
        let old = std::mem::replace(
            &mut *self.tracker_task.lock().unwrap_or_else(|e| e.into_inner()),
            task,
        );
        if let Some(t) = old {
            t.abort();
        }
    }

    /// Validates the provider, proxy and tracker-list settings of `config`
    /// without applying them.
    ///
    /// # Errors
    /// Invalid provider definitions or proxy.
    pub fn prepare_reload(
        &self,
        config: &Config,
        secrets: &Secrets,
    ) -> anyhow::Result<PreparedSearch> {
        let searcher = Searcher::new(SearcherConfig {
            proxy: config.search.proxy.clone(),
            user_agent: None,
        })
        .map_err(|e| anyhow::anyhow!("search: {e}"))?;
        let defs = config
            .search
            .effective_providers()
            .map_err(|e| anyhow::anyhow!(e))?;
        providers::apply_secrets(&searcher, secrets, &defs);
        searcher.set_providers(defs);
        Ok(PreparedSearch {
            searcher,
            history_size: config.search.history_size,
            tracker_list: config.search.tracker_list.clone(),
        })
    }

    /// Applies a prepared reload: new searcher (providers, proxy), history
    /// size and tracker-list settings (the refresher is restarted).
    pub fn apply_reload(&self, p: PreparedSearch) {
        *self.searcher.write().unwrap_or_else(|e| e.into_inner()) = p.searcher;
        self.history_size.store(p.history_size, Ordering::Relaxed);
        self.trackers.set_config(p.tracker_list);
        self.spawn_tracker_refresher();
    }

    /// Cancels running searches, waits for them (their history entry is
    /// still saved) and releases the store (daemon shutdown).
    pub async fn close(&self) {
        if let Some(t) = self
            .tracker_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            t.abort();
        }
        let running = std::mem::take(&mut *self.running.lock().unwrap_or_else(|e| e.into_inner()));
        for (cancel, _) in &running {
            cancel.notify_one();
        }
        for (_, task) in running {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
        self.store.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> dman_store::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let s = self
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| RpcError::new(ErrorCode::General, "the daemon is shutting down"))?;
        tokio::task::spawn_blocking(move || f(&s))
            .await
            .map_err(|e| RpcError::new(ErrorCode::General, e.to_string()))?
            .map_err(|e| RpcError::new(ErrorCode::General, format!("store: {e}")))
    }

    /// Dispatches one search, provider or tracker method.
    ///
    /// # Errors
    /// An [`RpcError`] for invalid params, unknown ids/providers and failures.
    pub async fn call(self: &Arc<Self>, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            api::SEARCH_START => value(self.start(parse(params)?)?),
            api::SEARCH_CANCEL => {
                self.cancel(parse::<api::SearchIdParams>(params)?.search_id)?;
                Ok(Value::Null)
            }
            api::SEARCH_RESULTS => {
                value(self.snapshot(parse::<api::SearchIdParams>(params)?.search_id)?)
            }
            api::SEARCH_RESOLVE => value(self.resolve(parse(params)?).await?),
            api::SEARCH_HISTORY => value(self.history().await?),
            api::SEARCH_HISTORY_CLEAR => {
                self.db(Store::clear_search_history).await?;
                Ok(Value::Null)
            }
            api::PROVIDERS_LIST => value(self.providers_list()),
            api::PROVIDERS_SET => value(self.providers_set(parse(params)?).await?),
            api::PROVIDERS_TEST => value(self.providers_test(parse(params)?).await?),
            api::PROVIDERS_IMPORT => value(self.providers_import(parse(params)?).await?),
            api::PROVIDERS_EXPORT => value(self.providers_export(parse(params)?)?),
            api::PROVIDERS_REMOVE => {
                self.providers_remove(parse(params)?).await?;
                Ok(Value::Null)
            }
            api::TRACKERS_STATUS => value(self.trackers.status()),
            other => Err(RpcError::new(
                ErrorCode::General,
                format!("not a search method: {other}"),
            )),
        }
    }

    // ----- searching ---------------------------------------------------------

    fn start(self: &Arc<Self>, p: SearchStartParams) -> Result<SearchStarted, RpcError> {
        let text = p.query.trim().to_owned();
        if text.is_empty() {
            return Err(RpcError::invalid("the query is empty"));
        }
        let requested: Option<Vec<String>> = p
            .providers
            .map(|v| {
                v.into_iter()
                    .map(|n| n.trim().to_owned())
                    .filter(|n| !n.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());
        let category = p
            .category
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        let query = Query {
            text: text.clone(),
            category: category.clone(),
            providers: requested.clone(),
        };

        let defs = self.searcher().providers();
        let mut names: Vec<String> = defs
            .iter()
            .filter(|d| d.enabled)
            .filter(|d| {
                requested
                    .as_ref()
                    .is_none_or(|r| r.iter().any(|n| n.eq_ignore_ascii_case(&d.name)))
            })
            .map(|d| d.name.clone())
            .collect();
        for n in requested.iter().flatten() {
            if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                names.push(n.clone());
            }
        }
        // Taken now so the provider set matches `names` even if one changes later.
        let stream = Box::pin(self.searcher().search(&query));

        let cancel = Arc::new(Notify::new());
        let id = {
            let mut st = self.state();
            st.next_id += 1;
            let id = st.next_id;
            st.searches.push_back(SearchState {
                id,
                query: text,
                category,
                finished: false,
                providers: names
                    .iter()
                    .map(|n| (n.clone(), ProviderStatus::Running))
                    .collect(),
                results: Vec::new(),
                cancel: cancel.clone(),
            });
            while st.searches.len() > KEEP_SEARCHES {
                st.searches.pop_front();
            }
            id
        };
        let task =
            tokio::spawn(
                self.clone()
                    .run(id, requested, names.clone(), stream, cancel.clone()),
            );
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.retain(|(_, t)| !t.is_finished());
            running.push((cancel, task));
        }
        Ok(SearchStarted {
            search_id: id,
            providers: names,
        })
    }

    async fn run(
        self: Arc<Self>,
        id: u64,
        requested: Option<Vec<String>>,
        names: Vec<String>,
        mut stream: std::pin::Pin<
            Box<impl futures_util::Stream<Item = dman_search::ProviderEvent>>,
        >,
        cancel: Arc<Notify>,
    ) {
        use dman_search::ProviderEvent as Ev;
        let mut agg = Aggregator::default();
        let mut statuses: Vec<(String, ProviderStatus)> = names
            .iter()
            .map(|n| (n.clone(), ProviderStatus::Running))
            .collect();
        let mut counts: Vec<(String, u32)> = Vec::new();
        let mut pending: Vec<String> = names.clone();
        let mut last_emit: Option<Instant>;
        let mut cancelled = false;

        fn set(statuses: &mut Vec<(String, ProviderStatus)>, name: &str, st: ProviderStatus) {
            match statuses
                .iter_mut()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
            {
                Some(e) => e.1 = st,
                None => statuses.push((name.to_owned(), st)),
            }
        }
        fn mark(pending: &mut Vec<String>, name: &str) {
            if !pending.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                pending.push(name.to_owned());
            }
        }

        // Announce every provider as running before any result can arrive.
        self.flush(id, &agg, &statuses, &mut pending);
        last_emit = Some(Instant::now());

        loop {
            let due = match (last_emit, pending.is_empty()) {
                (Some(t), false) => Some(t + THROTTLE),
                _ => None,
            };
            tokio::select! {
                () = cancel.notified() => { cancelled = true; break; }
                ev = stream.next() => match ev {
                    None => break,
                    Some(Ev::Results { provider, results }) => {
                        match counts.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&provider)) {
                            Some(c) => c.1 += u32::try_from(results.len()).unwrap_or(u32::MAX),
                            None => counts.push((provider.clone(), u32::try_from(results.len()).unwrap_or(u32::MAX))),
                        }
                        agg.add(results);
                        mark(&mut pending, &provider);
                    }
                    Some(Ev::Failed { provider, error }) => {
                        set(&mut statuses, &provider, ProviderStatus::Failed { error });
                        mark(&mut pending, &provider);
                    }
                    Some(Ev::Done { provider, elapsed_ms }) => {
                        let failed = statuses.iter().any(|(n, s)| {
                            n.eq_ignore_ascii_case(&provider) && matches!(s, ProviderStatus::Failed { .. })
                        });
                        if !failed {
                            let count = counts
                                .iter()
                                .find(|(n, _)| n.eq_ignore_ascii_case(&provider))
                                .map_or(0, |c| c.1);
                            set(&mut statuses, &provider, ProviderStatus::Done { count, elapsed_ms });
                        }
                        mark(&mut pending, &provider);
                    }
                },
                () = async { tokio::time::sleep_until(due.unwrap_or_else(Instant::now)).await }, if due.is_some() => {}
            }
            if !pending.is_empty() && last_emit.is_none_or(|t| t.elapsed() >= THROTTLE) {
                self.flush(id, &agg, &statuses, &mut pending);
                last_emit = Some(Instant::now());
            }
        }
        drop(stream); // cancels outstanding requests

        if cancelled {
            for (n, s) in &mut statuses {
                if matches!(s, ProviderStatus::Running) {
                    *s = ProviderStatus::Failed {
                        error: "cancelled".into(),
                    };
                    mark(&mut pending, n);
                }
            }
        }
        self.flush(id, &agg, &statuses, &mut pending);
        {
            let mut st = self.state();
            if let Some(s) = st.searches.iter_mut().find(|s| s.id == id) {
                s.finished = true;
            }
        }
        if !names.is_empty() {
            let (query, category) = self
                .state()
                .searches
                .iter()
                .find(|s| s.id == id)
                .map(|s| (s.query.clone(), s.category.clone()))
                .unwrap_or_default();
            let rec = SearchHistoryRecord {
                query,
                providers: requested.unwrap_or_default(),
                category,
                at: crate::manager::now(),
                result_count: u32::try_from(agg.len()).unwrap_or(u32::MAX),
            };
            let keep = self.history_size();
            if let Err(e) = self.db(move |s| s.record_search(&rec, keep)).await {
                tracing::warn!("cannot save the search history: {}", e.message);
            }
        }
        let _ = self.events.send(Event::SearchFinished { search_id: id });
    }

    /// Stores the merged list and statuses and emits one update per pending provider.
    fn flush(
        &self,
        id: u64,
        agg: &Aggregator,
        statuses: &[(String, ProviderStatus)],
        pending: &mut Vec<String>,
    ) {
        let mut results: Vec<SearchResultDto> = agg.results().into_iter().map(dto).collect();
        results.truncate(MAX_RESULTS);
        {
            let mut st = self.state();
            if let Some(s) = st.searches.iter_mut().find(|s| s.id == id) {
                s.results.clone_from(&results);
                s.providers = statuses.to_vec();
            }
        }
        for provider in pending.drain(..) {
            let Some((_, status)) = statuses
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&provider))
            else {
                continue;
            };
            let _ = self.events.send(Event::SearchUpdate {
                search_id: id,
                provider,
                status: status.clone(),
                results: results.clone(),
            });
        }
    }

    fn cancel(&self, id: u64) -> Result<(), RpcError> {
        let st = self.state();
        let s = st
            .searches
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| not_found(id))?;
        if !s.finished {
            s.cancel.notify_one();
        }
        Ok(())
    }

    fn snapshot(&self, id: u64) -> Result<SearchSnapshot, RpcError> {
        let st = self.state();
        let s = st
            .searches
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| not_found(id))?;
        Ok(SearchSnapshot {
            search_id: s.id,
            query: s.query.clone(),
            category: s.category.clone(),
            finished: s.finished,
            providers: s.providers.clone(),
            results: s.results.clone(),
        })
    }

    async fn resolve(&self, p: api::SearchResolveParams) -> Result<api::ResolvedMagnet, RpcError> {
        let (detail_url, provider) = match (p.index, p.search_id) {
            (Some(index), Some(id)) => {
                let snap = self.snapshot(id)?;
                let r = snap.results.get(index).ok_or_else(|| {
                    RpcError::new(
                        ErrorCode::NotFound,
                        format!("search {id} has no result #{index}"),
                    )
                })?;
                if let Some(m) = &r.magnet {
                    return Ok(api::ResolvedMagnet { magnet: m.clone() });
                }
                let url = r.detail_url.clone().ok_or_else(|| {
                    RpcError::invalid("the result has neither a magnet link nor a detail page")
                })?;
                let provider = p
                    .provider
                    .or_else(|| r.sources.first().cloned())
                    .ok_or_else(|| RpcError::invalid("the result names no provider"))?;
                (url, provider)
            }
            (None, _) => match (p.detail_url, p.provider) {
                (Some(u), Some(pr)) => {
                    self.check_detail_url(&u, &pr)?;
                    (u, pr)
                }
                _ => {
                    return Err(RpcError::invalid(
                        "give `search_id` with `index`, or `detail_url` with `provider`",
                    ));
                }
            },
            (Some(_), None) => return Err(RpcError::invalid("`index` needs `search_id`")),
        };
        let magnet = self
            .searcher()
            .resolve_magnet(&provider, &detail_url)
            .await
            .map_err(|e| provider_err(&e))?;
        Ok(api::ResolvedMagnet { magnet })
    }

    /// A `detail_url` given by a client is only fetched when it belongs to
    /// `provider`: a detail page one of the kept searches returned for it, or a
    /// page on the host of its own search URL. Anything else (internal hosts,
    /// other sites) is refused, so `search.resolve` cannot be used to make the
    /// daemon fetch arbitrary URLs.
    fn check_detail_url(&self, url: &str, provider: &str) -> Result<(), RpcError> {
        let returned = self.state().searches.iter().any(|s| {
            s.results.iter().any(|r| {
                r.detail_url.as_deref() == Some(url)
                    && r.sources.iter().any(|p| p.eq_ignore_ascii_case(provider))
            })
        });
        if returned {
            return Ok(());
        }
        let def = self
            .searcher()
            .providers()
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(provider))
            .ok_or_else(|| RpcError::not_found(format!("provider `{provider}`")))?;
        let refuse = || {
            RpcError::refused(format!(
                "`detail_url` is not a page of provider `{}`",
                def.name
            ))
        };
        let target = url::Url::parse(url).map_err(|_| refuse())?;
        let home = provider_home(&def.kind).ok_or_else(refuse)?;
        let same_host = matches!(target.scheme(), "http" | "https")
            && target
                .host_str()
                .is_some_and(|h| home.host_str().is_some_and(|ph| ph.eq_ignore_ascii_case(h)))
            && target.port() == home.port();
        if same_host { Ok(()) } else { Err(refuse()) }
    }

    async fn history(&self) -> Result<Vec<SearchHistoryEntry>, RpcError> {
        let n = self.history_size();
        let recs = self.db(move |s| s.search_history(n)).await?;
        Ok(recs
            .into_iter()
            .map(|r| SearchHistoryEntry {
                query: r.query,
                providers: r.providers,
                category: r.category,
                at: r.at,
                result_count: r.result_count,
            })
            .collect())
    }
}

fn not_found(id: u64) -> RpcError {
    RpcError::new(
        ErrorCode::NotFound,
        format!("no search {id} (only the last {KEEP_SEARCHES} searches are kept)"),
    )
}

/// The search URL of a provider definition, placeholders filled in.
fn provider_home(kind: &dman_search::def::Kind) -> Option<url::Url> {
    use dman_search::def::Kind;
    let template = match kind {
        Kind::Torznab { url, .. } | Kind::Feed { url, .. } => url,
        Kind::Html(h) => &h.search_url,
        Kind::Json(j) => &j.url,
    };
    url::Url::parse(&dman_search::util::fill_url(template, "x", "x")).ok()
}
