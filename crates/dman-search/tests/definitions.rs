use dman_search::{
    HtmlSpec, HttpMethod, JsonSpec, Kind, ProviderDef, builtin_providers, export, import,
};

fn base(name: &str, kind: Kind) -> ProviderDef {
    ProviderDef {
        name: name.into(),
        kind,
        enabled: false,
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

fn html_spec() -> HtmlSpec {
    HtmlSpec {
        search_url: "https://x.test/s?q={query}".into(),
        row: "tr".into(),
        title: "a".into(),
        magnet: Some("a.m @href".into()),
        detail_link: None,
        size: None,
        seeders: None,
        leechers: None,
        date: None,
        date_format: None,
        follow_detail_for_magnet: false,
        detail_magnet: None,
        detail_limit: 10,
    }
}

fn json_spec() -> JsonSpec {
    JsonSpec {
        url: "https://x.test/api?q={query}".into(),
        method: HttpMethod::Get,
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
        detail_url: None,
        category: None,
    }
}

fn err(d: &ProviderDef) -> String {
    d.validate().unwrap_err().to_string()
}

#[test]
fn builtins_parse_validate_and_are_disabled() {
    let defs = builtin_providers();
    let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "1337x",
            "The Pirate Bay",
            "Nyaa",
            "Knaben",
            "YTS",
            "Torznab"
        ]
    );
    for d in &defs {
        d.validate().unwrap_or_else(|e| panic!("{e}"));
        assert!(!d.enabled, "{} must be off by default", d.name);
        // only providers with a saved live sample (tests/live.rs) may claim it
        let live = ["The Pirate Bay", "Nyaa", "Knaben"].contains(&d.name.as_str());
        assert_eq!(d.verified, live, "{} verified flag", d.name);
    }
    let kinds: Vec<_> = defs.iter().map(|d| d.kind.label()).collect();
    assert_eq!(kinds, ["html", "json", "feed", "json", "json", "torznab"]);
    // no API key material in definitions
    let text = export(&defs);
    assert!(text.contains("api_key_ref = \"torznab\""));
}

