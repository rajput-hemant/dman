//! The search engine: concurrent, isolated, rate-limited provider queries.
//!
//! Typical use:
//!
//! ```no_run
//! use dman_search::{builtin_providers, Aggregator, ProviderEvent, Query, Searcher, SearcherConfig};
//! use futures_util::StreamExt;
//!
//! # async fn demo() -> Result<(), dman_search::ProviderError> {
//! let searcher = Searcher::new(SearcherConfig::default())?;
//! let mut defs = builtin_providers();
//! defs[2].enabled = true; // Nyaa
//! searcher.set_providers(defs);
//! let mut agg = Aggregator::default();
//! let mut stream = Box::pin(searcher.search(&Query::new("ubuntu")));
//! while let Some(event) = stream.next().await {
//!     if let ProviderEvent::Results { results, .. } = event {
//!         agg.add(results);
//!     }
//! }
//! let ranked = agg.results();
//! # let _ = ranked;
//! # Ok(()) }
//! ```

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Once, RwLock},
    time::Duration,
};

use futures_util::{
    Stream, StreamExt,
    future::BoxFuture,
    stream::{self, FuturesUnordered},
};
use reqwest::{Client, Method, RequestBuilder};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use url::Url;

use crate::{
    def::{HtmlSpec, HttpMethod, Kind, ProviderDef, parse_response},
    dns::DnsMode,
    doh::DohResolver,
    error::ProviderError,
    html,
    proxy::{PROXY_NONE, ProxyUrl},
    result::{Query, SearchResult},
    torznab, trackers, util,
};

const MAX_BODY: usize = 10 * 1024 * 1024;
const DETAIL_CONCURRENCY: usize = 3;

static CRYPTO: Once = Once::new();

/// Install the ring crypto provider for rustls (once per process).
/// Already-installed providers are fine; the result is ignored.
pub fn ensure_crypto_provider() {
    CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Searcher settings.
#[derive(Debug, Clone, Default)]
pub struct SearcherConfig {
    /// Proxy for every provider that does not set its own: `http://`,
    /// `https://`, `socks5://` or `socks5h://` URL (credentials stay out of
    /// `Debug`).
    pub proxy: Option<ProxyUrl>,
    /// How host names are resolved (system resolver or DNS over HTTPS). Not
    /// used for requests that go through a proxy, which resolves the names
    /// itself.
    pub dns: DnsMode,
    /// Overrides the default `dman/<version>` user agent.
    pub user_agent: Option<String>,
}

/// One event of a running search. Every provider ends with exactly one `Done`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProviderEvent {
    Results {
        provider: String,
        results: Vec<SearchResult>,
    },
    Failed {
        provider: String,
        error: String,
    },
    Done {
        provider: String,
        elapsed_ms: u64,
        /// Base URL that answered, for providers that have mirrors.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mirror: Option<String>,
    },
}

/// Outcome of a successful provider probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// Number of results parsed (0 is possible for a working provider).
    pub results: usize,
    pub sample: Option<SearchResult>,
    pub elapsed: Duration,
    /// Base URL that answered, for providers that have mirrors.
    pub mirror: Option<String>,
}

// ---------------------------------------------------------- rate limiter

/// Token-bucket style limiter (GCRA): `per_min` requests per minute with a
/// small burst allowance. `acquire` waits for the next slot.
#[derive(Debug)]
pub struct RateLimiter {
    per_min: u32,
    interval: Duration,
    burst: u32,
    origin: Instant,
    tat: Mutex<Duration>,
}

impl RateLimiter {
    pub fn new(per_min: u32) -> Self {
        let per_min = per_min.max(1);
        Self {
            per_min,
            interval: Duration::from_secs_f64(60.0 / f64::from(per_min)),
            burst: per_min.min(3),
            origin: Instant::now(),
            tat: Mutex::new(Duration::ZERO),
        }
    }

    pub fn per_min(&self) -> u32 {
        self.per_min
    }

