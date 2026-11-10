//! Mirror fallback, per-provider proxies and credential redaction.

use std::time::{Duration, Instant};

use dman_search::{
    HtmlSpec, JsonSpec, Kind, ProviderDef, ProviderError, ProviderEvent, ProxyUrl, Query, Searcher,
    SearcherConfig, test_provider,
};
use futures_util::StreamExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn fx(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn feed(name: &str, base: &str, mirrors: &[&str]) -> ProviderDef {
    ProviderDef {
        name: name.into(),
        kind: Kind::Feed {
            url: format!("{base}/feed?q={{query}}"),
        },
        enabled: true,
        timeout_secs: 15,
        rate_limit_per_min: None,
        max_results: 100,
        order: 0,
        test_query: None,
        verified: false,
        description: None,
        headers: Default::default(),
        mirrors: mirrors.iter().map(|m| (*m).to_owned()).collect(),
        proxy: None,
        local_filter: false,
    }
}

async fn ok_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("nyaa.xml")))
        .mount(&s)
        .await;
    s
}

async fn status_server(code: u16) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(path("/feed"))
        .respond_with(ResponseTemplate::new(code))
        .mount(&s)
        .await;
    s
}

/// Answers every path with `code`.
async fn down_server(code: u16) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(code))
        .mount(&s)
        .await;
    s
}

async fn hits(s: &MockServer) -> usize {
    s.received_requests().await.unwrap().len()
}

async fn run(s: &Searcher, name: &str) -> Vec<ProviderEvent> {
    let mut q = Query::new("frieren");
    q.providers = Some(vec![name.into()]);
    s.search(&q).collect().await
}

fn done_mirror(ev: &[ProviderEvent]) -> Option<String> {
    ev.iter().find_map(|e| match e {
        ProviderEvent::Done { mirror, .. } => mirror.clone(),
        _ => None,
    })
}

fn result_count(ev: &[ProviderEvent]) -> usize {
    ev.iter()
        .map(|e| match e {
            ProviderEvent::Results { results, .. } => results.len(),
            _ => 0,
        })
        .sum()
}

#[tokio::test]
async fn blocked_first_mirror_falls_through_and_is_remembered() {
    let a = status_server(403).await;
    let b = ok_server().await;
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![feed("P", &a.uri(), &[&b.uri()])]);

    let ev = run(&s, "P").await;
    assert_eq!(result_count(&ev), 2, "{ev:?}");
    assert_eq!(done_mirror(&ev), Some(b.uri()));
    assert_eq!((hits(&a).await, hits(&b).await), (1, 1));

    // The mirror that worked is asked first next time: the dead host is not retried.
    let ev = run(&s, "P").await;
    assert_eq!(result_count(&ev), 2);
    assert_eq!(done_mirror(&ev), Some(b.uri()));
    assert_eq!((hits(&a).await, hits(&b).await), (1, 2));
}

#[tokio::test]
async fn challenge_page_on_a_200_counts_as_blocked() {
    let a = MockServer::start().await;
    Mock::given(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("cloudflare.html")))
        .mount(&a)
        .await;
    let b = ok_server().await;
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![feed("P", &a.uri(), &[&b.uri()])]);
    let ev = run(&s, "P").await;
    assert_eq!(result_count(&ev), 2, "{ev:?}");
    assert_eq!(done_mirror(&ev), Some(b.uri()));
}

#[tokio::test]
async fn challenge_page_on_a_403_is_reported_as_blocked_without_mirrors() {
    let a = MockServer::start().await;
    Mock::given(path("/feed"))
        .respond_with(ResponseTemplate::new(403).set_body_string(fx("cloudflare.html")))
        .mount(&a)
        .await;
    let e = test_provider(&feed("P", &a.uri(), &[])).await.unwrap_err();
    assert!(matches!(e, ProviderError::Blocked(_)), "{e}");
    assert!(e.to_string().contains("docs/search.md"), "{e}");
}

#[tokio::test]
async fn every_retryable_status_moves_on() {
    for code in [403u16, 451, 500, 502, 503] {
        let a = status_server(code).await;
        let b = ok_server().await;
        let r = test_provider(&feed("P", &a.uri(), &[&b.uri()]))
            .await
            .unwrap_or_else(|e| panic!("{code}: {e}"));
        assert_eq!(r.results, 2, "{code}");
        assert_eq!(r.mirror, Some(b.uri()), "{code}");
    }
}

#[tokio::test]
async fn other_client_errors_do_not_trigger_a_mirror() {
    let a = status_server(404).await;
    let b = ok_server().await;
    let e = test_provider(&feed("P", &a.uri(), &[&b.uri()]))
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Http { status: 404, .. }), "{e}");
    assert_eq!(hits(&b).await, 0);
}

#[tokio::test]
async fn connection_refused_falls_through() {
    // a port nothing listens on
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let b = ok_server().await;
    let r = test_provider(&feed("P", &dead, &[&b.uri()])).await.unwrap();
    assert_eq!(r.results, 2);
    assert_eq!(r.mirror, Some(b.uri()));
}

