//! Search, providers and tracker-list tests. Providers are custom JSON
//! providers pointing at a local wiremock server: no network is used.
//!
//! The tracker-attach test needs a real `aria2c`:
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, DiskThreshold, Paths, Secrets};
use dman_daemon::{DaemonOptions, run};
use dman_proto::method::Event;
use dman_proto::search::{ProviderSetParams, ProviderStatus, SearchStartParams};
use dman_proto::{AddOutcome, AddParams, AddSource, ErrorCode, JobOptions};
use secrecy::SecretString;
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn aria2_enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HASH_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// A custom JSON provider `[[search.provider]]` table.
fn json_provider(name: &str, base: &str, endpoint: &str, enabled: bool, timeout: u64) -> String {
    format!(
        "[[search.provider]]\nname = \"{name}\"\nkind = \"json\"\nenabled = {enabled}\n\
         timeout_secs = {timeout}\nurl = \"{base}/{endpoint}?q={{query}}\"\nitems = \"/items\"\n\
         title = \"/t\"\ninfo_hash = \"/h\"\nseeders = \"/s\"\n\n"
    )
}

async fn json_endpoint(server: &MockServer, endpoint: &str, items: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(format!("/{endpoint}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": items })))
        .mount(server)
        .await;
}

struct Daemon {
    paths: Paths,
    url: Url,
    task: JoinHandle<anyhow::Result<()>>,
    config: Config,
}

fn base_config(root: &Path) -> Config {
    let mut c = Config::default();
    let p = c.profiles.get_mut("local").unwrap();
    p.daemon = DaemonTarget::Local {
        listen: format!("127.0.0.1:{}", free_port()),
        secret_ref: "daemon".into(),
        tls: None,
    };
    p.download_dir = root.join("dl").display().to_string();
    p.min_free = DiskThreshold::Absolute(0);
    p.warn_free = DiskThreshold::Absolute(0);
    c.aria2.path = "/nonexistent/aria2c".into();
    c.aria2.rpc_port = free_port();
    c.aria2.tick_ms = 200;
    c
}

/// Writes `config.toml` (the serialized `base` plus `extra` TOML text, with a
/// leading comment) and returns the config as the daemon will load it.
fn write_config(root: &Path, base: &Config, extra: &str) -> Config {
    let paths = Paths::with_root(root);
    let text = format!(
        "# my hand-written config\n{}\n# --- search section below ---\n{extra}",
        toml::to_string(base).unwrap()
    );
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(paths.config_file(), &text).unwrap();
    let cfg = Config::load(&paths).unwrap();
    cfg.validate().unwrap();
    cfg
}

impl Daemon {
    async fn start(root: &Path, config: Config) -> Self {
        let paths = Paths::with_root(root);
        let DaemonTarget::Local { listen, .. } = &config.profiles["local"].daemon else {
            unreachable!()
        };
        let url = Url::parse(&format!("ws://{listen}/api/v1/ws")).unwrap();
        let task = tokio::spawn(run(DaemonOptions {
            paths: paths.clone(),
            config: config.clone(),
            profile: "local".into(),
        }));
        let d = Self {
            paths,
            url,
            task,
            config,
        };
        for _ in 0..200 {
            if d.task.is_finished() {
                break;
            }
            if d.try_connect().await.is_ok() {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let r = d.task.await;
        panic!("daemon did not come up: {r:?}");
    }

    fn token(&self) -> SecretString {
        Secrets::load(&self.paths)
            .ok()
            .and_then(|s| s.get("daemon"))
            .unwrap_or_else(|| SecretString::from(String::new()))
    }

    async fn try_connect(&self) -> Result<Client, ClientError> {
        Client::connect(&self.url, self.token(), "dman-test").await
    }

    async fn client(&self) -> Client {
        self.try_connect().await.expect("connect")
    }

    async fn stop(self, client: &Client) {
        client.shutdown().await.expect("shutdown call");
        tokio::time::timeout(Duration::from_secs(30), self.task)
            .await
            .expect("daemon did not stop")
            .expect("join")
            .expect("daemon result");
    }
}

/// Everything a search published until `SearchFinished`.
struct Run {
    updates: Vec<(String, ProviderStatus, usize)>,
    finished: bool,
}

async fn collect(rx: &mut broadcast::Receiver<Event>, id: u64, secs: u64) -> Run {
    let mut run = Run {
        updates: Vec::new(),
        finished: false,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(Event::SearchUpdate {
                search_id,
                provider,
                status,
                results,
            })) if search_id == id => run.updates.push((provider, status, results.len())),
            Ok(Ok(Event::SearchFinished { search_id })) if search_id == id => {
                run.finished = true;
                return run;
            }
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(_)) | Err(_) => return run,
        }
    }
}