    /// Wait until a request may be sent.
    pub async fn acquire(&self) {
        let wait = {
            let now = self.origin.elapsed();
            let mut tat = self.tat.lock().unwrap_or_else(|e| e.into_inner());
            let new_tat = (*tat).max(now) + self.interval;
            *tat = new_tat;
            new_tat
                .saturating_sub(self.interval * self.burst)
                .saturating_sub(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

// ------------------------------------------------------------- searcher

struct Inner {
    /// Proxy of providers without their own (`[search] proxy`).
    default_proxy: Option<ProxyUrl>,
    user_agent: String,
    /// DNS-over-HTTPS resolver for direct connections, when configured.
    resolver: Option<Arc<DohResolver>>,
    /// One HTTP client per effective proxy setting.
    clients: Mutex<HashMap<String, Client>>,
    /// Mirror that answered last, per provider (lower-case name).
    last_mirror: Mutex<HashMap<String, String>>,
    providers: RwLock<Vec<ProviderDef>>,
    secrets: RwLock<HashMap<String, SecretString>>,
    limiters: Mutex<HashMap<String, Arc<RateLimiter>>>,
}

/// Runs searches against provider definitions. Cheap to clone.
#[derive(Clone)]
pub struct Searcher {
    inner: Arc<Inner>,
}

fn err_chain(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    msg
}

fn map_reqwest(e: reqwest::Error) -> ProviderError {
    let e = e.without_url();
    ProviderError::Network(err_chain(&e))
}

/// URL without credentials/query/fragment, safe to show (API keys live in the query).
fn redact(url: &str) -> String {
    Url::parse(url).map_or_else(
        |_| "(invalid url)".into(),
        |mut u| {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        },
    )
}

fn build_client(
    user_agent: &str,
    proxy: Option<&ProxyUrl>,
    resolver: Option<&Arc<DohResolver>>,
) -> Result<Client, ProviderError> {
    let mut b = Client::builder()
        .user_agent(user_agent)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5));
    let proxied = match proxy {
        Some(p) if p.is_direct() => {
            b = b.no_proxy();
            false
        }
        Some(p) => {
            p.validate().map_err(ProviderError::Definition)?;
            b = b.proxy(reqwest::Proxy::all(p.as_str().trim()).map_err(|e| {
                ProviderError::Definition(format!("bad proxy `{p}`: {}", e.without_url()))
            })?);
            true
        }
        None => false,
    };
    // Through a proxy, names are the proxy's business; the proxy's own host
    // (often `localhost`) must still resolve the ordinary way.
    if let Some(r) = resolver.filter(|_| !proxied) {
        b = b.dns_resolver(Arc::clone(r));
    }
    b.build().map_err(map_reqwest)
}

/// `url` with scheme, host and port taken from `base`.
fn rebase(url: &Url, base: &Url) -> Url {
    let mut u = url.clone();
    let _ = u.set_scheme(base.scheme());
    let _ = u.set_host(base.host_str());
    let _ = u.set_port(base.port());
    u
}

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Whether a failure says "this host does not work for us", so that the next
/// mirror is worth a try.
fn mirror_retryable(e: &ProviderError) -> bool {
    match e {
        ProviderError::Http { status, .. } => matches!(*status, 403 | 451 | 500..=599),
        ProviderError::Network(_) | ProviderError::Timeout(_) | ProviderError::Blocked(_) => true,
        _ => false,
    }
}

/// A request that can be sent to any of a provider's hosts.
#[derive(Debug, Clone)]
struct Req {
    method: Method,
    url: Url,
    body: Option<String>,
    accept: &'static str,
}

impl Req {
    fn get(url: Url, accept: &'static str) -> Self {
        Self {
            method: Method::GET,
            url,
            body: None,
            accept,
        }
    }
}

/// Keep the results whose title contains every word of `query`
/// (case-insensitive, punctuation ignored).
fn matches_query(title: &str, query: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_lowercase().next().unwrap_or(c)
                } else {
                    ' '
                }
            })
            .collect::<String>()
    };
    let t = norm(title);
    norm(query).split_whitespace().all(|w| t.contains(w))
}

