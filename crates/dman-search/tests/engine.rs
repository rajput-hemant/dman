use std::time::{Duration, Instant};

use dman_search::{
    Aggregator, HtmlSpec, HttpMethod, JsonSpec, Kind, ProviderDef, ProviderError, ProviderEvent,
    Query, Searcher, SearcherConfig, aggregate, engine::RateLimiter, fetch_tracker_list,
    test_provider,
};
use futures_util::StreamExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

fn fx(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn def(name: &str, kind: Kind) -> ProviderDef {
    ProviderDef {
        name: name.into(),
        kind,
        enabled: true,
        timeout_secs: 15,
        rate_limit_per_min: None,
        max_results: 100,
        order: 0,
        test_query: None,
        verified: false,
        description: None,
        headers: Default::default(),
    }
}

fn feed(name: &str, url: String) -> ProviderDef {
    def(name, Kind::Feed { url })
}

fn tpb_like(name: &str, url: String) -> ProviderDef {
    let mut d = dman_search::builtin_providers()
        .into_iter()
        .find(|d| d.name == "The Pirate Bay")
        .unwrap();
    d.name = name.into();
    d.enabled = true;
    if let Kind::Json(j) = &mut d.kind {
        j.url = url;
    }
    d
}

async fn collect(s: &Searcher, q: &Query) -> Vec<ProviderEvent> {
    s.search(q).collect().await
}

#[tokio::test]
async fn slow_failing_and_ok_providers_are_isolated() {
    let server = MockServer::start().await;
    Mock::given(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("tpb.json")))
        .mount(&server)
        .await;
    Mock::given(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("[]")
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    Mock::given(path("/boom"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let mut slow = tpb_like("Slow", format!("{}/slow?q={{query}}", server.uri()));
    slow.timeout_secs = 2;
    let boom = tpb_like("Boom", format!("{}/boom?q={{query}}", server.uri()));
    let ok = tpb_like("Ok", format!("{}/ok?q={{query}}", server.uri()));
    let mut disabled = tpb_like("Off", format!("{}/ok?q={{query}}", server.uri()));
    disabled.enabled = false;

    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![slow, boom, ok, disabled]);

    let start = Instant::now();
    let mut seen = Vec::new();
    let mut stream = Box::pin(s.search(&Query::new("ubuntu")));
    let mut first_results_at = None;
    while let Some(e) = stream.next().await {
        if matches!(e, ProviderEvent::Results { .. }) {
            first_results_at = Some(start.elapsed());
        }
        seen.push(e);
    }
    let total = start.elapsed();

    // OK provider delivered immediately, long before the slow one timed out.
    assert!(
        first_results_at.unwrap() < Duration::from_millis(1500),
        "{first_results_at:?}"
    );
    // Total ~ the slow provider's timeout (2s), not the 30s response delay or a sum.
    assert!(
        total >= Duration::from_millis(1900) && total < Duration::from_secs(5),
        "{total:?}"
    );

    let results: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Results { provider, results } => {
                Some((provider.as_str(), results.len()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(results, [("Ok", 2)]);
    let failed: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Failed { provider, error } => Some((provider.as_str(), error.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 2);
    let boom_err = failed.iter().find(|f| f.0 == "Boom").unwrap().1;
    assert!(boom_err.contains("HTTP 500"), "{boom_err}");
    let slow_err = failed.iter().find(|f| f.0 == "Slow").unwrap().1;
    assert!(slow_err.contains("timed out after 2s"), "{slow_err}");
    // exactly one Done per enabled provider, and none for the disabled one
    let mut done: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Done { provider, .. } => Some(provider.clone()),
            _ => None,
        })
        .collect();
    done.sort();
    assert_eq!(done, ["Boom", "Ok", "Slow"]);

    let ranked = aggregate(seen);
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].sources, ["Ok"]);
    assert_eq!(ranked[0].seeders, Some(321));
}

#[tokio::test]
async fn results_from_two_providers_are_merged_with_sources() {
    let server = MockServer::start().await;
    Mock::given(path("/a"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("tpb.json")))
        .mount(&server)
        .await;
    Mock::given(path("/b.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("nyaa.xml")))
        .mount(&server)
        .await;
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    let a = tpb_like("A", format!("{}/a?q={{query}}", server.uri()));
    let mut b = feed("B", format!("{}/b.xml?q={{query}}", server.uri()));
    b.order = 1;
    s.set_providers(vec![a, b]);
    let mut agg = Aggregator::default();
    let mut stream = Box::pin(s.search(&Query::new("x")));
    while let Some(e) = stream.next().await {
        if let ProviderEvent::Results { results, .. } = e {
            agg.add(results);
        }
    }
    let out = agg.results();
    // tpb: H1 (0123...) + 1123...; nyaa: H1 + 1123... -> two distinct hashes, each from both
    assert_eq!(out.len(), 2);
    for r in &out {
        let mut s = r.sources.clone();
        s.sort();
        assert_eq!(s, ["A", "B"]);
    }
    assert_eq!(out[0].seeders, Some(1523)); // max of 321 and 1523
}

#[tokio::test]
async fn explicit_provider_selection_and_unknown_names() {
    let server = MockServer::start().await;
    Mock::given(path("/a"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("tpb.json")))
        .mount(&server)
        .await;
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![
        tpb_like("A", format!("{}/a?q={{query}}", server.uri())),
        tpb_like("B", format!("{}/a?q={{query}}", server.uri())),
    ]);
    let q = Query {
        text: "x".into(),
        category: None,
        providers: Some(vec!["b".into(), "Nope".into()]),
    };
    let ev = collect(&s, &q).await;
    let providers: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Results { provider, .. } => Some(provider.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(providers, ["B"]);
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::Failed { provider, error } if provider == "Nope" && error.contains("unknown or disabled"))));
}

#[tokio::test]
async fn max_results_cap_and_sources_set() {
    let server = MockServer::start().await;
    Mock::given(path("/a"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("tpb.json")))
        .mount(&server)
        .await;
    let mut d = tpb_like("A", format!("{}/a?q={{query}}", server.uri()));
    d.max_results = 1;
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![d]);
    let ev = collect(&s, &Query::new("x")).await;
    match &ev[0] {
        ProviderEvent::Results { results, .. } => {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].sources, ["A"]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn query_is_url_encoded_and_user_agent_sent() {
    let server = MockServer::start().await;
    Mock::given(path("/q"))
        .and(query_param("q", "a b&c"))
        .and(header(
            "user-agent",
            format!("dman/{}", env!("CARGO_PKG_VERSION")).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("tpb.json")))
        .expect(1)
        .mount(&server)
        .await;
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![tpb_like(
        "A",
        format!("{}/q?q={{query}}", server.uri()),
    )]);
    let ev = collect(&s, &Query::new("a b&c")).await;
    assert!(matches!(ev[0], ProviderEvent::Results { .. }), "{ev:?}");
}

#[tokio::test]
async fn json_post_body_template() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1"))
        .and(body_json(
            serde_json::json!({"query": "he said \"hi\"", "size": 5}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("knaben.json")))
        .expect(1)
        .mount(&server)
        .await;
    let mut d = dman_search::builtin_providers()
        .into_iter()
        .find(|d| d.name == "Knaben")
        .unwrap();
    d.enabled = true;
    if let Kind::Json(JsonSpec {
        url, body, method, ..
    }) = &mut d.kind
    {
        *url = format!("{}/v1", server.uri());
        *body = Some(r#"{"query":"{query}","size":5}"#.into());
        assert_eq!(*method, HttpMethod::Post);
    }
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![d]);
    let ev = collect(&s, &Query::new("he said \"hi\"")).await;
    match &ev[0] {
        ProviderEvent::Results { results, .. } => assert_eq!(results.len(), 2),
        other => panic!("{other:?}"),
    }
}

fn leet(server_uri: &str) -> ProviderDef {
    let mut d = dman_search::builtin_providers()
        .into_iter()
        .find(|d| d.name == "1337x")
        .unwrap();
    d.enabled = true;
    d.rate_limit_per_min = None;
    if let Kind::Html(HtmlSpec { search_url, .. }) = &mut d.kind {
        *search_url = format!("{server_uri}/search/{{query}}/");
    }
    d
}

#[tokio::test]
async fn html_provider_follows_detail_pages_for_magnets() {
    let server = MockServer::start().await;
    // detail links in the fixture point at 1337x.to; rewrite them to the mock server
    let page = fx("1337x_search.html").replace(
        "href=\"/torrent/",
        &format!("href=\"{}/torrent/", server.uri()),
    );
    Mock::given(path("/search/ubuntu/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page))
        .mount(&server)
        .await;
    Mock::given(path("/torrent/5551111/Ubuntu-24-04-Desktop-amd64/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("1337x_detail.html")))
        .mount(&server)
        .await;
    Mock::given(path("/torrent/5552222/Ubuntu-Server-24-04/"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![leet(&server.uri())]);
    let ev = collect(&s, &Query::new("ubuntu")).await;
    let ProviderEvent::Results { results, .. } = &ev[0] else {
        panic!("{ev:?}")
    };
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].info_hash.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert!(
        results[0]
            .magnet
            .as_deref()
            .unwrap()
            .contains("tracker.opentrackr.org")
    );
    assert!(results[1].magnet.is_none() && results[1].detail_url.is_some());

    // on-demand resolution of a single detail page
    let m = s
        .resolve_magnet("1337x", results[0].detail_url.as_deref().unwrap())
        .await
        .unwrap();
    assert!(m.starts_with("magnet:?xt=urn:btih:0123"));
    let e = s
        .resolve_magnet("1337x", results[1].detail_url.as_deref().unwrap())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("HTTP 503"), "{e}");
    assert!(matches!(
        s.resolve_magnet("zzz", "http://x/").await,
        Err(ProviderError::Unknown(_))
    ));
}

#[tokio::test]
async fn torznab_uses_secret_and_never_leaks_it() {
    let server = MockServer::start().await;
    Mock::given(path("/api"))
        .and(query_param("t", "search"))
        .and(query_param("apikey", "s3cr3t"))
        .and(query_param("q", "ubuntu"))
        .and(query_param("cat", "2000"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("torznab.xml")))
        .mount(&server)
        .await;
    Mock::given(path("/bad"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let tz = |name: &str, p: &str| {
        def(
            name,
            Kind::Torznab {
                url: format!("{}{p}", server.uri()),
                api_key_ref: Some("prowlarr".into()),
                categories: vec![],
            },
        )
    };
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![tz("Good", "/api"), tz("Bad", "/bad")]);

    // without the secret: clear error, no request made
    let ev = collect(&s, &Query::new("ubuntu")).await;
    assert!(
        ev.iter()
            .all(|e| !matches!(e, ProviderEvent::Results { .. }))
    );
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::Failed { error, .. } if error.contains("missing secret `prowlarr`"))));

    s.set_secret("prowlarr", "s3cr3t");
    let q = Query {
        text: "ubuntu".into(),
        category: Some("movies".into()),
        providers: None,
    };
    let ev = collect(&s, &q).await;
    let bad = ev.iter().find_map(|e| match e {
        ProviderEvent::Failed { provider, error } if provider == "Bad" => Some(error.clone()),
        _ => None,
    });
    let bad = bad.unwrap();
    assert!(
        bad.contains("HTTP 401") && !bad.contains("s3cr3t") && !bad.contains("apikey"),
        "{bad}"
    );
    let good = ev.iter().find_map(|e| match e {
        ProviderEvent::Results { provider, results } if provider == "Good" => Some(results.clone()),
        _ => None,
    });
    let good = good.unwrap();
    assert_eq!(good.len(), 3);
    assert_eq!(
        good[0].info_hash.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
}

#[tokio::test]
async fn test_provider_reports_precise_failures() {
    let server = MockServer::start().await;
    Mock::given(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("nyaa.xml")))
        .mount(&server)
        .await;
    Mock::given(path("/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(path("/cf"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("cloudflare.html")))
        .mount(&server)
        .await;
    Mock::given(path("/notjson"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
        .mount(&server)
        .await;

    let ok = test_provider(&feed("F", format!("{}/ok?q={{query}}", server.uri())))
        .await
        .unwrap();
    assert_eq!(ok.results, 2);
    assert!(ok.sample.unwrap().title.contains("Frieren"));

    let e = test_provider(&feed("F", format!("{}/gone", server.uri())))
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Http { status: 404, .. }), "{e}");
    assert!(e.to_string().contains("/gone"));

    let mut h = leet(&server.uri());
    if let Kind::Html(s) = &mut h.kind {
        s.search_url = format!("{}/cf?q={{query}}", server.uri());
    }
    let e = test_provider(&h).await.unwrap_err().to_string();
    assert!(
        e.contains("row selector") && e.contains("matched 0 elements"),
        "{e}"
    );

    let e = test_provider(&tpb_like(
        "J",
        format!("{}/notjson?q={{query}}", server.uri()),
    ))
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("invalid JSON"), "{e}");

    // connection refused
    let e = test_provider(&feed("F", "http://127.0.0.1:1/x".into()))
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Network(_)), "{e}");

    // invalid definition is reported before any request
    let e = test_provider(&feed("", "http://x/".into()))
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Definition(_)));
}

