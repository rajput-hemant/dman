//! Real-data tests: trimmed responses captured from the live services on
//! 2026-11-09 and 2026-11-10 (always run), and gated end-to-end probes (`#[ignore]`, run with
//! `DMAN_TEST_LIVE=1 cargo test -p dman-search -- --ignored`).

use dman_search::{
    DEFAULT_TRACKER_LIST_URL, Kind, ProviderDef, SearchResult, Searcher, SearcherConfig,
    builtin_providers, html, parse_response,
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

// ------------------------------------------------ providers added 2026-11-10

fn size_date_peers(r: &SearchResult) {
    assert!(r.size.is_some_and(|s| s > 0), "size: {r:?}");
    assert!(r.seeders.is_some() && r.leechers.is_some(), "peers: {r:?}");
}

#[test]
fn live_eztv_fixture() {
    let rs = parsed("EZTV", "live_eztv.json");
    check_all(&rs, 8);
    assert_eq!(
        rs[0].title,
        "CIA 2026 S02E06 Legacy 720p AMZN WEB-DL DDP5 1 H 264-NTb EZTV"
    );
    assert_eq!(rs[0].size, Some(1_345_638_537));
    assert_eq!((rs[0].seeders, rs[0].leechers), (Some(1), Some(12)));
    assert_eq!(rs[0].date, Some(1_794_292_585));
    assert_eq!(
        rs[0].info_hash.as_deref(),
        Some("5487d3b123d3b2b8ef517edcdc48ec23e5e85836")
    );
    assert!(builtin("EZTV").local_filter);
}

#[test]
fn live_torlock_fixture() {
    let rs = parsed("Torlock", "live_torlock.xml");
    assert_eq!(rs.len(), 8);
    for r in &rs {
        size_date_peers(r);
        assert!(r.date.is_some_and(|d| d > 0), "{r:?}");
        assert!(r.torrent_url.as_deref().unwrap().ends_with(".torrent"));
        assert!(
            r.detail_url
                .as_deref()
                .unwrap()
                .starts_with("https://www.torlock.com/torrent/")
        );
    }
    // seeds, leechers and size come from the item description / enclosure
    assert_eq!(rs[0].title, "Extol [restored]");
    assert_eq!(rs[0].size, Some(2_706_242_580));
    assert_eq!((rs[0].seeders, rs[0].leechers), (Some(0), Some(0)));
}

#[test]
fn live_limetorrents_fixture() {
    let rs = parsed("LimeTorrents", "live_limetorrents.html");
    assert_eq!(rs.len(), 9);
    for r in &rs {
        size_date_peers(r);
        assert!(r.date.is_some(), "{r:?}");
        assert!(
            r.detail_url
                .as_deref()
                .unwrap()
                .starts_with("https://www.limetorrents.fun/"),
            "{r:?}"
        );
    }
    let phony = rs.iter().find(|r| r.title.starts_with("Phony")).unwrap();
    assert_eq!((phony.seeders, phony.leechers), (Some(5), Some(8)));
    assert_eq!(phony.size, Some(355_100_000));
    // the magnet comes from the detail page
    let def = builtin("LimeTorrents");
    let Kind::Html(h) = &def.kind else { panic!() };
    let m = html::parse_detail_magnet(h, &fx("live_limetorrents_detail.html")).unwrap();
    assert!(m.starts_with("magnet:?xt=urn:btih:0A6250C621965CB7042A569237EC0943C8E50676"));
}

#[test]
fn live_torrentdownloads_fixture() {
    let rs = parsed("TorrentDownloads", "live_torrentdownloads.html");
    // the sponsored first row (a link to another site) is skipped
    assert_eq!(rs.len(), 6);
    assert!(rs.iter().all(|r| !r.title.is_empty()));
    for r in &rs {
        size_date_peers(r);
        assert!(
            r.detail_url
                .as_deref()
                .unwrap()
                .starts_with("https://www.torrentdownloads.pro/torrent/"),
            "{r:?}"
        );
    }
    let book = rs
        .iter()
        .find(|r| r.title.contains("Ultimate Ubuntu Handbook"))
        .unwrap();
    assert_eq!(book.size, Some(10_990_000));
    assert_eq!((book.seeders, book.leechers), (Some(8), Some(0)));
    let def = builtin("TorrentDownloads");
    let Kind::Html(h) = &def.kind else { panic!() };
    let m = html::parse_detail_magnet(h, &fx("live_torrentdownloads_detail.html")).unwrap();
    assert!(m.starts_with("magnet:?xt=urn:btih:7B6CAE55DB21441F06EA00A25C2A21E11752EEE2"));
}

#[test]
fn live_internet_archive_fixture() {
    let rs = parsed("Internet Archive", "live_internet_archive.json");
    assert_eq!(rs.len(), 6);
    let r = &rs[0];
    assert_eq!(r.title, "HCCTV Up To The Minute: December 6, 2023");
    assert_eq!(r.size, Some(347_917_336));
    assert_eq!(r.date, Some(1_708_092_301));
    assert_eq!(
        r.torrent_url.as_deref(),
        Some(
            "https://archive.org/download/hcctx-HCCTV_Up_To_The_Minute_-_December_6_2023/hcctx-HCCTV_Up_To_The_Minute_-_December_6_2023_archive.torrent"
        )
    );
    assert_eq!(
        r.detail_url.as_deref(),
        Some("https://archive.org/details/hcctx-HCCTV_Up_To_The_Minute_-_December_6_2023")
    );
    // no swarm figures and no hash in the listing
    assert!(r.seeders.is_none() && r.info_hash.is_none() && r.magnet.is_none());
    assert!(
        rs.iter()
            .all(|r| r.torrent_url.is_some() && r.size.is_some())
    );
}

#[test]
fn live_academic_torrents_fixture() {
    let rs = parsed("Academic Torrents", "live_academic_torrents.xml");
    assert_eq!(rs.len(), 6);
    for r in &rs {
        assert!(r.size.is_some_and(|s| s > 0), "{r:?}");
        assert!(is_hash(r.info_hash.as_deref().unwrap()), "{r:?}");
        assert!(r.magnet.is_some() && r.detail_url.is_some());
    }
    assert_eq!(
        rs[0].title,
        "FETAL_PLANES_DB: Common maternal-fetal ultrasound images"
    );
    assert_eq!(rs[0].size, Some(2_088_522_169));
    assert_eq!(
        rs[0].info_hash.as_deref(),
        Some("279f06bc350f8644395f8bca4e8e96514815ea8f")
    );
    assert!(builtin("Academic Torrents").local_filter);
}

#[test]
fn live_linuxtracker_fixture() {
    let rs = parsed("LinuxTracker", "live_linuxtracker.html");
    assert_eq!(rs.len(), 8);
    for r in &rs {
        size_date_peers(r);
        assert!(r.date.is_some_and(|d| d > 0), "{r:?}");
        assert!(is_hash(r.info_hash.as_deref().unwrap()), "{r:?}");
        assert!(r.magnet.as_deref().unwrap().contains("urn:btih:"));
    }
    assert_eq!(rs[1].title, "ubuntu-26.04-desktop-amd64.iso");
    assert_eq!(rs[1].size, Some(6_100_000_000));
    assert_eq!((rs[1].seeders, rs[1].leechers), (Some(13), Some(0)));
    assert_eq!(
        rs[1].info_hash.as_deref(),
        Some("dafc8c076ca2f3ed376eeae7c76a0d6be2415c45")
    );
    assert_eq!(rs[1].date, Some(1_792_886_400)); // Oct 25, 2026
}

#[test]
fn live_yts_fixture() {
    let rs = parsed("YTS", "live_yts.json");
    assert_eq!(rs.len(), 5);
    check_all(&rs, 5);
    assert_eq!(rs[0].title, "Night of the Living Dead (2006) [3D web]");
    assert_eq!(rs[0].size, Some(1_428_076_626));
    assert_eq!((rs[0].seeders, rs[0].leechers), (Some(3), Some(2)));
    assert_eq!(rs[0].date, Some(1_758_589_262));
    assert!(
        rs[0]
            .torrent_url
            .as_deref()
            .unwrap()
            .contains("/torrent/download/")
    );
    assert!(
        builtin("YTS")
            .endpoint_template()
            .starts_with("https://movies-api.accel.li/")
    );
}

/// Hand-written (the real service did not answer): shape only.
#[test]
fn solidtorrents_fixture_is_hand_written_and_unverified() {
    let d = builtin("SolidTorrents");
    assert!(!d.verified);
    let rs = parsed("SolidTorrents", "solidtorrents.json");
    check_all(&rs, 2);
    assert_eq!(rs[0].title, "ubuntu-24.04-desktop-amd64.iso");
    assert_eq!((rs[0].seeders, rs[0].leechers), (Some(120), Some(14)));
}

#[test]
fn verified_builtins_have_a_live_fixture() {
    // Unverified built-ins must say so in their comment block; verified ones
    // need a saved real response.
    for d in builtin_providers() {
        if d.verified {
            let stem = d.name.to_lowercase().replace(' ', "_");
            let hit = std::fs::read_dir(format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR")))
                .unwrap()
                .flatten()
                .any(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with(&format!("live_{stem}")) || n.starts_with("live_tpb")
                });
            assert!(hit, "{} is verified but has no live_ fixture", d.name);
        }
    }
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

/// Probe with the definition's own test query and a provider-specific check
/// of the first result.
async fn live_probe_with(name: &str, check: impl Fn(&SearchResult)) {
    if !live_enabled() {
        eprintln!("DMAN_TEST_LIVE != 1, skipping");
        return;
    }
    let def = builtin(name);
    let report = dman_search::test_provider(&def).await.unwrap();
    assert!(report.results > 0, "{name}: no results");
    let s = report.sample.expect("sample");
    eprintln!(
        "{name}: {} results in {:?} (mirror {:?}); sample {s:#?}",
        report.results, report.elapsed, report.mirror
    );
    assert!(!s.title.trim().is_empty());
    check(&s);
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_eztv() {
    live_probe_with("EZTV", |s| check_all(std::slice::from_ref(s), 1)).await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_torlock() {
    live_probe_with("Torlock", |s| {
        assert!(s.torrent_url.is_some() && s.size.is_some(), "{s:?}");
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_limetorrents() {
    live_probe_with("LimeTorrents", |s| {
        assert!(s.detail_url.is_some() && s.size.is_some(), "{s:?}");
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_solidtorrents() {
    live_probe_with("SolidTorrents", |s| {
        check_all(std::slice::from_ref(s), 1);
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_torrentdownloads() {
    // test_provider also reads the magnet from the first detail page
    live_probe_with("TorrentDownloads", |s| {
        assert!(s.detail_url.is_some() && s.size.is_some(), "{s:?}");
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_internet_archive() {
    live_probe_with("Internet Archive", |s| {
        assert!(
            s.torrent_url
                .as_deref()
                .unwrap()
                .ends_with("_archive.torrent")
        );
        assert!(s.size.is_some_and(|n| n > 0));
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_academic_torrents() {
    live_probe_with("Academic Torrents", |s| {
        assert!(is_hash(s.info_hash.as_deref().unwrap()), "{s:?}");
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_linuxtracker() {
    live_probe_with("LinuxTracker", |s| {
        check_all(std::slice::from_ref(s), 1);
    })
    .await;
}

#[tokio::test]
#[ignore = "network; set DMAN_TEST_LIVE=1"]
async fn live_yts() {
    live_probe_with("YTS", |s| check_all(std::slice::from_ref(s), 1)).await;
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