impl Searcher {
    pub fn new(config: SearcherConfig) -> Result<Self, ProviderError> {
        ensure_crypto_provider();
        let user_agent = config
            .user_agent
            .unwrap_or_else(|| format!("dman/{}", env!("CARGO_PKG_VERSION")));
        let default_proxy = config.proxy.filter(|p| !p.as_str().trim().is_empty());
        // Fail early on an unusable proxy setting.
        let resolver = match &config.dns {
            DnsMode::System => None,
            DnsMode::Doh { endpoints, .. } => Some(
                DohResolver::new(&user_agent, endpoints.clone())
                    .map_err(|e| ProviderError::Definition(format!("dns: {e}")))?,
            ),
        };
        let first = build_client(&user_agent, default_proxy.as_ref(), resolver.as_ref())?;
        let key = default_proxy
            .as_ref()
            .map_or_else(String::new, |p| p.as_str().to_owned());
        Ok(Self {
            inner: Arc::new(Inner {
                default_proxy,
                user_agent,
                resolver,
                clients: Mutex::new(HashMap::from([(key, first)])),
                last_mirror: Mutex::new(HashMap::new()),
                providers: RwLock::new(Vec::new()),
                secrets: RwLock::new(HashMap::new()),
                limiters: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Replace the provider list; takes effect for the next search.
    pub fn set_providers(&self, mut defs: Vec<ProviderDef>) {
        defs.sort_by_key(|d| d.order);
        *self
            .inner
            .providers
            .write()
            .unwrap_or_else(|e| e.into_inner()) = defs;
    }

    pub fn providers(&self) -> Vec<ProviderDef> {
        self.inner
            .providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Provide a secret (e.g. a Torznab API key) by the name used in
    /// `api_key_ref`. Secrets are kept in memory only.
    pub fn set_secret(&self, name: &str, value: impl Into<String>) {
        self.inner
            .secrets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), SecretString::from(value.into()));
    }

    /// Search all enabled providers (or those named in the query), concurrently.
    /// Dropping the stream cancels outstanding requests.
    pub fn search(
        &self,
        query: &Query,
    ) -> impl Stream<Item = ProviderEvent> + Send + 'static + use<> {
        self.search_with(self.providers(), query)
    }

    /// Like [`Searcher::search`] with an explicit provider list.
    pub fn search_with(
        &self,
        defs: Vec<ProviderDef>,
        query: &Query,
    ) -> impl Stream<Item = ProviderEvent> + Send + 'static + use<> {
        let jobs: FuturesUnordered<BoxFuture<'static, Vec<ProviderEvent>>> =
            FuturesUnordered::new();
        let mut selected: Vec<ProviderDef> = defs.into_iter().filter(|d| d.enabled).collect();
        selected.sort_by_key(|d| d.order);
        if let Some(names) = &query.providers {
            for n in names {
                if !selected.iter().any(|d| d.name.eq_ignore_ascii_case(n)) {
                    let provider = n.clone();
                    jobs.push(Box::pin(async move {
                        vec![
                            ProviderEvent::Failed {
                                provider: provider.clone(),
                                error: ProviderError::Unknown(provider.clone()).to_string(),
                            },
                            ProviderEvent::Done {
                                provider,
                                elapsed_ms: 0,
                                mirror: None,
                            },
                        ]
                    }));
                }
            }
            selected.retain(|d| names.iter().any(|n| d.name.eq_ignore_ascii_case(n)));
        }
        for def in selected {
            let inner = Arc::clone(&self.inner);
            let q = query.clone();
            jobs.push(Box::pin(async move { run_provider(inner, def, q).await }));
        }
        jobs.flat_map(stream::iter)
    }

    /// Probe a provider with its test query and report precise failures.
    /// Works for disabled providers. Zero results is `Ok` (`results == 0`).
    pub async fn test_provider(&self, def: &ProviderDef) -> Result<TestReport, ProviderError> {
        def.validate()?;
        let start = Instant::now();
        let deadline = start + Duration::from_secs(def.timeout_secs);
        let q = def.probe_query().to_string();
        let (mut results, mirror) = tokio::time::timeout_at(
            deadline,
            self.inner.fetch_primary(def, &q, None, true, deadline),
        )
        .await
        .map_err(|_| ProviderError::Timeout(def.timeout_secs))??;
        if let Kind::Html(h) = &def.kind
            && h.follow_detail_for_magnet
            && let Some(url) = results.iter().find_map(|r| r.detail_url.clone())
        {
            tokio::time::timeout_at(
                deadline,
                self.inner.fetch_detail_magnet(def, h, &url, deadline),
            )
            .await
            .map_err(|_| ProviderError::Timeout(def.timeout_secs))?
            .map_err(|e| match e {
                ProviderError::Parse(m) => ProviderError::Parse(format!("detail page: {m}")),
                other => other,
            })?;
        }
        for r in &mut results {
            r.normalize();
        }
        Ok(TestReport {
            results: results.len(),
            sample: results.into_iter().next(),
            elapsed: start.elapsed(),
            mirror,
        })
    }

    /// Fetch a result's detail page and read its magnet link (for providers
    /// whose rows carry none, e.g. 1337x). `provider` selects the definition.
    pub async fn resolve_magnet(
        &self,
        provider: &str,
        detail_url: &str,
    ) -> Result<String, ProviderError> {
        let def = self
            .providers()
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(provider))
            .ok_or_else(|| ProviderError::Unknown(provider.to_string()))?;
        let Kind::Html(h) = &def.kind else {
            return Err(ProviderError::Definition(
                "provider is not an HTML provider".into(),
            ));
        };
        let deadline = Instant::now() + Duration::from_secs(def.timeout_secs);
        tokio::time::timeout_at(
            deadline,
            self.inner
                .fetch_detail_magnet(&def, h, detail_url, deadline),
        )
        .await
        .map_err(|_| ProviderError::Timeout(def.timeout_secs))?
    }

    /// Download a tracker list (see [`trackers::parse_tracker_list`]).
    pub async fn fetch_tracker_list(&self, url: &str) -> Result<Vec<String>, ProviderError> {
        let u = Url::parse(url)
            .map_err(|e| ProviderError::Definition(format!("bad tracker list url: {e}")))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(ProviderError::Definition(
                "tracker list url must be http or https".into(),
            ));
        }
        let client = self.inner.client_for(None)?;
        let req = client.get(u).timeout(Duration::from_secs(30));
        let body = self.inner.send(req).await?;
        let list = trackers::parse_tracker_list(&body);
        if list.is_empty() {
            return Err(ProviderError::Parse(
                "no valid tracker URLs found in the list".into(),
            ));
        }
        Ok(list)
    }
}

