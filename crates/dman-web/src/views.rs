//! Compile-time HTML templates (maud). Everything interpolated is escaped by maud, so
//! titles from search providers and remote file names cannot inject markup.

use std::collections::BTreeMap;

use dman_proto::method::ScheduleStatus;
use dman_proto::search::{ProviderInfo, ProviderStatus, SearchHistoryEntry, SearchResultDto};
use dman_proto::{GlobalStats, Job, JobDetail, JobState, Limits, SourceKind};
use maud::{DOCTYPE, Markup, html};

use crate::fmt;

/// Which nav entry is highlighted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Downloads,
    Search,
    Add,
    Settings,
    None,
}

/// Everything the page chrome needs.
pub struct Layout<'a> {
    pub base: &'a str,
    pub title: &'a str,
    pub csrf: &'a str,
    pub nav: Nav,
    pub exposed_warning: bool,
    pub show_logout: bool,
    pub stats: Option<&'a GlobalStats>,
    /// Query string for `/events` (`rows=1`, `search=3`); `None` = no live updates.
    pub sse: Option<String>,
}

fn nav_item(l: &Layout<'_>, which: Nav, path: &str, label: &str) -> Markup {
    html! {
        a href=(format!("{}{}", l.base, path)) class=(if l.nav == which { "on" } else { "" }) { (label) }
    }
}

/// Wraps `content` in the page chrome: nav, stats line, toast area, SSE connection.
pub fn page(l: &Layout<'_>, heading: &str, content: Markup) -> Markup {
    let headers = serde_json::json!({ "X-CSRF-Token": l.csrf }).to_string();
    let sse_url = l.sse.as_ref().map(|q| format!("{}/events?{}", l.base, q));
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false,"selfRequestsOnly":true}"#;
                meta name="color-scheme" content="light dark";
                title { (heading) " - " (l.title) }
                link rel="stylesheet" href=(format!("{}/static/app.css", l.base));
                script src=(format!("{}/static/htmx.min.js", l.base)) defer {}
                script src=(format!("{}/static/sse.js", l.base)) defer {}
            }
            body hx-ext="sse" hx-headers=(headers) {
                div.app sse-connect=[sse_url.as_deref()] {
                    header.top {
                        a.brand href=(format!("{}/", l.base)) { (l.title) }
                        nav {
                            (nav_item(l, Nav::Downloads, "/", "Downloads"))
                            (nav_item(l, Nav::Search, "/search", "Search"))
                            (nav_item(l, Nav::Add, "/add", "Add"))
                            (nav_item(l, Nav::Settings, "/settings", "Settings"))
                            @if l.show_logout {
                                form.logout method="post" action=(format!("{}/logout", l.base)) {
                                    input type="hidden" name="_csrf" value=(l.csrf);
                                    button type="submit" { "Log out" }
                                }
                            }
                        }
                    }
                    @if l.exposed_warning {
                        div.banner role="alert" {
                            "Warning: this web UI is reachable from the network and no access token is configured. Anyone who can reach this address can control your downloads."
                        }
                    }
                    div #stats.stats sse-swap="stats" hx-swap="innerHTML" {
                        @if let Some(s) = l.stats { (stats_inner(s)) }
                    }
                    div #toast.toast sse-swap="message" hx-swap="innerHTML" aria-live="polite" {}
                    main {
                        h1 { (heading) }
                        (content)
                    }
                }
            }
        }
    }
}

/// Inner HTML of the stats line.
pub fn stats_inner(s: &GlobalStats) -> Markup {
    html! {
        span title="Download speed" { "\u{2193} " (fmt::speed(s.download_speed)) }
        span title="Upload speed" { "\u{2191} " (fmt::speed(s.upload_speed)) }
        span { (s.num_active) " active" }
        span { (s.num_waiting) " waiting" }
        span { (s.num_stopped) " stopped" }
        @if let Some(d) = s.disk_free { span { "free " (fmt::bytes(d)) } }
        @if let Some(sch) = &s.schedule {
            @if sch.paused { span.warn { "schedule: paused" } }
            @else if let Some(a) = &sch.active { span { "schedule: " (a) } }
        }
    }
}

