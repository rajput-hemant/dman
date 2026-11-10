//! Search page: big query field with provider pills, live results table with a
//! seed-health indicator and one-click download, recent queries, and a friendly
//! panel when no provider is enabled.

use dman_proto::search::{ProviderInfo, ProviderStatus, SearchHistoryEntry, SearchResultDto};
use maud::{Markup, html};

use super::icon;
use crate::fmt;

pub struct SearchView<'a> {
    pub search_id: Option<u64>,
    pub finished: bool,
    pub providers: &'a [(String, ProviderStatus)],
    pub results: &'a [SearchResultDto],
    /// `""` (best match), `size`, `seeders`, `date` or `title`.
    pub sort: &'a str,
}

/// Results in the requested order.
fn sorted<'a>(v: &SearchView<'a>) -> Vec<&'a SearchResultDto> {
    let mut r: Vec<&SearchResultDto> = v.results.iter().collect();
    match v.sort {
        "size" => r.sort_by_key(|a| std::cmp::Reverse(a.size)),
        "seeders" => r.sort_by_key(|a| std::cmp::Reverse(a.seeders)),
        "date" => r.sort_by_key(|a| std::cmp::Reverse(a.date)),
        "title" => r.sort_by_key(|a| a.title.to_lowercase()),
        _ => {}
    }
    r
}

fn sort_th(base: &str, v: &SearchView<'_>, key: &str, label: &str, class: &str) -> Markup {
    let Some(id) = v.search_id else {
        return html! { th class=(class) { (label) } };
    };
    let qs = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("id", &id.to_string())
        .append_pair("sort", key)
        .finish();
    let on = v.sort == key;
    html! {
        th class=(class) aria-sort=[on.then_some("descending")] {
            a href=(format!("{base}/search?{qs}")) { (label) @if on { " \u{25be}" } }
        }
    }
}

/// Seed health: `0` dead, `1` weak, `2` fair, `3` good.
fn health(seeders: Option<u32>) -> (u8, &'static str) {
    match seeders {
        None => (0, "unknown"),
        Some(0) => (0, "no seeders"),
        Some(1..=4) => (1, "weak"),
        Some(5..=49) => (2, "fair"),
        Some(_) => (3, "good"),
    }
}

/// Provider status chips plus the results table (the SSE `search` payload).
pub fn search_results(base: &str, csrf: Option<&str>, v: &SearchView<'_>) -> Markup {
    let running = v.search_id.is_some() && !v.finished;
    html! {
        @if v.search_id.is_some() {
            div.chips.status aria-label="Providers" {
                @for (name, st) in v.providers {
                    @match st {
                        ProviderStatus::Running => { span.chip.running { span.spin {} (name) ": searching\u{2026}" } }
                        ProviderStatus::Done { count, elapsed_ms, .. } => {
                            span.chip.done title=(format!("{elapsed_ms} ms")) { span.dot {} (name) ": " (count) " results" }
                        }
                        ProviderStatus::Failed { error } => { span.chip.failed title=(error) { span.dot {} (name) " failed: " (error) } }
                    }
                }
                @if running { span.chip.running { span.spin {} "Searching\u{2026}" } }
            }
        }
        @if v.results.is_empty() {
            @if v.finished {
                div.emptyst.small {
                    span.eicon { (icon(base, "search")) }
                    p { "No results. Try fewer or different words, or enable more providers in " a href=(format!("{base}/settings#providers")) { "Settings" } "." }
                }
            } @else if running {
                div.skel.rowskel aria-hidden="true" { @for _ in 0..4 { div.skrow { span.sk {} span.sk.short {} } } }
            }
        } @else {
            div.resulthead {
                p.help { strong { (v.results.len()) " results" } " \u{b7} Download starts right away; Options lets you pick a folder or files first." }
                @if let Some(id) = v.search_id {
                    nav.seg.links aria-label="Sort results" {
                        @for (k, l) in [("", "Best match"), ("seeders", "Seeders"), ("size", "Size"), ("date", "Newest")] {
                            a href=(format!("{base}/search?id={id}&sort={k}")) aria-current=[(v.sort == k).then_some("true")] { (l) }
                        }
                    }
                }
            }
            div.tblwrap { table.tbl.results {
                thead { tr {
                    (sort_th(base, v, "title", "Title", ""))
                    (sort_th(base, v, "size", "Size", "num"))
                    (sort_th(base, v, "seeders", "Health", ""))
                    (sort_th(base, v, "date", "Date", ""))
                    th { span.sr { "Actions" } }
                } }
                tbody { @for r in sorted(v) { (result_row(base, csrf, r)) } }
            } }
        }
    }
}