/// Probe a provider with a default (no proxy, no secrets) searcher.
pub async fn test_provider(def: &ProviderDef) -> Result<TestReport, ProviderError> {
    Searcher::new(SearcherConfig::default())?
        .test_provider(def)
        .await
}

async fn run_provider(inner: Arc<Inner>, def: ProviderDef, q: Query) -> Vec<ProviderEvent> {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(def.timeout_secs);
    let provider = def.name.clone();
    let mut answered = None;
    let outcome = async {
        let (mut results, mirror) = tokio::time::timeout_at(
            deadline,
            inner.fetch_primary(&def, &q.text, q.category.as_deref(), false, deadline),
        )
        .await
        .map_err(|_| ProviderError::Timeout(def.timeout_secs))??;
        answered = mirror;
        if let Kind::Html(h) = &def.kind
            && h.follow_detail_for_magnet
        {
            inner.follow_details(&def, h, &mut results, deadline).await;
        }
        Ok::<_, ProviderError>(results)
    }
    .await;

    let mut events = Vec::with_capacity(2);
    match outcome {
        Ok(mut results) => {
            for r in &mut results {
                r.normalize();
            }
            results.retain(|r| !r.title.is_empty());
            results.truncate(def.max_results);
            for r in &mut results {
                r.sources = vec![provider.clone()];
            }
            events.push(ProviderEvent::Results {
                provider: provider.clone(),
                results,
            });
        }
        Err(e) => {
            tracing::debug!(provider = %provider, error = %e, "provider search failed");
            events.push(ProviderEvent::Failed {
                provider: provider.clone(),
                error: e.to_string(),
            });
        }
    }
    events.push(ProviderEvent::Done {
        provider,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        mirror: answered,
    });
    events
}

