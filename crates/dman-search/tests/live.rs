//! Real-data tests: trimmed responses captured from the live services on
//! 2026-11-09 (always run), and gated end-to-end probes (`#[ignore]`, run with
//! `DMAN_TEST_LIVE=1 cargo test -p dman-search -- --ignored`).

use dman_search::{
    DEFAULT_TRACKER_LIST_URL, ProviderDef, SearchResult, Searcher, SearcherConfig,
    builtin_providers, parse_response,
};

fn fx(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn builtin(name: &str) -> ProviderDef {
    builtin_providers()
        .into_iter()
        .find(|d| d.name == name)
        .unwrap()
}

fn parsed(name: &str, file: &str) -> Vec<SearchResult> {
    let mut v = parse_response(&builtin(name), &fx(file), true).unwrap();
    v.iter_mut().for_each(SearchResult::normalize);
    v
}

fn is_hash(h: &str) -> bool {
    h.len() == 40 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Every result must be complete and consistent.
fn check_all(rs: &[SearchResult], want: usize) {
    assert_eq!(rs.len(), want);
    for r in rs {
        assert!(!r.title.trim().is_empty(), "{r:?}");
        assert!(r.size.is_some_and(|s| s > 0), "size: {r:?}");
        assert!(r.seeders.is_some() && r.leechers.is_some(), "peers: {r:?}");
        assert!(r.date.is_some_and(|d| d > 0), "date: {r:?}");
        let h = r.info_hash.as_deref().expect("info_hash");
        assert!(is_hash(h), "hash {h}");
        let m = r.magnet.as_deref().expect("magnet");
        assert!(
            m.to_lowercase()
                .starts_with(&format!("magnet:?xt=urn:btih:{h}")),
            "magnet {m}"
        );
    }
}

#[test]
fn live_nyaa_fixture() {
    let rs = parsed("Nyaa", "live_nyaa.xml");
    check_all(&rs, 8);
    let r = &rs[0];
    assert_eq!(r.info_hash.as_deref().map(str::len), Some(40));
    assert!(
        r.detail_url
            .as_deref()
            .unwrap()
            .starts_with("https://nyaa.si/view/")
    );
    assert!(r.torrent_url.as_deref().unwrap().contains("/download/"));
    // "624.0 MiB" style sizes are converted to bytes
    assert!(
        rs.iter()
            .any(|r| r.size.unwrap() % 1024 == 0 || r.size.unwrap() > 1_000_000)
    );
}

#[test]
fn live_tpb_fixture() {
    let rs = parsed("The Pirate Bay", "live_tpb.json");
    check_all(&rs, 8);
    assert_eq!(rs[0].title, "Ubuntu 22.04 LTS");
    assert_eq!(rs[0].size, Some(3_654_957_056));
    assert_eq!((rs[0].seeders, rs[0].leechers), (Some(34), Some(11)));
    assert_eq!(rs[0].date, Some(1_652_877_231));
    assert_eq!(
        rs[0].info_hash.as_deref(),
        Some("2c6b6858d61da9543d4231a71db4b1c9264b0685")
    );
    assert_eq!(
        rs[0].detail_url.as_deref(),
        Some("https://thepiratebay.org/description.php?id=59191690")
    );
    // HTML entities in names are decoded
    assert!(rs.iter().any(|r| r.title.contains("& Linux")), "{rs:#?}");
}

#[test]
fn live_knaben_fixture() {
    let rs = parsed("Knaben", "live_knaben.json");
    check_all(&rs, 10);
    let r = &rs[0];
    assert_eq!(r.title, "ubuntu-26.04-desktop-amd64.iso");
    assert_eq!(r.size, Some(6_517_612_871));
    assert_eq!((r.seeders, r.leechers), (Some(124), Some(52)));
    assert_eq!(r.date, Some(1_777_131_300));
    assert_eq!(
        r.info_hash.as_deref(),
        Some("dafc8c076ca2f3ed376eeae7c76a0d6be2415c45")
    );
    assert_eq!(r.category.as_deref(), Some("PC / Unix"));
}

// ------------------------------------------------------------- live (gated)

fn live_enabled() -> bool {
    std::env::var("DMAN_TEST_LIVE").is_ok_and(|v| v == "1")
}

async fn live_probe(name: &str) {
    if !live_enabled() {
        eprintln!("DMAN_TEST_LIVE != 1, skipping");
        return;
    }
    let mut def = builtin(name);
    def.test_query = Some("ubuntu".into());
    let report = dman_search::test_provider(&def).await.unwrap();
    assert!(report.results > 0, "{name}: no results");
    let s = report.sample.expect("sample");
    eprintln!(
        "{name}: {} results in {:?}; sample {s:#?}",
        report.results, report.elapsed
    );
    check_all(std::slice::from_ref(&s), 1);
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_nyaa() {
    live_probe("Nyaa").await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_tpb() {
    live_probe("The Pirate Bay").await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_knaben() {
    live_probe("Knaben").await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_tracker_list() {
    if !live_enabled() {
        return;
    }
    let s = Searcher::new(SearcherConfig::default()).unwrap();
    let list = s
        .fetch_tracker_list(DEFAULT_TRACKER_LIST_URL)
        .await
        .unwrap();
    eprintln!(
        "{} trackers, first: {:?}",
        list.len(),
        &list[..3.min(list.len())]
    );
    assert!(list.len() >= 5);
    for t in &list {
        let u = url::Url::parse(t).unwrap();
        assert!(
            matches!(u.scheme(), "http" | "https" | "udp" | "wss"),
            "{t}"
        );
        assert!(u.host_str().is_some());
    }
}