/// Toast content: a status message with a severity class.
pub fn toast(level: &str, text: &str) -> Markup {
    html! { div class=(format!("msg {level}")) { (text) } }
}

// ---------------------------------------------------------------- downloads

/// Table state carried in the query string.
#[derive(Debug, Clone, Default)]
pub struct TableQuery {
    pub q: String,
    pub sort: String,
    pub desc: bool,
    pub all: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Section {
    Active,
    Waiting,
    Paused,
    Stopped,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Active => "Active",
            Section::Waiting => "Waiting",
            Section::Paused => "Paused",
            Section::Stopped => "Stopped",
        }
    }
}

/// Section of a job in the list; `None` = not shown.
pub fn section_of(j: &Job) -> Option<Section> {
    match j.state {
        JobState::Active | JobState::Seeding => Some(Section::Active),
        JobState::Waiting => Some(Section::Waiting),
        JobState::Paused => Some(Section::Paused),
        JobState::Complete | JobState::Error => Some(Section::Stopped),
        JobState::Removed => None,
    }
}

pub fn state_name(s: JobState) -> &'static str {
    match s {
        JobState::Waiting => "waiting",
        JobState::Active => "active",
        JobState::Seeding => "seeding",
        JobState::Paused => "paused",
        JobState::Complete => "complete",
        JobState::Error => "error",
        JobState::Removed => "removed",
    }
}