impl Inner {
    fn limiter(&self, def: &ProviderDef) -> Option<Arc<RateLimiter>> {
        let n = def.rate_limit_per_min?;
        let mut map = self.limiters.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map
            .entry(def.name.clone())
            .and_modify(|l| {
                if l.per_min() != n {
                    *l = Arc::new(RateLimiter::new(n));
                }
            })
            .or_insert_with(|| Arc::new(RateLimiter::new(n)));
        Some(Arc::clone(entry))
    }

    async fn throttle(&self, def: &ProviderDef) {
        if let Some(l) = self.limiter(def) {
            l.acquire().await;
        }
    }

    fn headers(&self, def: &ProviderDef, mut req: RequestBuilder) -> RequestBuilder {
        for (k, v) in &def.headers {
            req = req.header(k, v);
        }
        req
    }

    /// The HTTP client for a proxy setting (`None` = the searcher-wide one).
    fn client_for(&self, own: Option<&ProxyUrl>) -> Result<Client, ProviderError> {
        let proxy = own.or(self.default_proxy.as_ref());
        let key = match proxy {
            None => String::new(),
            Some(p) if p.is_direct() => PROXY_NONE.to_owned(),
            Some(p) => p.as_str().to_owned(),
        };
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = clients.get(&key) {
            return Ok(c.clone());
        }
        let c = build_client(&self.user_agent, proxy, self.resolver.as_ref())?;
        clients.insert(key, c.clone());
        Ok(c)
    }

    /// Send a request, enforce 2xx and the body cap, return the body as text.
    /// Anti-bot interstitials are reported as [`ProviderError::Blocked`].
    async fn send(&self, req: RequestBuilder) -> Result<String, ProviderError> {
        let mut resp = req.send().await.map_err(map_reqwest)?;
        let status = resp.status();
        let url = redact(resp.url().as_str());
        let cap = if status.is_success() {
            MAX_BODY
        } else {
            // Only enough of an error page to recognise a challenge.
            64 * 1024
        };
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(map_reqwest)? {
            if buf.len() + chunk.len() > cap {
                if status.is_success() {
                    return Err(ProviderError::Parse(format!(
                        "response larger than {} MiB",
                        MAX_BODY / 1024 / 1024
                    )));
                }
                break;
            }
            buf.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&buf).into_owned();
        let host = Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        if util::is_block_page(&body) {
            return Err(ProviderError::Blocked(host));
        }
        if !status.is_success() {
            return Err(ProviderError::Http {
                status: status.as_u16(),
                url,
            });
        }
        Ok(body)
    }