#[tokio::test]
async fn slow_first_mirror_does_not_use_up_the_whole_timeout() {
    let a = MockServer::start().await;
    Mock::given(path("/feed"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(fx("nyaa.xml"))
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&a)
        .await;
    let b = ok_server().await;
    let mut d = feed("P", &a.uri(), &[&b.uri()]);
    d.timeout_secs = 4;
    let t = Instant::now();
    let r = test_provider(&d).await.unwrap();
    assert_eq!(r.mirror, Some(b.uri()));
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
}

#[tokio::test]
async fn all_mirrors_failing_lists_each_reason() {
    let a = status_server(403).await;
    let b = status_server(503).await;
    let e = test_provider(&feed("P", &a.uri(), &[&b.uri()]))
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Mirrors(_)), "{e}");
    let m = e.to_string();
    assert!(m.starts_with("no mirror answered"), "{m}");
    assert!(m.contains(&a.uri()) && m.contains("403"), "{m}");
    assert!(m.contains(&b.uri()) && m.contains("503"), "{m}");
}

#[tokio::test]
async fn provider_without_mirrors_reports_none() {
    let a = ok_server().await;
    let r = test_provider(&feed("P", &a.uri(), &[])).await.unwrap();
    assert_eq!(r.mirror, None);
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![feed("P", &a.uri(), &[])]);
    assert_eq!(done_mirror(&run(&s, "P").await), None);
}

#[tokio::test]
async fn html_links_and_detail_pages_follow_the_mirror() {
    // The main host is down; the mirror serves the search page and the detail
    // pages. Relative links in the page resolve to the mirror, and a detail
    // link that names the main host is requested from the mirror too.
    let a = down_server(403).await;
    let b = MockServer::start().await;
    let page = format!(
        r#"<table><tr class="r"><td><a class="t" href="/d/1">One</a></td><td class="s">1 GB</td></tr>
           <tr class="r"><td><a class="t" href="{}/d/2">Two</a></td><td class="s">2 GB</td></tr></table>"#,
        a.uri()
    );
    Mock::given(path("/s"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page))
        .mount(&b)
        .await;
    for n in ["1", "2"] {
        Mock::given(path(format!("/d/{n}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"<a href="magnet:?xt=urn:btih:{:0<40}&dn=x">m</a>"#,
                n
            )))
            .mount(&b)
            .await;
    }
    let mut d = feed("H", &a.uri(), &[&b.uri()]);
    d.kind = Kind::Html(HtmlSpec {
        search_url: format!("{}/s?q={{query}}", a.uri()),
        row: "tr.r".into(),
        title: "a.t".into(),
        magnet: None,
        info_hash: None,
        detail_link: Some("a.t @href".into()),
        size: Some("td.s".into()),
        seeders: None,
        leechers: None,
        date: None,
        date_format: None,
        follow_detail_for_magnet: true,
        detail_magnet: Some("a[href^='magnet:'] @href".into()),
        detail_limit: 10,
    });
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![d]);
    let ev = run(&s, "H").await;
    let results = ev
        .iter()
        .find_map(|e| match e {
            ProviderEvent::Results { results, .. } => Some(results.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{ev:?}"));
    assert_eq!(results.len(), 2);
    assert_eq!(done_mirror(&ev), Some(b.uri()));
    for r in &results {
        assert!(
            r.detail_url.as_deref().unwrap().starts_with(&b.uri()),
            "{r:?}"
        );
        assert!(
            r.magnet
                .as_deref()
                .unwrap()
                .starts_with("magnet:?xt=urn:btih:")
        );
    }
    // resolve_magnet (used when adding a result later) also falls back
    let m = s
        .resolve_magnet("H", &format!("{}/d/1", a.uri()))
        .await
        .unwrap();
    assert!(m.contains("btih:1000"), "{m}");
}

#[tokio::test]
async fn json_links_on_the_main_host_follow_the_mirror() {
    let a = down_server(503).await;
    let b = MockServer::start().await;
    Mock::given(path("/api"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"items":[{{"t":"x","h":"0123456789abcdef0123456789abcdef01234567","u":"{0}/page/1","o":"https://elsewhere.invalid/p"}}]}}"#,
            a.uri()
        )))
        .mount(&b)
        .await;
    let mut d = feed("J", &a.uri(), &[&b.uri()]);
    d.kind = Kind::Json(JsonSpec {
        url: format!("{}/api?q={{query}}", a.uri()),
        method: Default::default(),
        body: None,
        items: "/items".into(),
        expand: None,
        title: "/t".into(),
        size: None,
        seeders: None,
        leechers: None,
        date: None,
        info_hash: Some("/h".into()),
        magnet: None,
        torrent_url: None,
        detail_url: Some("/u".into()),
        category: None,
    });
    let r = test_provider(&d).await.unwrap();
    assert_eq!(r.mirror, Some(b.uri()));
    assert_eq!(
        r.sample.unwrap().detail_url,
        Some(format!("{}/page/1", b.uri()))
    );
}