#[tokio::test]
async fn test_provider_checks_detail_page_selector() {
    let server = MockServer::start().await;
    let page = fx("1337x_search.html").replace(
        "href=\"/torrent/",
        &format!("href=\"{}/torrent/", server.uri()),
    );
    Mock::given(path("/search/test/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page))
        .mount(&server)
        .await;
    Mock::given(path_regex_torrent())
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("1337x_detail_nomagnet.html")))
        .mount(&server)
        .await;
    let e = test_provider(&leet(&server.uri()))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("detail page") && e.contains("no magnet"), "{e}");
}

fn path_regex_torrent() -> wiremock::matchers::PathRegexMatcher {
    wiremock::matchers::path_regex(r"^/torrent/")
}

#[tokio::test(start_paused = true)]
async fn rate_limiter_spaces_requests() {
    // 600/min = one per 100 ms, burst of 3
    let l = RateLimiter::new(600);
    let t0 = tokio::time::Instant::now();
    for _ in 0..3 {
        l.acquire().await;
    }
    assert!(
        t0.elapsed() < Duration::from_millis(5),
        "burst is free: {:?}",
        t0.elapsed()
    );
    for _ in 0..5 {
        l.acquire().await;
    }
    let e = t0.elapsed();
    assert!(
        e >= Duration::from_millis(490) && e <= Duration::from_millis(520),
        "{e:?}"
    );

    // 6/min = one every 10 s
    let l = RateLimiter::new(6);
    let t0 = tokio::time::Instant::now();
    for _ in 0..4 {
        l.acquire().await;
    }
    let e = t0.elapsed();
    assert!(
        e >= Duration::from_secs(9) && e <= Duration::from_secs(11),
        "{e:?}"
    );
}