    /// Hosts to try for `req`, the one that worked last first. A URL that is
    /// not on one of the provider's hosts is requested as it is.
    fn bases_for(&self, def: &ProviderDef, req: &Req) -> (Vec<Url>, bool) {
        let mut origins = def.origins();
        let own = origin_of(&req.url);
        if !origins.contains(&own) {
            return (vec![req.url.clone()], false);
        }
        if let Some(last) = self
            .last_mirror
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&def.name.to_lowercase())
            && let Some(i) = origins.iter().position(|o| o == last)
        {
            let o = origins.remove(i);
            origins.insert(0, o);
        }
        let bases = origins.iter().filter_map(|o| Url::parse(o).ok()).collect();
        (bases, true)
    }

    /// Send `req` to the provider's hosts in turn until one answers.
    /// Returns the body and, for providers with mirrors, the base URL that
    /// answered (remembered for the next request).
    ///
    /// All but the last attempt get a share of the time left until `deadline`
    /// so that one dead host cannot use up the whole budget.
    async fn exchange(
        &self,
        def: &ProviderDef,
        req: &Req,
        deadline: Instant,
    ) -> Result<(String, Option<String>), ProviderError> {
        let client = self.client_for(def.proxy.as_ref())?;
        let (bases, ours) = self.bases_for(def, req);
        let multi = bases.len() > 1;
        let mut failures: Vec<String> = Vec::new();
        let mut last: Option<ProviderError> = None;
        let mut blocked = false;
        for (i, base) in bases.iter().enumerate() {
            let left = u32::try_from(bases.len() - i).unwrap_or(1);
            self.throttle(def).await;
            let attempt = async {
                let url = rebase(&req.url, base);
                let mut r = client
                    .request(req.method.clone(), url)
                    .header("Accept", req.accept);
                if let Some(b) = &req.body {
                    r = r.header("Content-Type", "application/json").body(b.clone());
                }
                // Provider headers (cookies, auth) only go to the provider's own hosts.
                if ours {
                    r = self.headers(def, r);
                }
                self.send(r).await
            };
            let res = if left > 1 {
                let share = deadline.saturating_duration_since(Instant::now()) / left;
                tokio::time::timeout(share, attempt)
                    .await
                    .unwrap_or_else(|_| Err(ProviderError::Timeout(share.as_secs().max(1))))
            } else {
                attempt.await
            };
            match res {
                Ok(body) => {
                    let origin = origin_of(base);
                    if multi {
                        self.last_mirror
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(def.name.to_lowercase(), origin.clone());
                    }
                    return Ok((body, multi.then_some(origin)));
                }
                Err(e) if multi && mirror_retryable(&e) => {
                    tracing::debug!(provider = %def.name, mirror = %origin_of(base), error = %e, "mirror failed");
                    let why = match &e {
                        ProviderError::Blocked(_) => "anti-bot challenge page".to_owned(),
                        other => other.to_string(),
                    };
                    failures.push(format!("{}: {why}", origin_of(base)));
                    blocked |= matches!(e, ProviderError::Blocked(_));
                    last = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        match (last, failures.len()) {
            (Some(e), 1) => Err(e),
            _ => Err(ProviderError::Mirrors(format!(
                "{}{}",
                failures.join("; "),
                if blocked { " (see docs/search.md)" } else { "" }
            ))),
        }
    }

    /// Move result links on the provider's main host to the host that answered.
    fn follow_mirror(def: &ProviderDef, answered: &str, results: &mut [SearchResult]) {
        let (Ok(base), origins) = (Url::parse(answered), def.origins()) else {
            return;
        };
        // Links on the main host (relative links resolve against it) follow the
        // mirror; links on other hosts, mirrors included, are left alone.
        let Some(main) = origins.first() else {
            return;
        };
        for r in results {
            for link in [&mut r.detail_url, &mut r.torrent_url] {
                if let Some(u) = link.as_deref().and_then(|l| Url::parse(l).ok())
                    && &origin_of(&u) == main
                {
                    *link = Some(rebase(&u, &base).to_string());
                }
            }
        }
    }

    /// Request the provider's search endpoint (rate-limited, with mirror
    /// fallback) and parse the response.
    async fn fetch_primary(
        &self,
        def: &ProviderDef,
        text: &str,
        category: Option<&str>,
        strict: bool,
        deadline: Instant,
    ) -> Result<(Vec<SearchResult>, Option<String>), ProviderError> {
        let cat = category.unwrap_or("");
        let bad_url = |e: url::ParseError| ProviderError::Definition(format!("bad url: {e}"));
        let req = match &def.kind {
            Kind::Torznab {
                url,
                api_key_ref,
                categories,
            } => {
                let key = match api_key_ref {
                    Some(r) => Some(
                        self.secrets
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(r)
                            .map(|s| s.expose_secret().to_string())
                            .ok_or_else(|| ProviderError::MissingSecret(r.clone()))?,
                    ),
                    None => None,
                };
                let mut cats = categories.clone();
                if let Some(c) = category {
                    let extra = torznab::category_ids(c);
                    if !extra.is_empty() {
                        cats = extra;
                    }
                }
                let u = torznab::search_url(url, text, &cats, key.as_deref(), def.max_results)?;
                Req::get(u, "application/xml, text/xml, */*")
            }
            Kind::Feed { url } => Req::get(
                Url::parse(&util::fill_url(url, text, cat)).map_err(bad_url)?,
                "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
            ),
            Kind::Html(h) => Req::get(
                Url::parse(&util::fill_url(&h.search_url, text, cat)).map_err(bad_url)?,
                "text/html,application/xhtml+xml",
            ),
            Kind::Json(j) => {
                let url = Url::parse(&util::fill_url(&j.url, text, cat)).map_err(bad_url)?;
                match (j.method, &j.body) {
                    (HttpMethod::Post, Some(b)) => Req {
                        method: Method::POST,
                        url,
                        body: Some(util::fill_json_body(b, text, cat)),
                        accept: "application/json",
                    },
                    _ => Req::get(url, "application/json"),
                }
            }
        };
        let (body, mirror) = self.exchange(def, &req, deadline).await?;
        let mut results = parse_response(def, &body, strict)?;
        if let Some(m) = &mirror {
            Self::follow_mirror(def, m, &mut results);
        }
        if def.local_filter {
            results.retain(|r| matches_query(&r.title, text));
        }
        Ok((results, mirror))
    }

    async fn fetch_detail_magnet(
        &self,
        def: &ProviderDef,
        spec: &HtmlSpec,
        url: &str,
        deadline: Instant,
    ) -> Result<String, ProviderError> {
        let target = Url::parse(url)
            .map_err(|e| ProviderError::Definition(format!("bad detail url: {e}")))?;
        if !matches!(target.scheme(), "http" | "https") {
            return Err(ProviderError::Definition(
                "detail url must be http or https".into(),
            ));
        }
        let req = Req::get(target, "text/html,application/xhtml+xml");
        let (body, _) = self.exchange(def, &req, deadline).await?;
        html::parse_detail_magnet(spec, &body)
    }

    /// Fetch detail pages for results without a magnet, until `deadline`.
    /// Failures and timeouts leave results as they were.
    async fn follow_details(
        &self,
        def: &ProviderDef,
        spec: &HtmlSpec,
        results: &mut [SearchResult],
        deadline: Instant,
    ) {
        let todo: Vec<(usize, String)> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.magnet.is_none() && r.info_hash.is_none())
            .filter_map(|(i, r)| Some((i, r.detail_url.clone()?)))
            .take(spec.detail_limit)
            .collect();
        let mut s = Box::pin(
            stream::iter(todo)
                .map(|(i, url)| async move {
                    (i, self.fetch_detail_magnet(def, spec, &url, deadline).await)
                })
                .buffer_unordered(DETAIL_CONCURRENCY),
        );
        while let Ok(Some((i, res))) = tokio::time::timeout_at(deadline, s.next()).await {
            match res {
                Ok(m) => results[i].magnet = Some(m),
                Err(e) => tracing::debug!(provider = %def.name, error = %e, "detail page failed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_strips_credentials_and_query() {
        let r = redact("socks5://user:hunter2@proxy.invalid:1080/p?apikey=k#f");
        assert_eq!(r, "socks5://proxy.invalid:1080/p");
        assert!(!r.contains("hunter2") && !r.contains("apikey"));
    }

    #[test]
    fn rebase_swaps_scheme_host_and_port() {
        let u = Url::parse("https://a.invalid/search?q=x%20y#f").unwrap();
        let b = Url::parse("http://b.invalid:8080").unwrap();
        assert_eq!(
            rebase(&u, &b).as_str(),
            "http://b.invalid:8080/search?q=x%20y#f"
        );
    }

    #[test]
    fn local_filter_needs_every_word() {
        assert!(matches_query(
            "Ubuntu 24.04 Desktop (amd64)",
            "ubuntu desktop"
        ));
        assert!(matches_query("Big.Buck.Bunny.1080p", "big buck"));
        assert!(!matches_query("Debian 12", "ubuntu"));
        assert!(matches_query("anything", ""));
    }

    #[test]
    fn only_host_problems_are_retried_on_a_mirror() {
        let http = |status| ProviderError::Http {
            status,
            url: String::new(),
        };
        for s in [403, 451, 500, 503] {
            assert!(mirror_retryable(&http(s)), "{s}");
        }
        for s in [404, 400, 429] {
            assert!(!mirror_retryable(&http(s)), "{s}");
        }
        assert!(mirror_retryable(&ProviderError::Timeout(3)));
        assert!(mirror_retryable(&ProviderError::Blocked("x".into())));
        assert!(!mirror_retryable(&ProviderError::Parse("x".into())));
    }
}
