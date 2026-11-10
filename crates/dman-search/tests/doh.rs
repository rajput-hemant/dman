//! DNS over HTTPS: a wiremock server plays the DoH resolver and answers every
//! name with 127.0.0.1, so a provider on an unresolvable host name is reached
//! only if the lookup really went through it.

use dman_search::{
    DnsMode, Kind, ProviderDef, ProviderError, ProxyUrl, Searcher, SearcherConfig, doh::DohResolver,
};
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn answer(rcode: u8, ips: &[[u8; 4]]) -> Vec<u8> {
    let mut m = vec![
        0,
        0,
        0x81,
        0x80 | rcode,
        0,
        0,
        0,
        u8::try_from(ips.len()).unwrap(),
        0,
        0,
        0,
        0,
    ];
    for ip in ips {
        m.extend_from_slice(&[0, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        m.extend_from_slice(ip);
    }
    m
}

async fn doh_server(body: Vec<u8>) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns-query"))
        .and(wiremock::matchers::header(
            "content-type",
            "application/dns-message",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body)
                .insert_header("content-type", "application/dns-message"),
        )
        .mount(&s)
        .await;
    s
}

fn endpoint(s: &MockServer) -> Url {
    Url::parse(&format!("{}/dns-query", s.uri())).unwrap()
}

fn mode(endpoints: Vec<Url>) -> DnsMode {
    DnsMode::Doh {
        label: "doh:test".into(),
        endpoints,
    }
}

fn provider(port_of: &MockServer) -> ProviderDef {
    let port = port_of.address().port();
    let mut d = dman_search::import(&format!(
        "[[provider]]\nname = \"P\"\nkind = \"feed\"\nurl = \"http://doh-origin.invalid:{port}/feed?q={{query}}\"\nproxy = \"none\"\n"
    ))
    .unwrap()
    .remove(0);
    d.enabled = true;
    d
}

async fn feed_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(path("/feed"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                std::fs::read_to_string(format!(
                    "{}/tests/fixtures/nyaa.xml",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .unwrap(),
            ),
        )
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn provider_host_is_resolved_through_doh() {
    let doh = doh_server(answer(0, &[[127, 0, 0, 1]])).await;
    let feed = feed_server().await;
    let s = Searcher::new(SearcherConfig {
        dns: mode(vec![endpoint(&doh)]),
        ..SearcherConfig::default()
    })
    .unwrap();
    let r = s.test_provider(&provider(&feed)).await.unwrap();
    assert_eq!(r.results, 2);
    assert!(!doh.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn next_endpoint_is_used_when_the_first_is_down() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        Url::parse(&format!("http://{}/dns-query", l.local_addr().unwrap())).unwrap()
    };
    let doh = doh_server(answer(0, &[[127, 0, 0, 1]])).await;
    let feed = feed_server().await;
    let s = Searcher::new(SearcherConfig {
        dns: mode(vec![dead, endpoint(&doh)]),
        ..SearcherConfig::default()
    })
    .unwrap();
    assert_eq!(s.test_provider(&provider(&feed)).await.unwrap().results, 2);
}

#[tokio::test]
async fn nxdomain_is_reported_as_a_dns_failure() {
    let doh = doh_server(answer(3, &[])).await;
    let feed = feed_server().await;
    let s = Searcher::new(SearcherConfig {
        dns: mode(vec![endpoint(&doh)]),
        ..SearcherConfig::default()
    })
    .unwrap();
    let e = s.test_provider(&provider(&feed)).await.unwrap_err();
    assert!(matches!(e, ProviderError::Network(_)), "{e}");
    assert!(e.to_string().contains("DNS-over-HTTPS"), "{e}");
    assert!(e.to_string().contains("NXDOMAIN"), "{e}");
}

#[tokio::test]
async fn answers_are_cached_for_their_ttl() {
    let doh = doh_server(answer(0, &[[127, 0, 0, 1]])).await;
    let r = DohResolver::new("dman-test", vec![endpoint(&doh)]).unwrap();
    let first = r.lookup("cache.example").await.unwrap();
    let asked = doh.received_requests().await.unwrap().len();
    assert_eq!(asked, 2, "one A and one AAAA query");
    assert_eq!(r.lookup("CACHE.example").await.unwrap(), first);
    assert_eq!(doh.received_requests().await.unwrap().len(), asked);
}

#[tokio::test]
async fn proxy_providers_do_not_need_doh() {
    // A proxy resolves the target itself: an unusable DoH server is not asked.
    let doh = MockServer::start().await; // answers 404 to everything
    let feed = feed_server().await;
    let s = Searcher::new(SearcherConfig {
        dns: mode(vec![endpoint(&doh)]),
        ..SearcherConfig::default()
    })
    .unwrap();
    let mut d = provider(&feed);
    d.kind = Kind::Feed {
        url: "http://unreachable-origin.invalid/feed?q={query}".into(),
    };
    d.proxy = Some(ProxyUrl::new(feed.uri()));
    assert_eq!(s.test_provider(&d).await.unwrap().results, 2);
    assert!(doh.received_requests().await.unwrap().is_empty());
}