#[test]
fn export_import_round_trip() {
    let mut defs = builtin_providers();
    let mut custom = base("Mine", Kind::Html(html_spec()));
    custom.enabled = true;
    custom.rate_limit_per_min = Some(12);
    custom.test_query = Some("linux".into());
    custom.headers.insert("Cookie".into(), "a=b".into());
    defs.push(custom);
    let mut post = json_spec();
    post.method = HttpMethod::Post;
    post.body = Some(r#"{"q":"{query}"}"#.into());
    post.url = "https://x.test/api".into();
    defs.push(base("Poster", Kind::Json(post)));
    defs.push(base(
        "Idx",
        Kind::Torznab {
            url: "http://h/api".into(),
            api_key_ref: Some("k".into()),
            categories: vec![2000, 5000],
        },
    ));
    defs.push(base(
        "Feed",
        Kind::Feed {
            url: "https://f.test/rss".into(),
        },
    ));

    let text = export(&defs);
    assert!(text.contains("[[provider]]"));
    let back = import(&text).unwrap();
    assert_eq!(back, defs);
}

#[test]
fn defaults_apply_when_importing_minimal_toml() {
    let d = import(
        r#"
[[provider]]
name = "Tiny"
kind = "feed"
url = "https://f.test/rss?q={query}"
"#,
    )
    .unwrap();
    assert_eq!(d.len(), 1);
    assert!(!d[0].enabled);
    assert_eq!(d[0].timeout_secs, 15);
    assert_eq!(d[0].max_results, 100);
    assert_eq!(d[0].rate_limit_per_min, None);
}

#[test]
fn import_errors_are_clear() {
    let e = import("[[provider]]\nname = \"x\"\nkind = \"bogus\"\n")
        .unwrap_err()
        .to_string();
    assert!(e.contains("bogus") || e.contains("kind"), "{e}");
    let e =
        import("[[provider]]\nname = \"x\"\nkind = \"html\"\nsearch_url = \"https://a/{query}\"\n")
            .unwrap_err()
            .to_string();
    assert!(e.contains("row") || e.contains("title"), "{e}");
    let dup = "[[provider]]\nname='a'\nkind='feed'\nurl='https://f/rss'\n[[provider]]\nname='A'\nkind='feed'\nurl='https://f/rss'\n";
    assert!(import(dup).unwrap_err().to_string().contains("duplicate"));
    assert!(import("not toml [").is_err());
}

#[test]
fn validation_html() {
    let mut s = html_spec();
    assert!(base("h", Kind::Html(s.clone())).validate().is_ok());

    s.search_url = "https://x.test/s".into();
    assert!(err(&base("h", Kind::Html(s))).contains("must contain `{query}`"));

    let mut s = html_spec();
    s.row = "div >> p".into();
    let e = err(&base("h", Kind::Html(s)));
    assert!(e.contains("row") && e.contains("bad CSS selector"), "{e}");

    let mut s = html_spec();
    s.size = Some("td[".into());
    let e = err(&base("h", Kind::Html(s)));
    assert!(e.contains("size"), "{e}");

    let mut s = html_spec();
    s.follow_detail_for_magnet = true;
    assert!(err(&base("h", Kind::Html(s.clone()))).contains("detail_magnet"));
    s.detail_magnet = Some("a @href".into());
    assert!(err(&base("h", Kind::Html(s.clone()))).contains("detail_link"));
    s.detail_link = Some("a @href".into());
    assert!(base("h", Kind::Html(s)).validate().is_ok());

    let mut s = html_spec();
    s.magnet = None;
    assert!(err(&base("h", Kind::Html(s))).contains("magnet"));

    let mut s = html_spec();
    s.search_url = "ftp://x/{query}".into();
    assert!(err(&base("h", Kind::Html(s))).contains("scheme"));

    let mut s = html_spec();
    s.search_url = "https://x/{foo}/{query}".into();
    assert!(err(&base("h", Kind::Html(s))).contains("unknown placeholder"));
}

#[test]
fn validation_json_and_common() {
    assert!(base("j", Kind::Json(json_spec())).validate().is_ok());

    let mut j = json_spec();
    j.url = "https://x.test/api".into();
    assert!(err(&base("j", Kind::Json(j))).contains("{query}"));

    let mut j = json_spec();
    j.method = HttpMethod::Post;
    assert!(err(&base("j", Kind::Json(j))).contains("POST requires a body"));

    let mut j = json_spec();
    j.body = Some("{}".into());
    assert!(err(&base("j", Kind::Json(j))).contains("only allowed with method"));

    let mut j = json_spec();
    j.method = HttpMethod::Post;
    j.url = "https://x.test/api".into();
    j.body = Some(r#"{"q": {query}}"#.into());
    assert!(err(&base("j", Kind::Json(j))).contains("not valid JSON"));

    let mut j = json_spec();
    j.items = "items".into();
    assert!(err(&base("j", Kind::Json(j))).contains("JSON pointer"));

    let mut j = json_spec();
    j.title = "{nope}".into();
    assert!(err(&base("j", Kind::Json(j))).contains("title"));

    let mut j = json_spec();
    j.info_hash = None;
    assert!(err(&base("j", Kind::Json(j))).contains("info_hash"));

    let mut d = base(
        "",
        Kind::Feed {
            url: "https://f/rss".into(),
        },
    );
    assert!(err(&d).contains("name"));
    d.name = "ok".into();
    d.timeout_secs = 0;
    assert!(err(&d).contains("timeout_secs"));
    d.timeout_secs = 5;
    d.rate_limit_per_min = Some(0);
    assert!(err(&d).contains("rate_limit_per_min"));
    d.rate_limit_per_min = None;
    d.kind = Kind::Feed {
        url: "not a url".into(),
    };
    assert!(err(&d).contains("not a valid URL"));
}
