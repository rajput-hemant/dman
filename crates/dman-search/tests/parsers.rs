//! Parser tests against hand-written fixtures that mirror the real formats.

use dman_search::{
    ProviderDef, ProviderError, builtin_providers, feed, html, json, parse_response, torznab,
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

fn normalized(def: &ProviderDef, body: &str) -> Vec<dman_search::SearchResult> {
    let mut v = parse_response(def, body, true).unwrap();
    v.iter_mut()
        .for_each(dman_search::result::SearchResult::normalize);
    v
}

const H1: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn torznab_items() {
    let r = torznab::parse(&fx("torznab.xml")).unwrap();
    assert_eq!(r.len(), 3);
    let a = &r[0];
    assert_eq!(a.title, "Ubuntu 24.04 Desktop amd64 & extras");
    assert_eq!(a.seeders, Some(120));
    assert_eq!(a.leechers, Some(30), "leechers = peers - seeders");
    assert_eq!(a.size, Some(6_144_000_000));
    assert_eq!(a.date, Some(1_794_218_400));
    assert_eq!(a.info_hash.as_deref(), Some(&H1.to_uppercase()[..])); // raw, normalized later
    assert!(
        a.magnet
            .as_deref()
            .unwrap()
            .starts_with("magnet:?xt=urn:btih:0123")
    );
    assert!(a.torrent_url.as_deref().unwrap().contains("/1/download"));
    assert_eq!(
        a.detail_url.as_deref(),
        Some("https://indexer.example/details/42")
    );
    assert_eq!(a.category.as_deref(), Some("PC/ISO"));

    let b = &r[1];
    assert_eq!(b.title, "Debian 12 netinst");
    assert_eq!((b.seeders, b.leechers), (Some(3), Some(1)));
    assert_eq!(b.size, Some(400_000_000), "size from enclosure length");
    assert_eq!(b.date, Some(1_794_126_600));
    assert_eq!(r[2].seeders, None);
}

#[test]
fn torznab_error_document() {
    match torznab::parse(&fx("torznab_error.xml")) {
        Err(ProviderError::Remote(m)) => assert!(
            m.contains("100") && m.contains("Incorrect user credentials"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn feed_rejects_html_and_garbage() {
    let e = feed::parse(&fx("cloudflare.html")).unwrap_err().to_string();
    assert!(
        e.contains("not an RSS/Atom feed") || e.contains("invalid XML"),
        "{e}"
    );
    assert!(feed::parse("").is_err());
    assert!(feed::parse("<rss><channel><item><title>x</item></channel></rss>").is_err());
}

#[test]
fn nyaa_feed_namespaced_fields() {
    let r = normalized(&builtin("Nyaa"), &fx("nyaa.xml"));
    assert_eq!(r.len(), 2);
    let a = &r[0];
    assert_eq!(
        a.title,
        "[SubsPlease] Sousou no Frieren - 28 (1080p) [ABCD1234].mkv"
    );
    assert_eq!(a.seeders, Some(1523));
    assert_eq!(a.leechers, Some(42));
    assert_eq!(a.size, Some(1_503_238_554));
    assert_eq!(a.info_hash.as_deref(), Some(H1));
    assert_eq!(
        a.torrent_url.as_deref(),
        Some("https://nyaa.si/download/1000001.torrent")
    );
    assert_eq!(
        a.detail_url.as_deref(),
        Some("https://nyaa.si/view/1000001")
    );
    assert_eq!(a.category.as_deref(), Some("Anime - English-translated"));
    assert_eq!(a.date, Some(1_772_811_062));
    assert!(
        a.magnet
            .as_deref()
            .unwrap()
            .starts_with(&format!("magnet:?xt=urn:btih:{H1}&dn="))
    );
    assert_eq!(r[1].size, Some(48_533_130_445));
}

#[test]
fn atom_feed() {
    let r = feed::parse(&fx("atom.xml")).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].title, "Some Linux ISO");
    assert_eq!(
        r[0].torrent_url.as_deref(),
        Some("https://tracker.example/dl/9")
    );
    assert_eq!(
        r[0].detail_url.as_deref(),
        Some("https://tracker.example/t/9")
    );
    assert_eq!(r[0].size, Some(734_003_200));
    assert_eq!(r[0].date, Some(1_794_135_600)); // 2026-11-08T12:00+01:00
    assert_eq!(r[0].category.as_deref(), Some("Linux"));
    assert!(
        r[1].magnet
            .as_deref()
            .unwrap()
            .starts_with("magnet:?xt=urn:btih:2123")
    );
}

#[test]
fn html_1337x_rows() {
    let def = builtin("1337x");
    let r = normalized(&def, &fx("1337x_search.html"));
    assert_eq!(r.len(), 2, "row without a title link is skipped");
    assert_eq!(r[0].title, "Ubuntu 24.04 Desktop amd64");
    assert_eq!(r[0].seeders, Some(1204));
    assert_eq!(r[0].leechers, Some(31));
    assert_eq!(r[0].size, Some(5_700_000_000));
    assert_eq!(
        r[0].detail_url.as_deref(),
        Some("https://1337x.to/torrent/5551111/Ubuntu-24-04-Desktop-amd64/")
    );
    assert!(r[0].magnet.is_none());
    assert_eq!(r[1].size, Some(2_600_000_000));
}

#[test]
fn html_detail_magnet() {
    let def = builtin("1337x");
    let dman_search::Kind::Html(h) = &def.kind else {
        panic!()
    };
    let m = html::parse_detail_magnet(h, &fx("1337x_detail.html")).unwrap();
    assert!(m.starts_with(&format!("magnet:?xt=urn:btih:{H1}")));
    let e = html::parse_detail_magnet(h, &fx("1337x_detail_nomagnet.html"))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("detail_magnet selector") && e.contains("no magnet"),
        "{e}"
    );
}

#[test]
fn html_error_messages_are_precise() {
    let def = builtin("1337x");
    let e = parse_response(&def, &fx("cloudflare.html"), true)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("row selector `table.table-list tbody tr` matched 0 elements"),
        "{e}"
    );
    // lenient mode: no rows is simply "no results"
    assert!(
        parse_response(&def, &fx("cloudflare.html"), false)
            .unwrap()
            .is_empty()
    );

    let mut bad_title = def.clone();
    if let dman_search::Kind::Html(h) = &mut bad_title.kind {
        h.title = "td.nonexistent".into();
    }
    let e = parse_response(&bad_title, &fx("1337x_search.html"), false)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("title selector `td.nonexistent` matched nothing in any of the 3 rows"),
        "{e}"
    );
}

