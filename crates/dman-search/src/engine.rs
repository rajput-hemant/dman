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
    error::ProviderError,
    html,
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
    /// `http://`, `https://`, `socks5://` or `socks5h://` proxy URL.
    pub proxy: Option<String>,
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
    },
}

/// Outcome of a successful provider probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// Number of results parsed (0 is possible for a working provider).
    pub results: usize,
    pub sample: Option<SearchResult>,
    pub elapsed: Duration,
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
    client: Client,
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

/// Whether `url` has the same scheme, host and port as the provider's search
/// URL template.
fn same_origin(url: &Url, template: &str) -> bool {
    Url::parse(&template.replace(['{', '}'], "")).is_ok_and(|t| t.origin() == url.origin())
}

impl Searcher {
    pub fn new(config: SearcherConfig) -> Result<Self, ProviderError> {
        ensure_crypto_provider();
        let ua = config
            .user_agent
            .unwrap_or_else(|| format!("dman/{}", env!("CARGO_PKG_VERSION")));
        let mut b = Client::builder()
            .user_agent(ua)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5));
        if let Some(p) = config.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
            b = b.proxy(reqwest::Proxy::all(p).map_err(|e| {
                ProviderError::Definition(format!("bad proxy `{}`: {e}", redact(p)))
            })?);
        }
        let client = b.build().map_err(map_reqwest)?;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
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
        let mut results =
            tokio::time::timeout_at(deadline, self.inner.fetch_primary(def, &q, None, true))
                .await
                .map_err(|_| ProviderError::Timeout(def.timeout_secs))??;
        if let Kind::Html(h) = &def.kind
            && h.follow_detail_for_magnet
            && let Some(url) = results.iter().find_map(|r| r.detail_url.clone())
        {
            tokio::time::timeout_at(deadline, self.inner.fetch_detail_magnet(def, h, &url))
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
        tokio::time::timeout(
            Duration::from_secs(def.timeout_secs),
            self.inner.fetch_detail_magnet(&def, h, detail_url),
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
        let req = self.inner.client.get(u).timeout(Duration::from_secs(30));
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
    let outcome = async {
        let mut results = tokio::time::timeout_at(
            deadline,
            inner.fetch_primary(&def, &q.text, q.category.as_deref(), false),
        )
        .await
        .map_err(|_| ProviderError::Timeout(def.timeout_secs))??;
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

    /// Send a request, enforce 2xx and the body cap, return the body as text.
    async fn send(&self, req: RequestBuilder) -> Result<String, ProviderError> {
        let mut resp = req.send().await.map_err(map_reqwest)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ProviderError::Http {
                status: status.as_u16(),
                url: redact(resp.url().as_str()),
            });
        }
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(map_reqwest)? {
            if buf.len() + chunk.len() > MAX_BODY {
                return Err(ProviderError::Parse(format!(
                    "response larger than {} MiB",
                    MAX_BODY / 1024 / 1024
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Rate-limit, request and parse the provider's search endpoint.
    async fn fetch_primary(
        &self,
        def: &ProviderDef,
        text: &str,
        category: Option<&str>,
        strict: bool,
    ) -> Result<Vec<SearchResult>, ProviderError> {
        let cat = category.unwrap_or("");
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
                self.client
                    .get(u)
                    .header("Accept", "application/xml, text/xml, */*")
            }
            Kind::Feed { url } => self.client.get(util::fill_url(url, text, cat)).header(
                "Accept",
                "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
            ),
            Kind::Html(h) => self
                .client
                .get(util::fill_url(&h.search_url, text, cat))
                .header("Accept", "text/html,application/xhtml+xml"),
            Kind::Json(j) => {
                let url = util::fill_url(&j.url, text, cat);
                match (j.method, &j.body) {
                    (HttpMethod::Post, Some(b)) => self
                        .client
                        .request(Method::POST, url)
                        .header("Content-Type", "application/json")
                        .body(util::fill_json_body(b, text, cat)),
                    _ => self.client.get(url),
                }
                .header("Accept", "application/json")
            }
        };
        let req = self.headers(def, req);
        self.throttle(def).await;
        let body = self.send(req).await?;
        parse_response(def, &body, strict)
    }

    async fn fetch_detail_magnet(
        &self,
        def: &ProviderDef,
        spec: &HtmlSpec,
        url: &str,
    ) -> Result<String, ProviderError> {
        let target = Url::parse(url)
            .map_err(|e| ProviderError::Definition(format!("bad detail url: {e}")))?;
        if !matches!(target.scheme(), "http" | "https") {
            return Err(ProviderError::Definition(
                "detail url must be http or https".into(),
            ));
        }
        self.throttle(def).await;
        let req = self
            .client
            .get(target.clone())
            .header("Accept", "text/html,application/xhtml+xml");
        // Provider headers (cookies, auth) must not leak to other sites.
        let req = if same_origin(&target, &spec.search_url) {
            self.headers(def, req)
        } else {
            req
        };
        let body = self.send(req).await?;
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
                .map(|(i, url)| async move { (i, self.fetch_detail_magnet(def, spec, &url).await) })
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
    fn same_origin_compares_scheme_host_port() {
        let t = "https://site.invalid/search?q={query}";
        assert!(same_origin(
            &Url::parse("https://site.invalid/d/1").unwrap(),
            t
        ));
        assert!(!same_origin(
            &Url::parse("https://evil.invalid/d/1").unwrap(),
            t
        ));
        assert!(!same_origin(
            &Url::parse("http://site.invalid/d/1").unwrap(),
            t
        ));
    }
}