fn matches_filter(j: &Job, q: &str) -> bool {
    let hay = format!("{} {} {}", j.name, state_name(j.state), j.source).to_lowercase();
    q.split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

fn sort_jobs(jobs: &mut [&Job], q: &TableQuery) {
    use std::cmp::Ordering;
    let key = q.sort.as_str();
    jobs.sort_by(|a, b| {
        let o = match key {
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            "size" => a.total_bytes.cmp(&b.total_bytes),
            "progress" => a.progress().total_cmp(&b.progress()),
            "speed" => a.download_speed.cmp(&b.download_speed),
            "ratio" => a.ratio().total_cmp(&b.ratio()),
            "date" => a.added_at.cmp(&b.added_at),
            "eta" => match (a.eta_secs(), b.eta_secs()) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            _ => {
                return a
                    .queue_position
                    .unwrap_or(u32::MAX)
                    .cmp(&b.queue_position.unwrap_or(u32::MAX))
                    .then(b.added_at.cmp(&a.added_at))
                    .then(b.id.cmp(&a.id));
            }
        };
        let o = o.then(a.id.cmp(&b.id));
        if q.desc { o.reverse() } else { o }
    });
}

fn btn(base: &str, id: u64, action: &str, label: &str, vals: Option<(&str, &str)>) -> Markup {
    let url = format!("{base}/jobs/{id}/{action}");
    let vals = vals.map(|(k, v)| serde_json::json!({ k: v }).to_string());
    html! {
        button type="button" hx-post=(url) hx-vals=[vals.as_deref()]
            hx-target="closest tr" hx-swap="outerHTML" title=(label) { (label) }
    }
}

/// One `<tr>` of the downloads table. The same markup is pushed over SSE.
pub fn job_row(base: &str, j: &Job) -> Markup {
    let id = j.id.0;
    let pct = j.progress() * 100.0;
    let name = if j.name.is_empty() {
        &j.source
    } else {
        &j.name
    };
    let peers = match (j.connections, j.seeders) {
        (c, Some(s)) => format!("{c} / {s}"),
        (c, None) => c.to_string(),
    };
    let queued = j.state == JobState::Waiting;
    let show_ratio = matches!(j.kind, SourceKind::Magnet | SourceKind::Torrent);
    let list_reorder = |label: &str, how: &str| {
        html! {
            button type="button" hx-post=(format!("{base}/jobs/{id}/reorder"))
                hx-vals=(serde_json::json!({ "how": how }).to_string())
                hx-target="#jobs" hx-swap="outerHTML" title=(format!("Move {how}")) { (label) }
        }
    };
    html! {
        tr id=(format!("job-{id}")) class=(format!("job st-{}", state_name(j.state)))
            sse-swap=(format!("job-{id}")) hx-swap="outerHTML" {
            td.name data-label="Name" {
                a href=(format!("{base}/jobs/{id}")) { (name) }
                @if let Some(e) = &j.error { div.err { (e) } }
            }
            td data-label="State" { span class=(format!("badge {}", state_name(j.state))) { (state_name(j.state)) } }
            td.prog data-label="Progress" {
                progress max="1000" value=((pct * 10.0).round() as u32) {}
                " " span.pct { (format!("{pct:.1}%")) }
            }
            td.num data-label="Down" { (fmt::speed(j.download_speed)) }
            td.num data-label="Up" { (fmt::speed(j.upload_speed)) }
            td.num data-label="ETA" { (fmt::eta(j.eta_secs())) }
            td.num data-label="Size" { (fmt::bytes(j.total_bytes)) }
            td.num data-label="Peers" { (peers) }
            td.num data-label="Ratio" { @if show_ratio { (format!("{:.2}", j.ratio())) } @else { "-" } }
            td.actions {
                @match j.state {
                    JobState::Active | JobState::Seeding | JobState::Waiting => { (btn(base, id, "pause", "Pause", None)) }
                    JobState::Paused => { (btn(base, id, "resume", "Resume", None)) }
                    JobState::Error => { (btn(base, id, "resume", "Retry", None)) }
                    _ => {}
                }
                @if queued {
                    (list_reorder("\u{2912}", "top"))
                    (list_reorder("\u{2191}", "up"))
                    (list_reorder("\u{2193}", "down"))
                    (list_reorder("\u{2913}", "bottom"))
                }
                @if j.state.is_stopped() { (btn(base, id, "purge", "Purge", None)) }
                a.button href=(format!("{base}/jobs/{id}/remove")) { "Remove" }
            }
        }
    }
}

fn sort_link(base: &str, q: &TableQuery, key: &str, label: &str) -> Markup {
    let desc = q.sort == key && !q.desc;
    let qs = table_qs(&TableQuery {
        sort: key.into(),
        desc,
        ..q.clone()
    });
    html! {
        th { a href=(format!("{base}/{qs}")) hx-get=(format!("{base}/downloads/table{qs}"))
                hx-target="#jobs" hx-swap="outerHTML" {
            (label) @if q.sort == key { (if q.desc { " \u{25be}" } else { " \u{25b4}" }) }
        } }
    }
}

/// `?q=..&sort=..` for the current table state.
pub fn table_qs(q: &TableQuery) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    if !q.q.is_empty() {
        s.append_pair("q", &q.q);
    }
    if !q.sort.is_empty() {
        s.append_pair("sort", &q.sort);
        s.append_pair("dir", if q.desc { "desc" } else { "asc" });
    }
    if q.all {
        s.append_pair("all", "1");
    }
    let s = s.finish();
    if s.is_empty() {
        String::new()
    } else {
        format!("?{s}")
    }
}