fn last_status<'a>(run: &'a Run, provider: &str) -> &'a ProviderStatus {
    &run.updates
        .iter()
        .rev()
        .find(|(p, _, _)| p == provider)
        .unwrap_or_else(|| panic!("no update for {provider}: {:?}", run.updates))
        .1
}

fn start(query: &str) -> SearchStartParams {
    SearchStartParams {
        query: query.into(),
        ..Default::default()
    }
}

async fn stored_providers(root: &Path) -> String {
    std::fs::read_to_string(Paths::with_root(root).config_file()).unwrap()
}

// ----- tests ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn search_streams_per_provider_and_isolates_failures() {
    let mock = MockServer::start().await;
    json_endpoint(
        &mock,
        "alpha",
        json!([{"t": "Ubuntu 24.04", "h": HASH_A, "s": 10}, {"t": "Ubuntu mini", "h": HASH_B, "s": 3}]),
    )
    .await;
    json_endpoint(
        &mock,
        "beta",
        json!([{"t": "Ubuntu 24.04 (beta copy)", "h": HASH_A, "s": 50}, {"t": "Other", "h": HASH_C, "s": 1}]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/gamma"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock)
        .await;
    // A disabled provider must never be queried.
    Mock::given(method("GET"))
        .and(path("/delta"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&mock)
        .await;
    let base = mock.uri();
    let extra = [
        json_provider("alpha", &base, "alpha", true, 10),
        json_provider("beta", &base, "beta", true, 10),
        json_provider("gamma", &base, "gamma", true, 10),
        json_provider("delta", &base, "delta", false, 10),
    ]
    .concat();
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(root.path(), &base_config(root.path()), &extra);
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut rx = c.subscribe().await.unwrap();

    let started = c.search_start(start("ubuntu")).await.unwrap();
    let mut names = started.providers.clone();
    names.sort();
    assert_eq!(names, ["alpha", "beta", "gamma"]);
    let run = collect(&mut rx, started.search_id, 15).await;
    assert!(run.finished, "no SearchFinished: {:?}", run.updates);
    assert!(
        matches!(
            last_status(&run, "alpha"),
            ProviderStatus::Done { count: 2, .. }
        ),
        "{:?}",
        run.updates
    );
    assert!(matches!(
        last_status(&run, "beta"),
        ProviderStatus::Done { count: 2, .. }
    ));
    match last_status(&run, "gamma") {
        ProviderStatus::Failed { error } => assert!(error.contains("500"), "{error}"),
        other => panic!("gamma should fail: {other:?}"),
    }
    // Every provider announced itself while running.
    for p in ["alpha", "beta", "gamma"] {
        assert!(
            run.updates
                .iter()
                .any(|(n, s, _)| n == p && matches!(s, ProviderStatus::Running)),
            "{p} never reported Running: {:?}",
            run.updates
        );
    }

    // The merged list: HASH_A deduplicated with both sources, best seeders first.
    let snap = c.search_results(started.search_id).await.unwrap();
    assert!(snap.finished);
    assert_eq!(snap.query, "ubuntu");
    assert_eq!(snap.results.len(), 3, "{:?}", snap.results);
    assert_eq!(snap.results[0].info_hash.as_deref(), Some(HASH_A));
    let mut src = snap.results[0].sources.clone();
    src.sort();
    assert_eq!(src, ["alpha", "beta"]);
    assert!(
        snap.results[0]
            .magnet
            .as_deref()
            .unwrap()
            .starts_with("magnet:?xt=urn:btih:")
    );
    assert!(
        snap.results
            .windows(2)
            .all(|w| w[0].seeders >= w[1].seeders)
    );
    assert_eq!(snap.providers.len(), 3);

    // Restricting providers; an unknown name fails on its own.
    let s2 = c
        .search_start(SearchStartParams {
            query: "ubuntu".into(),
            providers: Some(vec!["ALPHA".into(), "nope".into()]),
            category: None,
        })
        .await
        .unwrap();
    let run2 = collect(&mut rx, s2.search_id, 15).await;
    assert!(run2.finished);
    assert!(matches!(
        last_status(&run2, "alpha"),
        ProviderStatus::Done { .. }
    ));
    assert!(matches!(
        last_status(&run2, "nope"),
        ProviderStatus::Failed { .. }
    ));
    assert!(run2.updates.iter().all(|(p, _, _)| p != "beta"));

    // Bad input and unknown ids.
    let err = c.search_start(start("   ")).await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::InvalidInput),
        "{err:?}"
    );
    let err = c.search_results(999).await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );

    // Only the last five searches are kept.
    for i in 0..5 {
        let s = c.search_start(start(&format!("q{i}"))).await.unwrap();
        collect(&mut rx, s.search_id, 15).await;
    }
    assert!(c.search_results(started.search_id).await.is_err());
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_are_capped_at_500_results() {
    let mock = MockServer::start().await;
    let many: Vec<_> = (0..700)
        .map(|i| json!({"t": format!("item {i}"), "h": format!("{i:040x}"), "s": i}))
        .collect();
    json_endpoint(&mock, "big", json!(many)).await;
    let base = mock.uri();
    let extra = json_provider("big", &base, "big", true, 10)
        .replace("timeout_secs = 10", "timeout_secs = 10\nmax_results = 700");
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(root.path(), &base_config(root.path()), &extra);
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut rx = c.subscribe().await.unwrap();
    let s = c.search_start(start("x")).await.unwrap();
    let run = collect(&mut rx, s.search_id, 15).await;
    assert!(run.finished);
    let max = run.updates.iter().map(|u| u.2).max().unwrap();
    assert_eq!(max, 500, "{:?}", run.updates);
    assert_eq!(
        c.search_results(s.search_id).await.unwrap().results.len(),
        500
    );
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_a_slow_provider_without_hiding_others() {
    let mock = MockServer::start().await;
    json_endpoint(&mock, "fast", json!([{"t": "quick", "h": HASH_A, "s": 1}])).await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"items": []}))
                .set_delay(Duration::from_secs(25)),
        )
        .mount(&mock)
        .await;
    let base = mock.uri();
    let extra = [
        json_provider("fast", &base, "fast", true, 30),
        json_provider("slow", &base, "slow", true, 30),
    ]
    .concat();
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(root.path(), &base_config(root.path()), &extra);
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut rx = c.subscribe().await.unwrap();
    let s = c.search_start(start("x")).await.unwrap();

    // fast completes while slow is still running.
    let mut fast_done = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !fast_done && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(Event::SearchUpdate {
            search_id,
            provider,
            status: ProviderStatus::Done { .. },
            results,
        })) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            && search_id == s.search_id
            && provider == "fast"
        {
            assert_eq!(results.len(), 1);
            fast_done = true;
        }
    }
    assert!(fast_done, "fast provider did not finish");
    let snap = c.search_results(s.search_id).await.unwrap();
    assert!(!snap.finished);

    let t = std::time::Instant::now();
    c.search_cancel(s.search_id).await.unwrap();
    let run = collect(&mut rx, s.search_id, 5).await;
    assert!(run.finished, "cancel did not finish the search");
    assert!(t.elapsed() < Duration::from_secs(5));
    let snap = c.search_results(s.search_id).await.unwrap();
    assert!(snap.finished);
    assert_eq!(snap.results.len(), 1, "results found so far are kept");
    let slow = snap.providers.iter().find(|(n, _)| n == "slow").unwrap();
    assert!(
        matches!(&slow.1, ProviderStatus::Failed { error } if error == "cancelled"),
        "{slow:?}"
    );
    // Cancelling a finished search is fine.
    c.search_cancel(s.search_id).await.unwrap();
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_is_persisted_deduplicated_and_clearable() {
    let mock = MockServer::start().await;
    json_endpoint(&mock, "a", json!([{"t": "x", "h": HASH_A, "s": 1}])).await;
    let base = mock.uri();
    let extra = json_provider("a", &base, "a", true, 10);
    let root = tempfile::tempdir().unwrap();
    let mut base_cfg = base_config(root.path());
    base_cfg.search.history_size = 3;
    let cfg = write_config(root.path(), &base_cfg, &extra);
    let d = Daemon::start(root.path(), cfg.clone()).await;
    let c = d.client().await;
    let mut rx = c.subscribe().await.unwrap();
    for q in ["one", "one", "two", "three", "four"] {
        let s = c.search_start(start(q)).await.unwrap();
        assert!(collect(&mut rx, s.search_id, 10).await.finished);
    }
    let h = c.search_history().await.unwrap();
    let queries: Vec<_> = h.iter().map(|e| e.query.as_str()).collect();
    assert_eq!(
        queries,
        ["four", "three", "two"],
        "newest first, capped at 3"
    );
    assert_eq!(h[0].result_count, 1);
    d.stop(&c).await;

    // A new daemon on the same data directory still knows them.
    let mut cfg2 = cfg;
    let DaemonTarget::Local { listen, .. } = &mut cfg2.profiles.get_mut("local").unwrap().daemon
    else {
        unreachable!()
    };
    *listen = format!("127.0.0.1:{}", free_port());
    cfg2.aria2.rpc_port = free_port();
    let d = Daemon::start(root.path(), cfg2).await;
    let c = d.client().await;
    let h = c.search_history().await.unwrap();
    assert_eq!(h.len(), 3);
    assert_eq!(h[0].query, "four");
    c.search_history_clear().await.unwrap();
    assert!(c.search_history().await.unwrap().is_empty());
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_set_applies_at_once_and_is_saved_with_comments() {
    let mock = MockServer::start().await;
    json_endpoint(&mock, "a", json!([{"t": "from a", "h": HASH_A, "s": 1}])).await;
    json_endpoint(&mock, "b", json!([{"t": "from b", "h": HASH_B, "s": 2}])).await;
    let base = mock.uri();
    let extra = format!(
        "# providers I use\n{}{}",
        json_provider("a", &base, "a", true, 10),
        json_provider("b", &base, "b", false, 10)
            .replace("enabled = false", "enabled = false # off for now"),
    ) + "[[search.provider]]\nname = \"keyed\"\nkind = \"torznab\"\nurl = \"http://127.0.0.1:9/api?apikey=SECRET123\"\napi_key_ref = \"keyed\"\n";
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(root.path(), &base_config(root.path()), &extra);
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut rx = c.subscribe().await.unwrap();

    let list = c.providers_list().await.unwrap();
    let json = serde_json::to_string(&list).unwrap();
    assert!(!json.contains("SECRET123"), "secret leaked: {json}");
    let keyed = list.iter().find(|p| p.name == "keyed").unwrap();
    assert_eq!(keyed.api_key_ref.as_deref(), Some("keyed"));
    assert_eq!(keyed.kind, "torznab");
    assert!(!keyed.builtin && !keyed.enabled);
    let nyaa = list.iter().find(|p| p.name == "Nyaa").unwrap();
    assert!(nyaa.builtin && !nyaa.enabled);
    assert!(list.windows(2).all(|w| w[0].order <= w[1].order));

    // Before: only `a` answers.
    let s = c.search_start(start("q")).await.unwrap();
    assert_eq!(s.providers, ["a"]);
    collect(&mut rx, s.search_id, 10).await;

    // Enable b, give it order -1 and a short timeout; no restart.
    let info = c
        .providers_set(ProviderSetParams {
            name: "B".into(),
            enabled: Some(true),
            order: Some(-1),
            timeout_secs: Some(7),
        })
        .await
        .unwrap();
    assert!(info.enabled && info.order == -1 && info.timeout_secs == 7);
    let s = c.search_start(start("q")).await.unwrap();
    assert_eq!(s.providers, ["b", "a"], "ordered by `order`");
    let run = collect(&mut rx, s.search_id, 10).await;
    assert!(matches!(
        last_status(&run, "b"),
        ProviderStatus::Done { count: 1, .. }
    ));

    // A built-in is overridden by a small [[search.provider]] entry; flip it back off.
    c.providers_set(ProviderSetParams {
        name: "nyaa".into(),
        enabled: Some(true),
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(
        c.providers_list()
            .await
            .unwrap()
            .iter()
            .any(|p| p.name == "Nyaa" && p.enabled)
    );
    c.providers_set(ProviderSetParams {
        name: "Nyaa".into(),
        enabled: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let text = stored_providers(root.path()).await;
    assert!(text.contains("# my hand-written config"), "{text}");
    assert!(text.contains("# providers I use"), "{text}");
    assert!(text.contains("# --- search section below ---"), "{text}");
    assert!(text.contains("enabled = true # off for now"), "{text}");
    let saved = Config::from_toml_str(&text, Path::new("config.toml")).unwrap();
    saved.validate().unwrap();
    let defs = saved.search.effective_providers().unwrap();
    let b = defs.iter().find(|d| d.name == "b").unwrap();
    assert!(b.enabled && b.order == -1 && b.timeout_secs == 7);
    assert!(
        defs.iter()
            .find(|d| d.name == "Nyaa")
            .is_some_and(|d| !d.enabled)
    );

    // Errors.
    let err = c
        .providers_set(ProviderSetParams {
            name: "nope".into(),
            enabled: Some(true),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );
    let err = c
        .providers_set(ProviderSetParams {
            name: "a".into(),
            timeout_secs: Some(0),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::InvalidInput),
        "{err:?}"
    );
    // The rejected change was not saved or applied.
    assert!(
        !stored_providers(root.path())
            .await
            .contains("timeout_secs = 0")
    );
    assert_eq!(
        c.providers_list()
            .await
            .unwrap()
            .iter()
            .find(|p| p.name == "a")
            .unwrap()
            .timeout_secs,
        10
    );
    d.stop(&c).await;

    // A fresh daemon reads the persisted state.
    let mut cfg = Config::load(&Paths::with_root(root.path())).unwrap();
    let DaemonTarget::Local { listen, .. } = &mut cfg.profiles.get_mut("local").unwrap().daemon
    else {
        unreachable!()
    };
    *listen = format!("127.0.0.1:{}", free_port());
    cfg.aria2.rpc_port = free_port();
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let list = c.providers_list().await.unwrap();
    assert!(list.iter().find(|p| p.name == "b").unwrap().enabled);
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_test_import_export_remove() {
    let mock = MockServer::start().await;
    json_endpoint(&mock, "ok", json!([{"t": "sample", "h": HASH_A, "s": 4}])).await;
    Mock::given(method("GET"))
        .and(path("/bad"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&mock)
        .await;
    let base = mock.uri();
    let extra = [
        json_provider("ok", &base, "ok", false, 10),
        json_provider("bad", &base, "bad", false, 10),
    ]
    .concat();
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(root.path(), &base_config(root.path()), &extra);
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;

    // Testing works for disabled providers; failures are reports, not errors.
    let r = c.providers_test("ok").await.unwrap();
    assert!(r.ok && r.results == 1 && r.error.is_none(), "{r:?}");
    assert_eq!(r.sample.unwrap().title, "sample");
    let r = c.providers_test("bad").await.unwrap();
    assert!(
        !r.ok && r.error.as_deref().unwrap().contains("503"),
        "{r:?}"
    );
    let err = c.providers_test("nope").await.unwrap_err();
    assert!(matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound));

    // Export: custom providers by default, named ones (also built-ins) on request.
    let all = c.providers_export(None).await.unwrap();
    let defs = dman_search::import(&all).unwrap();
    let mut names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["bad", "ok"]);
    let one = c.providers_export(Some(vec!["Nyaa".into()])).await.unwrap();
    assert!(one.contains("Nyaa") && !one.contains("\"ok\""), "{one}");

    // Import: validated, added disabled, replaces a custom provider of the same name.
    let file = format!(
        "[[provider]]\nname = \"shared\"\nkind = \"json\"\nenabled = true\nverified = true\nurl = \"{base}/ok?q={{query}}\"\nitems = \"/items\"\ntitle = \"/t\"\ninfo_hash = \"/h\"\n"
    );
    let imported = c.providers_import(&file).await.unwrap();
    assert_eq!(imported.names, ["shared"]);
    let list = c.providers_list().await.unwrap();
    let shared = list.iter().find(|p| p.name == "shared").unwrap();
    assert!(!shared.enabled && !shared.verified && !shared.builtin);
    c.providers_import(&file.replace("/ok?", "/bad?"))
        .await
        .unwrap();
    let list = c.providers_list().await.unwrap();
    assert_eq!(list.iter().filter(|p| p.name == "shared").count(), 1);
    assert!(
        list.iter()
            .find(|p| p.name == "shared")
            .unwrap()
            .url
            .contains("/bad")
    );
    let text = stored_providers(root.path()).await;
    assert_eq!(text.matches("name = \"shared\"").count(), 1, "{text}");
    assert!(text.contains("# my hand-written config"));
    assert!(c.providers_test("shared").await.unwrap().error.is_some());

    // Bad imports name the provider and field.
    let err = c
        .providers_import("[[provider]]\nname = \"x\"\nkind = \"json\"\nurl = \"ftp://h/{query}\"\nitems = \"/i\"\ntitle = \"/t\"\n")
        .await
        .unwrap_err();
    let ClientError::Rpc(e) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert!(
        e.message.contains('x') && e.message.contains("url"),
        "{}",
        e.message
    );
    for bad in [
        "not toml [",
        "",
        "[[provider]]\nname = \"Nyaa\"\nkind = \"feed\"\nurl = \"http://x/\"\n",
    ] {
        let err = c.providers_import(bad).await.unwrap_err();
        assert!(
            matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::InvalidInput),
            "{bad}: {err:?}"
        );
    }

    // Remove: custom only.
    c.providers_remove("SHARED").await.unwrap();
    assert!(
        !c.providers_list()
            .await
            .unwrap()
            .iter()
            .any(|p| p.name == "shared")
    );
    assert!(!stored_providers(root.path()).await.contains("shared"));
    let err = c.providers_remove("Nyaa").await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::Refused),
        "{err:?}"
    );
    let err = c.providers_remove("shared").await.unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_reads_the_magnet_from_a_detail_page() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/foo"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<table><tr><td><a class=\"t\" href=\"/t/1\">Foo item</a></td><td class=\"s\">12</td></tr></table>",
        ))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/t/1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<html><a href=\"magnet:?xt=urn:btih:{HASH_A}&dn=Foo\">get</a></html>"
        )))
        .mount(&mock)
        .await;
    let base = mock.uri();
    let extra = format!(
        "[[search.provider]]\nname = \"scrape\"\nkind = \"html\"\nenabled = true\nsearch_url = \"{base}/search/{{query}}\"\nrow = \"tr\"\ntitle = \"a.t\"\ndetail_link = \"a.t @href\"\nseeders = \"td.s\"\nfollow_detail_for_magnet = false\ndetail_magnet = \"a[href^='magnet:'] @href\"\n"
    );
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(root.path(), &base_config(root.path()), &extra);
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let mut rx = c.subscribe().await.unwrap();
    let s = c.search_start(start("foo")).await.unwrap();
    assert!(collect(&mut rx, s.search_id, 10).await.finished);
    let snap = c.search_results(s.search_id).await.unwrap();
    assert_eq!(snap.results.len(), 1, "{snap:?}");
    assert!(snap.results[0].magnet.is_none());
    assert!(
        snap.results[0]
            .detail_url
            .as_deref()
            .unwrap()
            .ends_with("/t/1")
    );
    let m = c
        .search_resolve(dman_proto::search::SearchResolveParams {
            search_id: Some(s.search_id),
            index: Some(0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(m.magnet.contains(HASH_A), "{}", m.magnet);
    let m2 = c
        .search_resolve(dman_proto::search::SearchResolveParams {
            detail_url: snap.results[0].detail_url.clone(),
            provider: Some("scrape".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(m, m2);
    let err = c
        .search_resolve(dman_proto::search::SearchResolveParams {
            search_id: Some(s.search_id),
            index: Some(5),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );
    d.stop(&c).await;
}

fn tracker_config(root: &Path, url: &str, enabled: bool, attach: bool) -> Config {
    let mut c = base_config(root);
    c.search.tracker_list.enabled = enabled;
    c.search.tracker_list.url = url.into();
    c.search.tracker_list.attach_to_new = attach;
    c
}

async fn wait_trackers(c: &Client, count: u32) -> dman_proto::search::TrackersStatus {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let st = c.trackers_status().await.unwrap();
        if st.count == count || tokio::time::Instant::now() > deadline {
            return st;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tracker_list_is_fetched_cached_and_survives_offline_start() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/list.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "# best trackers\nudp://t1.example:1337/announce\n\nhttps://t2.example/announce\nbogus line\n",
        ))
        .mount(&mock)
        .await;
    let url = format!("{}/list.txt", mock.uri());
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(
        root.path(),
        &tracker_config(root.path(), &url, true, true),
        "",
    );
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    let st = wait_trackers(&c, 2).await;
    assert!(st.enabled && st.attach_to_new && st.count == 2, "{st:?}");
    assert!(
        st.last_refresh.is_some() && st.last_error.is_none(),
        "{st:?}"
    );
    let cache = std::fs::read_to_string(
        Paths::with_root(root.path())
            .data_dir()
            .join("trackers.txt"),
    )
    .unwrap();
    assert!(
        cache.contains("udp://t1.example:1337/announce")
            && cache.contains("https://t2.example/announce")
    );
    d.stop(&c).await;
    drop(mock);

    // The source is gone: the cache serves the list and the error is reported.
    let mut cfg2 = Config::load(&Paths::with_root(root.path())).unwrap();
    cfg2.search.tracker_list.url = format!("http://127.0.0.1:{}/list.txt", free_port());
    let DaemonTarget::Local { listen, .. } = &mut cfg2.profiles.get_mut("local").unwrap().daemon
    else {
        unreachable!()
    };
    *listen = format!("127.0.0.1:{}", free_port());
    cfg2.aria2.rpc_port = free_port();
    let d = Daemon::start(root.path(), cfg2).await;
    let c = d.client().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let st = loop {
        let st = c.trackers_status().await.unwrap();
        if st.last_error.is_some() || tokio::time::Instant::now() > deadline {
            break st;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(st.count, 2, "{st:?}");
    assert!(st.last_error.is_some(), "{st:?}");
    d.stop(&c).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_tracker_list_makes_no_request() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/list.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("udp://t:1/announce\n"))
        .expect(0)
        .mount(&mock)
        .await;
    let url = format!("{}/list.txt", mock.uri());
    let root = tempfile::tempdir().unwrap();
    let cfg = write_config(
        root.path(),
        &tracker_config(root.path(), &url, false, true),
        "",
    );
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let st = c.trackers_status().await.unwrap();
    assert!(
        !st.enabled && st.count == 0 && st.last_refresh.is_none() && st.last_error.is_none(),
        "{st:?}"
    );
    d.stop(&c).await;
    assert!(
        !Paths::with_root(root.path())
            .data_dir()
            .join("trackers.txt")
            .exists()
    );
    mock.verify().await;
}

/// The list joins the user's own trackers on new magnets and torrents and is
/// left off for plain downloads. Needs a real aria2c.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs aria2c; run with DMAN_TEST_ARIA2=1 and --include-ignored"]
async fn tracker_list_is_attached_to_new_magnets_with_real_aria2() {
    if !aria2_enabled() {
        eprintln!("DMAN_TEST_ARIA2 != 1; skipping");
        return;
    }
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/list.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "udp://t1.example:1337/announce\nudp://mine.example:1/announce\nhttps://t2.example/announce\n",
        ))
        .mount(&mock)
        .await;
    let url = format!("{}/list.txt", mock.uri());
    let root = tempfile::tempdir().unwrap();
    let mut cfg = tracker_config(root.path(), &url, true, true);
    cfg.aria2.path = "aria2c".into();
    let cfg = write_config(root.path(), &cfg, "");
    let d = Daemon::start(root.path(), cfg).await;
    let c = d.client().await;
    assert!(wait_trackers(&c, 3).await.count == 3);
    let magnet = format!("magnet:?xt=urn:btih:{HASH_A}&dn=x");
    let out = c
        .add(AddParams {
            sources: vec![AddSource::Uris { uris: vec![magnet] }],
            options: JobOptions {
                engine_options: [(
                    "bt-tracker".to_owned(),
                    "udp://mine.example:1/announce".to_owned(),
                )]
                .into(),
                ..Default::default()
            },
            paused: true,
        })
        .await
        .unwrap();
    let AddOutcome::Added { id, .. } = &out[0] else {
        panic!("{out:?}")
    };
    let detail = c.detail(*id).await.unwrap();
    assert_eq!(
        detail.options.engine_options["bt-tracker"],
        "udp://mine.example:1/announce,udp://t1.example:1337/announce,https://t2.example/announce"
    );
    d.stop(&c).await;
    assert_no_aria2(root.path());
}

fn assert_no_aria2(root: &Path) {
    let needle = root.display().to_string();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(cmd) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        let c = String::from_utf8_lossy(&cmd);
        assert!(
            !(c.contains("aria2c") && c.contains(&needle) && !stat.contains(") Z ")),
            "aria2c still running"
        );
    }
}