#[tokio::test]
async fn local_filter_keeps_only_matching_titles() {
    let a = ok_server().await;
    let mut d = feed("F", &a.uri(), &[]);
    d.local_filter = true;
    d.test_query = Some("batch".into());
    let r = test_provider(&d).await.unwrap();
    assert_eq!(r.results, 1);
    assert!(r.sample.unwrap().title.contains("Batch"));
    d.test_query = Some("nothing-like-this".into());
    assert_eq!(test_provider(&d).await.unwrap().results, 0);
}

// ----------------------------------------------------------------- proxy

/// A wiremock server acts as an HTTP proxy: the request for the (unresolvable)
/// origin arrives there in absolute form.
async fn proxy_server() -> MockServer {
    let p = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("nyaa.xml")))
        .mount(&p)
        .await;
    p
}

#[tokio::test]
async fn provider_proxy_is_used_for_that_provider_only() {
    let proxy = proxy_server().await;
    let direct = ok_server().await;
    let mut via = feed("Via", "http://unreachable-origin.invalid", &[]);
    via.proxy = Some(ProxyUrl::new(proxy.uri()));
    let plain = feed("Plain", &direct.uri(), &[]);
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![via, plain]);
    assert_eq!(result_count(&run(&s, "Via").await), 2);
    assert_eq!(hits(&proxy).await, 1);
    assert_eq!(result_count(&run(&s, "Plain").await), 2);
    assert_eq!((hits(&proxy).await, hits(&direct).await), (1, 1));
}

#[tokio::test]
async fn global_proxy_applies_and_none_forces_direct() {
    let proxy = proxy_server().await;
    let direct = ok_server().await;
    let global = ProxyUrl::new(proxy.uri());
    let s = Searcher::new(SearcherConfig {
        proxy: Some(global),
        user_agent: None,
        dns: Default::default(),
    })
    .unwrap();
    let falls_back = feed("Global", "http://unreachable-origin.invalid", &[]);
    let mut bypass = feed("Direct", &direct.uri(), &[]);
    bypass.proxy = Some(ProxyUrl::new("none"));
    s.set_providers(vec![falls_back, bypass]);
    assert_eq!(result_count(&run(&s, "Global").await), 2);
    assert_eq!(hits(&proxy).await, 1);
    assert_eq!(result_count(&run(&s, "Direct").await), 2);
    assert_eq!(
        hits(&proxy).await,
        1,
        "`none` must not use the global proxy"
    );
    assert_eq!(hits(&direct).await, 1);
}

#[tokio::test]
async fn bad_proxies_are_rejected_without_echoing_credentials() {
    let e = Searcher::new(SearcherConfig {
        proxy: Some(ProxyUrl::new("ftp://alice:hunter2@proxy.invalid:1")),
        user_agent: None,
        dns: Default::default(),
    })
    .err()
    .expect("rejected");
    assert!(!e.to_string().contains("hunter2"), "{e}");

    let mut d = feed("P", "http://x.invalid", &[]);
    d.proxy = Some(ProxyUrl::new("nonsense://alice:hunter2@h"));
    let e = d.validate().unwrap_err().to_string();
    assert!(e.contains("proxy") && !e.contains("hunter2"), "{e}");
    d.mirrors = vec!["https://a.example/with/path".into()];
    d.proxy = None;
    assert!(d.validate().unwrap_err().to_string().contains("mirrors"));
}

#[tokio::test]
async fn unreachable_proxy_error_does_not_leak_credentials() {
    let mut d = feed("P", "http://origin.invalid", &[]);
    d.proxy = Some(ProxyUrl::new("socks5h://alice:hunter2@127.0.0.1:1"));
    let e = test_provider(&d).await.unwrap_err().to_string();
    assert!(!e.contains("hunter2") && !e.contains("alice"), "{e}");
}

#[test]
fn debug_output_redacts_proxy_credentials() {
    let mut d = feed("P", "http://x.invalid", &[]);
    d.proxy = Some(ProxyUrl::new("socks5h://alice:hunter2@proxy.invalid:1080"));
    let cfg = SearcherConfig {
        proxy: d.proxy.clone(),
        user_agent: None,
        dns: Default::default(),
    };
    for s in [
        format!("{d:?}"),
        format!("{cfg:?}"),
        format!("{:#?}", d.proxy),
    ] {
        assert!(!s.contains("hunter2") && !s.contains("alice"), "{s}");
        assert!(s.contains("proxy.invalid:1080"), "{s}");
    }
}

#[test]
fn mirrors_and_proxy_round_trip_through_toml() {
    let mut d = feed("P", "https://main.example", &["https://b.example:8443"]);
    d.proxy = Some(ProxyUrl::new("socks5h://127.0.0.1:9050"));
    d.local_filter = true;
    let text = dman_search::export(std::slice::from_ref(&d));
    assert!(
        text.contains("mirrors = [\"https://b.example:8443\"]"),
        "{text}"
    );
    assert!(text.contains("proxy = \"socks5h://127.0.0.1:9050\""));
    assert_eq!(dman_search::import(&text).unwrap(), vec![d.clone()]);
    assert_eq!(
        d.origins(),
        ["https://main.example", "https://b.example:8443"]
    );
}