fn result_row(base: &str, csrf: Option<&str>, r: &SearchResultDto) -> Markup {
    // What to send to /search/add, and where "Options" leads.
    let mut vals: Vec<(&str, String)> = vec![("title", r.title.clone())];
    let options_href = if let Some(m) = &r.magnet {
        vals.push(("magnet", m.clone()));
        Some(add_href(base, m))
    } else if let Some(t) = &r.torrent_url {
        vals.push(("torrent_url", t.clone()));
        Some(add_href(base, t))
    } else if let Some(d) = &r.detail_url {
        vals.push(("detail_url", d.clone()));
        let p = r.sources.first().cloned().unwrap_or_default();
        vals.push(("provider", p.clone()));
        let qs = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("detail_url", d)
            .append_pair("provider", &p)
            .finish();
        Some(format!("{base}/search/resolve?{qs}"))
    } else {
        None
    };
    let add = format!("{base}/search/add");
    let (level, word) = health(r.seeders);
    let dash = || "\u{2013}".to_string();
    html! {
        tr {
            td.name data-label="Title" {
                span.rtitle { (r.title) }
                span.rsrc { (r.sources.join(", ")) }
            }
            td.num data-label="Size" { @if let Some(s) = r.size { (fmt::bytes(s)) } @else { (dash()) } }
            td data-label="Health" {
                span class=(format!("health h{level}")) title=(format!("Seed health: {word}")) {
                    span.hb {} span.hb {} span.hb {}
                    span.hn { span.seeds { (r.seeders.map_or_else(dash, |s| s.to_string())) } span.leech { " / " (r.leechers.map_or_else(dash, |s| s.to_string())) } }
                }
            }
            td.date data-label="Date" { @if let Some(d) = r.date { (fmt::timestamp(d).get(..10).unwrap_or("")) } }
            td.actions {
                @if let Some(href) = &options_href {
                    form.inline method="post" action=(add) hx-post=(add) hx-target="#toasts" hx-swap="beforeend" hx-disabled-elt="find button" {
                        @if let Some(c) = csrf { input type="hidden" name="_csrf" value=(c); }
                        @for (k, val) in &vals { input type="hidden" name=(k) value=(val); }
                        button.btn.small.primary type="submit" aria-label=(format!("Download {}", r.title)) { (icon(base, "dl")) span { "Download" } }
                    }
                    a.btn.small.ghost href=(href) aria-label=(format!("Download {} with options", r.title)) { "Options\u{2026}" }
                } @else { span.muted { "no link" } }
            }
        }
    }
}

fn add_href(base: &str, uri: &str) -> String {
    let qs = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("uri", uri)
        .finish();
    format!("{base}/add?{qs}")
}

/// One provider with an enable button (used by the no-provider panel).
fn enable_row(base: &str, csrf: &str, p: &ProviderInfo) -> Markup {
    html! {
        li.prov {
            div.pinfo {
                div.pname { strong { (p.name) } span.muted { (p.kind) }
                    @if p.verified { span.pill.complete { "verified" } } @else { span.pill.paused { "unverified" } } }
                @if let Some(d) = &p.description { p.help { (d) } }
            }
            form.inline method="post" action=(format!("{base}/settings/provider")) {
                input type="hidden" name="_csrf" value=(csrf);
                input type="hidden" name="name" value=(p.name);
                input type="hidden" name="enabled" value="true";
                input type="hidden" name="back" value="search";
                button.btn.small.primary type="submit" { "Enable" }
            }
        }
    }
}

pub fn search_page(
    base: &str,
    csrf: &str,
    query: &str,
    providers: &[ProviderInfo],
    history: &[SearchHistoryEntry],
    view: &SearchView<'_>,
) -> Markup {
    let enabled: Vec<&ProviderInfo> = providers.iter().filter(|p| p.enabled).collect();
    let used: Vec<&str> = view.providers.iter().map(|(n, _)| n.as_str()).collect();
    html! {
        div.pagehead { h1 { "Search" } }
        @if enabled.is_empty() {
            section.card.welcome {
                span.eicon { (icon(base, "search")) }
                h2 { "Turn on a search provider first" }
                p.sub { "dman searches public torrent indexes (\u{201c}providers\u{201d}). They are all off until you enable one. Only use sites that are legal where you live." }
                @if providers.is_empty() {
                    p.muted { "No providers are configured. Add one to the config file (" code { "[[provider]]" } ") and reload." }
                } @else {
                    ul.provs { @for p in providers { (enable_row(base, csrf, p)) } }
                }
                p.help { "You can change this any time in " a href=(format!("{base}/settings#providers")) { "Settings \u{2192} Search providers" } "." }
            }
        } @else {
            form.searchhero method="post" action=(format!("{base}/search")) role="search" {
                input type="hidden" name="_csrf" value=(csrf);
                div.bigsearch {
                    (icon(base, "search"))
                    input type="search" name="q" value=(query) placeholder="What are you looking for?" required aria-label="Search query" autofocus[query.is_empty()];
                    button.btn.primary type="submit" { "Search" }
                }
                div.chips.provpills {
                    span.lbl { "Providers" }
                    @for p in &enabled {
                        label.chip.toggle {
                            input.sr type="checkbox" name="providers" value=(p.name) checked[used.is_empty() || used.contains(&p.name.as_str())];
                            (icon(base, "check")) span { (p.name) }
                        }
                    }
                    a.chip.ghost href=(format!("{base}/settings#providers")) { "Manage" }
                }
            }
            @if !history.is_empty() {
                div.chips.recent {
                    span.lbl { "Recent" }
                    @for h in history.iter().take(12) {
                        form.inline method="post" action=(format!("{base}/search")) {
                            input type="hidden" name="_csrf" value=(csrf);
                            input type="hidden" name="q" value=(h.query);
                            @for p in &h.providers { input type="hidden" name="providers" value=(p); }
                            button.chip type="submit" title=(format!("{} results on {} UTC", h.result_count, fmt::timestamp(h.at))) { (h.query) }
                        }
                    }
                    form.inline method="post" action=(format!("{base}/search/history/clear")) {
                        input type="hidden" name="_csrf" value=(csrf);
                        button.linkbtn type="submit" { "Clear" }
                    }
                }
            }
        }
        div #search-results sse-swap="search" hx-swap="innerHTML" { (search_results(base, Some(csrf), view)) }
    }
}