/// The `#jobs` container: refreshes itself when the server announces a changed layout.
pub fn jobs_table(base: &str, jobs: &[Job], q: &TableQuery) -> Markup {
    const STOPPED_PAGE: usize = 50;
    let mut shown: Vec<&Job> = jobs
        .iter()
        .filter(|j| section_of(j).is_some() && matches_filter(j, &q.q))
        .collect();
    sort_jobs(&mut shown, q);
    let mut sections: BTreeMap<Section, Vec<&Job>> = BTreeMap::new();
    for j in shown {
        if let Some(s) = section_of(j) {
            sections.entry(s).or_default().push(j);
        }
    }
    let refresh = format!("{base}/downloads/table{}", table_qs(q));
    html! {
        div #jobs.jobs hx-get=(refresh) hx-trigger="sse:jobs-changed" hx-swap="outerHTML" {
            @if sections.is_empty() {
                p.empty { "No downloads" @if !q.q.is_empty() { " match the filter" } "." }
            } @else {
                table.list {
                    thead { tr {
                        (sort_link(base, q, "name", "Name"))
                        th { "State" }
                        (sort_link(base, q, "progress", "Progress"))
                        (sort_link(base, q, "speed", "Down"))
                        th { "Up" }
                        (sort_link(base, q, "eta", "ETA"))
                        (sort_link(base, q, "size", "Size"))
                        th { "Peers" }
                        (sort_link(base, q, "ratio", "Ratio"))
                        th { "" }
                    } }
                    @for (sec, list) in &sections {
                        tbody {
                            tr.sec { th colspan="10" { (sec.label()) " (" (list.len()) ")" } }
                            @let limit = if *sec == Section::Stopped && !q.all { STOPPED_PAGE } else { usize::MAX };
                            @for j in list.iter().take(limit) { (job_row(base, j)) }
                            @if list.len() > limit {
                                tr { td colspan="10" {
                                    a href=(format!("{base}/{}", table_qs(&TableQuery { all: true, ..q.clone() })))
                                        hx-get=(format!("{base}/downloads/table{}", table_qs(&TableQuery { all: true, ..q.clone() })))
                                        hx-target="#jobs" hx-swap="outerHTML" {
                                        "Show all " (list.len()) " stopped"
                                    }
                                } }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn downloads_page(base: &str, jobs: &[Job], q: &TableQuery) -> Markup {
    let sort_opts = [
        ("", "Queue order"),
        ("name", "Name"),
        ("size", "Size"),
        ("progress", "Progress"),
        ("speed", "Speed"),
        ("eta", "ETA"),
        ("date", "Date added"),
        ("ratio", "Ratio"),
    ];
    html! {
        div.toolbar {
            form.filter method="get" action=(format!("{base}/")) hx-get=(format!("{base}/downloads/table"))
                hx-target="#jobs" hx-swap="outerHTML" hx-trigger="input delay:250ms, change, submit" {
                input type="search" name="q" value=(q.q) placeholder="Filter" aria-label="Filter downloads";
                select name="sort" aria-label="Sort by" {
                    @for (v, l) in sort_opts { option value=(v) selected[q.sort == v] { (l) } }
                }
                select name="dir" aria-label="Direction" {
                    option value="asc" selected[!q.desc] { "Ascending" }
                    option value="desc" selected[q.desc] { "Descending" }
                }
                button type="submit" { "Apply" }
            }
            div.bulk {
                button type="button" hx-post=(format!("{base}/queue/pause-all")) hx-target="#toast" hx-swap="innerHTML" { "Pause all" }
                button type="button" hx-post=(format!("{base}/queue/resume-all")) hx-target="#toast" hx-swap="innerHTML" { "Resume all" }
            }
        }
        (jobs_table(base, jobs, q))
    }
}

// ------------------------------------------------------------------- detail

pub fn detail_live(base: &str, d: &JobDetail) -> Markup {
    let j = &d.job;
    html! {
        div #detail-live hx-get=(format!("{base}/jobs/{}/live", j.id.0)) hx-trigger="every 3s" hx-swap="outerHTML" {
            table.kv {
                tr { th { "State" } td { span class=(format!("badge {}", state_name(j.state))) { (state_name(j.state)) } @if let Some(e) = &j.error { " " span.err { (e) } } } }
                tr { th { "Progress" } td {
                    progress max="1000" value=((j.progress() * 1000.0).round() as u32) {}
                    " " (format!("{:.1}%", j.progress() * 100.0)) " (" (fmt::bytes(j.completed_bytes)) " / " (fmt::bytes(j.total_bytes)) ")"
                } }
                tr { th { "Speed" } td { "\u{2193} " (fmt::speed(j.download_speed)) " \u{2191} " (fmt::speed(j.upload_speed)) " ETA " (fmt::eta(j.eta_secs())) } }
                tr { th { "Connections" } td { (j.connections) @if let Some(s) = j.seeders { " (" (s) " seeders)" } } }
                tr { th { "Uploaded" } td { (fmt::bytes(j.uploaded_bytes)) " (ratio " (format!("{:.2}", j.ratio())) ")" } }
            }
            @if !d.files.is_empty() {
                h2 { "Files" }
                table.list {
                    thead { tr { th { "File" } th { "Size" } th { "Done" } th { "Selected" } } }
                    tbody { @for f in &d.files { tr {
                        td.name data-label="File" { (f.path) }
                        td.num data-label="Size" { (fmt::bytes(f.length)) }
                        td.num data-label="Done" { (format!("{:.1}%", if f.length == 0 { 0.0 } else { f.completed as f64 * 100.0 / f.length as f64 })) }
                        td data-label="Selected" { (if f.selected { "yes" } else { "no" }) }
                    } } }
                }
            }
            @if !d.peers.is_empty() {
                h2 { "Peers (" (d.peers.len()) ")" }
                table.list {
                    thead { tr { th { "Address" } th { "Down" } th { "Up" } th { "Progress" } th { "Seeder" } } }
                    tbody { @for p in &d.peers { tr {
                        td data-label="Address" { (p.ip) ":" (p.port) }
                        td.num data-label="Down" { (fmt::speed(p.download_speed)) }
                        td.num data-label="Up" { (fmt::speed(p.upload_speed)) }
                        td.num data-label="Progress" { (format!("{:.0}%", p.progress * 100.0)) }
                        td data-label="Seeder" { (if p.seeder { "yes" } else { "" }) }
                    } } }
                }
            }
            @if !d.trackers.is_empty() {
                h2 { "Trackers" }
                table.list {
                    thead { tr { th { "URL" } th { "Tier" } th { "Status" } } }
                    tbody { @for t in &d.trackers { tr {
                        td.name data-label="URL" { (t.url) }
                        td data-label="Tier" { (t.tier) }
                        td data-label="Status" { "unknown" }
                    } } }
                }
            }
        }
    }
}

fn rate_value(v: Option<u64>) -> String {
    match v {
        None => String::new(),
        Some(0) => "unlimited".into(),
        Some(n) => format!("{}K", n / 1024),
    }
}

pub fn detail_page(base: &str, csrf: &str, d: &JobDetail) -> Markup {
    let j = &d.job;
    let id = j.id.0;
    let can_rename = j.state.is_renamable_in_place() || j.state == JobState::Complete;
    html! {
        p.crumbs { a href=(format!("{base}/")) { "\u{2190} Downloads" } }
        h2.title { (j.name) }
        table.kv {
            tr { th { "Source" } td.wrap { (j.source) } }
            tr { th { "Directory" } td.wrap { (j.dir) } }
            @if let Some(h) = d.info_hash.as_ref().or(j.info_hash.as_ref()) { tr { th { "Info hash" } td.wrap { (h) } } }
            @if let Some(c) = &d.comment { tr { th { "Comment" } td.wrap { (c) } } }
            tr { th { "Added" } td { (fmt::timestamp(j.added_at)) " UTC" } }
            @if let Some(c) = j.completed_at { tr { th { "Completed" } td { (fmt::timestamp(c)) " UTC" } } }
        }
        div.actions {
            @match j.state {
                JobState::Paused => { button type="button" hx-post=(format!("{base}/jobs/{id}/resume")) hx-target="#toast" hx-swap="innerHTML" { "Resume" } }
                JobState::Error => { button type="button" hx-post=(format!("{base}/jobs/{id}/resume")) hx-target="#toast" hx-swap="innerHTML" { "Retry" } }
                JobState::Complete | JobState::Removed => {}
                _ => { button type="button" hx-post=(format!("{base}/jobs/{id}/pause")) hx-target="#toast" hx-swap="innerHTML" { "Pause" } }
            }
            a.button href=(format!("{base}/jobs/{id}/remove")) { "Remove\u{2026}" }
        }
        (detail_live(base, d))
        h2 { "Rename" }
        @if can_rename {
            form method="post" action=(format!("{base}/jobs/{id}/rename")) {
                input type="hidden" name="_csrf" value=(csrf);
                @if d.files.len() > 1 && j.state != JobState::Complete {
                    label { "File " select name="file_index" {
                        @for f in &d.files { option value=(f.index) { (f.path) } }
                    } }
                }
                input type="text" name="name" value=(j.name) required maxlength="255" aria-label="New name";
                button type="submit" { "Rename" }
            }
            @if j.state == JobState::Complete { p.hint { "The files are moved on disk and the rename is recorded." } }
        } @else {
            p.hint { "Renaming is only possible while the download is waiting or paused, or after it completed. An active transfer cannot be renamed." }
        }
        @if !d.renames.is_empty() {
            ul { @for r in &d.renames { li { (r.from) " \u{2192} " (r.to) " (" (fmt::timestamp(r.at)) ")" } } }
        }
        h2 { "Speed limits" }
        form method="post" action=(format!("{base}/jobs/{id}/limits")) {
            input type="hidden" name="_csrf" value=(csrf);
            label { "Download " input type="text" name="download" value=(rate_value(d.options.max_download_limit)) placeholder="e.g. 500K, 2M, 0 = unlimited"; }
            label { "Upload " input type="text" name="upload" value=(rate_value(d.options.max_upload_limit)) placeholder="e.g. 100K"; }
            button type="submit" { "Set limits" }
        }
    }
}

pub fn remove_page(base: &str, csrf: &str, j: &Job) -> Markup {
    let id = j.id.0;
    html! {
        p.crumbs { a href=(format!("{base}/jobs/{id}")) { "\u{2190} Back" } }
        p { "Remove " strong { (j.name) } " from the list?" }
        form method="post" action=(format!("{base}/jobs/{id}/remove")) class="confirm" {
            input type="hidden" name="_csrf" value=(csrf);
            p { "Downloaded files stay on disk unless you explicitly delete them below." }
            fieldset.danger {
                legend { "Delete files" }
                label { input type="checkbox" name="delete_files" value="1"; " Also delete the downloaded files from disk" }
                label { "To confirm, type " code { "DELETE" } ": "
                    input type="text" name="confirm" autocomplete="off" placeholder="DELETE"; }
            }
            button.danger type="submit" { "Remove" }
            " " a.button href=(format!("{base}/jobs/{id}")) { "Cancel" }
        }
    }
}

// --------------------------------------------------------------------- add

pub fn add_page(base: &str, csrf: &str, video: bool) -> Markup {
    html! {
        form method="post" action=(format!("{base}/add")) {
            input type="hidden" name="_csrf" value=(csrf);
            h2 { "Links" }
            label { "Magnet links and URLs, one per line"
                textarea name="uris" rows="5" placeholder="magnet:?xt=urn:btih:...\nhttps://example.org/file.iso" {} }
            label { "Directory (optional) " input type="text" name="dir"; }
            label.inline { input type="checkbox" name="paused" value="1"; " Add paused" }
            @if video {
                h2 { "Video" }
                label { "Video page URL " input type="url" name="video_url" placeholder="https://..."; }
                label { "Quality " select name="quality" {
                    @for q in ["best", "2160p", "1440p", "1080p", "720p", "480p", "360p", "audio"] { option value=(q) { (q) } }
                } }
                label.inline { input type="checkbox" name="playlist" value="1"; " Whole playlist" }
            }
            button type="submit" { "Add" }
        }
        h2 { ".torrent / .metalink file" }
        form hx-post=(format!("{base}/add/upload")) hx-encoding="multipart/form-data" hx-target="#toast" hx-swap="innerHTML" {
            input type="file" name="file" accept=".torrent,.metalink,.meta4,application/x-bittorrent" multiple required;
            label.inline { input type="checkbox" name="paused" value="1"; " Add paused" }
            button type="submit" { "Upload and add" }
            p.hint { "Up to 10 MiB per upload." }
        }
    }
}

pub fn add_result_page(base: &str, added: &[u64], failed: &[String]) -> Markup {
    html! {
        @if !added.is_empty() { div.msg.info { "Added " (added.len()) " download(s)." } }
        @for f in failed { div.msg.error { (f) } }
        p { a href=(format!("{base}/")) { "Downloads" } " \u{b7} " a href=(format!("{base}/add")) { "Add more" } }
    }
}

// ------------------------------------------------------------------ search

pub struct SearchView<'a> {
    pub search_id: Option<u64>,
    pub finished: bool,
    pub providers: &'a [(String, ProviderStatus)],
    pub results: &'a [SearchResultDto],
}

pub fn search_results(base: &str, v: &SearchView<'_>) -> Markup {
    html! {
        div.chips {
            @for (name, st) in v.providers {
                span class=(format!("chip {}", match st { ProviderStatus::Running => "running", ProviderStatus::Done { .. } => "done", ProviderStatus::Failed { .. } => "failed" })) {
                    (name) ": "
                    @match st {
                        ProviderStatus::Running => { "searching" }
                        ProviderStatus::Done { count, elapsed_ms } => { (count) " results, " (elapsed_ms) " ms" }
                        ProviderStatus::Failed { error } => { "failed: " (error) }
                    }
                }
            }
            @if v.search_id.is_some() && !v.finished { span.chip.running { "\u{2026}" } }
        }
        @if v.results.is_empty() {
            @if v.finished { p.empty { "No results." } }
        } @else {
            table.list {
                thead { tr { th { "Title" } th { "Size" } th { "S" } th { "L" } th { "Sources" } th { "Date" } th { "" } } }
                tbody {
                    @for r in v.results { (result_row(base, r)) }
                }
            }
        }
    }
}

fn result_row(base: &str, r: &SearchResultDto) -> Markup {
    let vals = if let Some(m) = &r.magnet {
        Some(serde_json::json!({ "magnet": m }))
    } else if let Some(t) = &r.torrent_url {
        Some(serde_json::json!({ "torrent_url": t }))
    } else {
        r.detail_url
            .as_ref()
            .map(|d| serde_json::json!({ "detail_url": d, "provider": r.sources.first() }))
    };

    html! {
        tr {
            td.name data-label="Title" { (r.title) }
            td.num data-label="Size" { @if let Some(s) = r.size { (fmt::bytes(s)) } @else { "-" } }
            td.num data-label="Seeders" { (r.seeders.map_or("-".into(), |s| s.to_string())) }
            td.num data-label="Leechers" { (r.leechers.map_or("-".into(), |s| s.to_string())) }
            td data-label="Sources" { (r.sources.join(", ")) }
            td data-label="Date" { @if let Some(d) = r.date { (fmt::timestamp(d)) } }
            td.actions {
                @if let Some(v) = vals {
                    button type="button" hx-post=(format!("{base}/search/add")) hx-vals=(v.to_string())
                        hx-target="#toast" hx-swap="innerHTML" { "Add" }
                }
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
    html! {
        form.searchform method="post" action=(format!("{base}/search")) {
            input type="hidden" name="_csrf" value=(csrf);
            input type="search" name="q" value=(query) placeholder="Search torrents" required aria-label="Query";
            button type="submit" { "Search" }
            @if !providers.is_empty() {
                div.providers {
                    @for p in providers { label.inline {
                        input type="checkbox" name="providers" value=(p.name) checked[p.enabled];
                        " " (p.name)
                    } }
                }
            }
        }
        div #search-results sse-swap="search" hx-swap="innerHTML" { (search_results(base, view)) }
        @if !history.is_empty() {
            h2 { "History" }
            ul.history {
                @for h in history { li {
                    form method="post" action=(format!("{base}/search")) {
                        input type="hidden" name="_csrf" value=(csrf);
                        input type="hidden" name="q" value=(h.query);
                        @for p in &h.providers { input type="hidden" name="providers" value=(p); }
                        button type="submit" { "Run again" }
                        " " (h.query) " " span.hint { "(" (h.result_count) " results, " (fmt::timestamp(h.at)) ")" }
                    }
                } }
            }
            form method="post" action=(format!("{base}/search/history/clear")) {
                input type="hidden" name="_csrf" value=(csrf);
                button type="submit" { "Clear history" }
            }
        }
    }
}

// ---------------------------------------------------------------- settings

pub fn provider_row(base: &str, idx: usize, p: &ProviderInfo) -> Markup {
    html! {
        tr id=(format!("prov-{idx}")) {
            td data-label="Provider" { (p.name) " " span.hint { (p.kind) } @if !p.verified { " " span.warn { "unverified" } } }
            td data-label="Enabled" { (if p.enabled { "yes" } else { "no" }) }
            td.actions {
                button type="button" hx-post=(format!("{base}/settings/provider"))
                    hx-vals=(serde_json::json!({ "name": p.name, "enabled": (!p.enabled).to_string(), "idx": idx.to_string() }).to_string())
                    hx-target=(format!("#prov-{idx}")) hx-swap="outerHTML" {
                    (if p.enabled { "Disable" } else { "Enable" })
                }
                button type="button" hx-post=(format!("{base}/settings/provider/test"))
                    hx-vals=(serde_json::json!({ "name": p.name }).to_string())
                    hx-target="#toast" hx-swap="innerHTML" { "Test" }
            }
        }
    }
}

pub fn settings_page(
    base: &str,
    csrf: &str,
    limits: Option<&Limits>,
    providers: &[ProviderInfo],
    schedule: Option<&ScheduleStatus>,
) -> Markup {
    let rate = |n: u64| {
        if n == 0 {
            "unlimited".to_string()
        } else {
            fmt::bytes(n) + "/s"
        }
    };
    html! {
        h2 { "Limits" }
        @if let Some(l) = limits {
            p.hint { "Current: down " (rate(l.download)) ", up " (rate(l.upload)) ", max " (l.max_concurrent) " concurrent." }
        }
        form method="post" action=(format!("{base}/settings/limits")) {
            input type="hidden" name="_csrf" value=(csrf);
            label { "Download limit " input type="text" name="download" placeholder="e.g. 2M, 0 = unlimited (empty = keep)"; }
            label { "Upload limit " input type="text" name="upload" placeholder="e.g. 500K"; }
            label { "Max concurrent " input type="number" name="max_concurrent" min="1" max="100" value=[limits.map(|l| l.max_concurrent)]; }
            button type="submit" { "Apply" }
        }
        @if !providers.is_empty() {
            h2 { "Search providers" }
            table.list {
                thead { tr { th { "Provider" } th { "Enabled" } th { "" } } }
                tbody { @for (i, p) in providers.iter().enumerate() { (provider_row(base, i, p)) } }
            }
        }
        @if let Some(s) = schedule {
            @if !s.windows.is_empty() {
                h2 { "Schedule" }
                p.hint {
                    "Time zone " (s.time_zone) ". "
                    @if s.paused { "Downloads are currently held by a pause window. " }
                    @if let Some(a) = &s.active { "Active window: " (a) ". " }
                    @if let Some(n) = s.next_change { "Next change " (fmt::timestamp(n)) " UTC" @if let Some(w) = &s.next_window { " (" (w) ")" } "." }
                }
                table.list {
                    thead { tr { th { "Window" } th { "Days" } th { "Time" } th { "Action" } th { "Down" } th { "Up" } } }
                    tbody { @for w in &s.windows { tr {
                        td data-label="Window" { (w.name) }
                        td data-label="Days" { (w.days.join(", ")) }
                        td data-label="Time" { (w.start) "\u{2013}" (w.end) }
                        td data-label="Action" { (w.action) }
                        td.num data-label="Down" { (w.download_limit.map_or("-".to_string(), rate)) }
                        td.num data-label="Up" { (w.upload_limit.map_or("-".to_string(), rate)) }
                    } } }
                }
            }
        }
    }
}

// ------------------------------------------------------------------- login

pub fn login_page(base: &str, title: &str, error: Option<&str>) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                title { "Log in - " (title) }
                link rel="stylesheet" href=(format!("{base}/static/app.css"));
            }
            body {
                main.login {
                    h1 { (title) }
                    @if let Some(e) = error { div.msg.error { (e) } }
                    form method="post" action=(format!("{base}/login")) {
                        label { "Access token" input type="password" name="token" autocomplete="current-password" required autofocus; }
                        button type="submit" { "Log in" }
                    }
                }
            }
        }
    }
}

pub fn error_page(base: &str, msg: &str) -> Markup {
    html! {
        div.msg.error { (msg) }
        p { a href=(format!("{base}/")) { "\u{2190} Downloads" } }
    }
}