#[tokio::test]
async fn engine_applies_provider_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(path("/a"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("tpb.json")))
        .mount(&server)
        .await;
    let mut d = tpb_like("A", format!("{}/a?q={{query}}", server.uri()));
    d.rate_limit_per_min = Some(300); // 200 ms spacing, burst 3
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    s.set_providers(vec![d]);
    let start = Instant::now();
    for _ in 0..6 {
        let ev = collect(&s, &Query::new("x")).await;
        assert!(matches!(ev[0], ProviderEvent::Results { .. }));
    }
    // 3 burst + 3 spaced by 200 ms
    assert!(
        start.elapsed() >= Duration::from_millis(550),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn tracker_list_download() {
    let server = MockServer::start().await;
    Mock::given(path("/t.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fx("trackers.txt")))
        .mount(&server)
        .await;
    Mock::given(path("/empty.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# nothing\n"))
        .mount(&server)
        .await;
    Mock::given(path("/500"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let list = fetch_tracker_list(&format!("{}/t.txt", server.uri()))
        .await
        .unwrap();
    assert_eq!(
        list,
        [
            "udp://tracker.opentrackr.org:1337/announce",
            "udp://open.stealth.si:80/announce",
            "http://tracker.example.org:6969/announce"
        ]
    );
    assert!(
        fetch_tracker_list(&format!("{}/empty.txt", server.uri()))
            .await
            .is_err()
    );
    assert!(matches!(
        fetch_tracker_list(&format!("{}/500", server.uri())).await,
        Err(ProviderError::Http { status: 500, .. })
    ));
    assert!(fetch_tracker_list("ftp://x/y").await.is_err());
}

#[tokio::test]
async fn bad_proxy_is_rejected_and_proxy_accepted() {
    assert!(
        Searcher::new(SearcherConfig {
            proxy: Some("socks5://127.0.0.1:9050".into()),
            user_agent: None
        })
        .is_ok()
    );
    assert!(
        Searcher::new(SearcherConfig {
            proxy: Some("http://127.0.0.1:3128".into()),
            user_agent: Some("x".into())
        })
        .is_ok()
    );
    assert!(
        Searcher::new(SearcherConfig {
            proxy: Some("::bad::".into()),
            user_agent: None
        })
        .is_err()
    );
}