#[test]
fn html_attribute_and_date_format() {
    let mut def = builtin("1337x");
    if let dman_search::Kind::Html(h) = &mut def.kind {
        h.row = "tr:has(td.name)".into();
        h.date = Some("td.coll-date".into());
    }

    let html_doc = r#"<ul><li class="r" data-hash="0123456789abcdef0123456789abcdef01234567"><a class="t" href="/t/1">First</a><span class="d">2026-11-09 10:00</span><b class="s">1,5 GB</b></li></ul>"#;
    let spec = dman_search::HtmlSpec {
        search_url: "https://x.test/?q={query}".into(),
        row: "li.r".into(),
        title: "a.t".into(),
        magnet: Some("@data-hash".into()),
        info_hash: None,
        detail_link: Some("a.t @href".into()),
        size: Some("b.s".into()),
        seeders: None,
        leechers: None,
        date: Some("span.d".into()),
        date_format: Some("%Y-%m-%d %H:%M".into()),
        follow_detail_for_magnet: false,
        detail_magnet: None,
        detail_limit: 10,
    };
    let base = url::Url::parse("https://x.test/?q=a").unwrap();
    let r = html::parse(&spec, html_doc, &base, true).unwrap();
    assert_eq!(r[0].date, Some(1_794_218_400));
    assert_eq!(r[0].size, Some(1_500_000_000));
    assert_eq!(r[0].magnet.as_deref(), Some(H1));
    assert_eq!(r[0].detail_url.as_deref(), Some("https://x.test/t/1"));
}

#[test]
fn tpb_json() {
    let def = builtin("The Pirate Bay");
    let r = normalized(&def, &fx("tpb.json"));
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].title, "Ubuntu 24.04 Desktop amd64");
    assert_eq!(r[0].info_hash.as_deref(), Some(H1));
    assert_eq!(r[0].seeders, Some(321));
    assert_eq!(r[0].leechers, Some(15));
    assert_eq!(r[0].size, Some(6_144_000_000));
    assert_eq!(r[0].date, Some(1_794_218_400));
    assert_eq!(
        r[0].detail_url.as_deref(),
        Some("https://thepiratebay.org/description.php?id=70000001")
    );
    let m = r[0].magnet.as_deref().unwrap();
    assert!(
        m.to_lowercase().starts_with(&format!(
            "magnet:?xt=urn:btih:{H1}&dn=ubuntu%2024.04%20desktop%20amd64&tr=udp"
        )),
        "{m}"
    );
}

#[test]
fn tpb_no_results_placeholder_is_dropped() {
    let def = builtin("The Pirate Bay");
    assert!(
        parse_response(&def, &fx("tpb_empty.json"), true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn knaben_json() {
    let r = normalized(&builtin("Knaben"), &fx("knaben.json"));
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].seeders, Some(400));
    assert_eq!(r[0].leechers, Some(25));
    assert_eq!(r[0].size, Some(6_144_000_000));
    assert_eq!(r[0].date, Some(1_794_132_225));
    assert_eq!(r[0].category.as_deref(), Some("Software"));
    assert_eq!(r[0].info_hash.as_deref(), Some(H1));
    // second hit: hash null, hash recovered from a base32 magnet
    assert_eq!(
        r[1].info_hash.as_deref(),
        Some("000102030405060708090a0b0c0d0e0f10111213")
    );
}

#[test]
fn yts_one_result_per_torrent() {
    let r = normalized(&builtin("YTS"), &fx("yts.json"));
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].title, "Test Movie (2020) [720p web]");
    assert_eq!(r[1].title, "Test Movie (2020) [1080p bluray]");
    assert_eq!(r[1].seeders, Some(120));
    assert_eq!(r[1].leechers, Some(20));
    assert_eq!(r[1].size, Some(1_717_986_918));
    assert_eq!(r[1].date, Some(1_588_420_800));
    assert_eq!(
        r[1].info_hash.as_deref(),
        Some("bbbb23456789abcdef0123456789abcdef01234b")
    );
    assert_eq!(
        r[1].detail_url.as_deref(),
        Some("https://yts.mx/movies/test-movie-2020")
    );
    assert_eq!(
        r[1].torrent_url.as_deref(),
        Some("https://yts.mx/torrent/download/BBBB23456789ABCDEF0123456789ABCDEF01234")
    );
    assert_eq!(r[1].category.as_deref(), Some("movies"));
    let m = r[1].magnet.as_deref().unwrap();
    assert!(
        m.contains("xt=urn:btih:BBBB")
            && m.contains("dn=Test%20Movie%20%282020%29%20[1080p]")
            && m.contains("&tr="),
        "{m}"
    );
}

#[test]
fn yts_empty_ok_lenient_error_strict() {
    let def = builtin("YTS");
    assert!(
        parse_response(&def, &fx("yts_empty.json"), false)
            .unwrap()
            .is_empty()
    );
    let e = parse_response(&def, &fx("yts_empty.json"), true)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("items pointer `/data/movies` not found") && e.contains("status"),
        "{e}"
    );
}

#[test]
fn json_errors_have_context() {
    let def = builtin("Knaben");
    let e = parse_response(&def, "<html>Cloudflare</html>", true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("invalid JSON") && e.contains("<html>"), "{e}");
    let e = parse_response(&def, r#"{"hits": {"a": 1}}"#, true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not point to an array"), "{e}");
    let e = parse_response(&def, r#"{"hits": [{"nope": 1}]}"#, true)
        .unwrap_err()
        .to_string();
    assert!(e.contains("title mapping"), "{e}");
    let _ = json::eval("/x", &serde_json::json!({}), &serde_json::json!({}));
}

#[test]
fn html_info_hash_selector_finds_the_hash_in_a_link() {
    let doc = r#"<table><tr class="r"><td><a class="t" href="/torrents/0123456789ABCDEF0123456789abcdef01234567/">First</a></td><td class="s">1.5 GB</td></tr>
        <tr class="r"><td><a class="t" href="/torrents/not-a-hash/">Second</a></td><td class="s">2 GB</td></tr></table>"#;
    let spec = dman_search::HtmlSpec {
        search_url: "https://x.test/?q={query}".into(),
        row: "tr.r".into(),
        title: "a.t".into(),
        magnet: None,
        info_hash: Some("a.t @href".into()),
        detail_link: Some("a.t @href".into()),
        size: Some("td.s".into()),
        seeders: None,
        leechers: None,
        date: None,
        date_format: None,
        follow_detail_for_magnet: false,
        detail_magnet: None,
        detail_limit: 10,
    };
    let base = url::Url::parse("https://x.test/?q=a").unwrap();
    let mut r = html::parse(&spec, doc, &base, true).unwrap();
    r.iter_mut().for_each(dman_search::SearchResult::normalize);
    assert_eq!(r[0].info_hash.as_deref(), Some(H1));
    assert!(r[0].magnet.as_deref().unwrap().contains(H1));
    assert!(r[1].info_hash.is_none() && r[1].magnet.is_none());
}

#[test]
fn feed_reads_peers_and_size_from_the_description() {
    let xml = r#"<rss version="2.0"><channel><item><title>T</title>
        <link>https://x.test/t/1</link>
        <description><![CDATA[
Category: Music
Seeds: 7
Leechers: 3
Size: 2.5 GB
]]></description></item></channel></rss>"#;
    let r = dman_search::feed::parse(xml).unwrap();
    assert_eq!((r[0].seeders, r[0].leechers), (Some(7), Some(3)));
    assert_eq!(r[0].size, Some(2_500_000_000));
}

#[test]
fn challenge_pages_are_recognised_but_normal_pages_are_not() {
    use dman_search::util::is_block_page;
    assert!(is_block_page(&fx("cloudflare.html")));
    assert!(is_block_page(
        "<!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title></head></html>"
    ));
    assert!(is_block_page(
        "<html><body><form id=\"challenge-form\" action=\"/cdn-cgi/l/chk_jschl\"></form></body></html>"
    ));
    // Cloudflare-fronted sites embed a passive script on every page
    assert!(!is_block_page(
        "<html><head><title>Torrents</title></head><body><script src=\"/cdn-cgi/challenge-platform/scripts/jsd/main.js\"></script></body></html>"
    ));
    assert!(!is_block_page(r#"{"title":"Just a moment..."}"#));
    assert!(!is_block_page(&fx("nyaa.xml")));
}
