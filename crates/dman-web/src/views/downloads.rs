//! Downloads page: paste box, segmented filter and search row, list rows with live
//! progress, floating bulk bar, and the remove confirmation (single and bulk).

use std::collections::BTreeMap;

use dman_proto::{Engine, Job, JobState};
use maud::{Markup, html};

use super::{ActionForm, action_form, bar, display_name, icon, is_torrent, metadata_pending};
use super::{state_label, state_name};
use crate::fmt;

/// Table state carried in the query string.
#[derive(Debug, Clone, Default)]
pub struct TableQuery {
    pub q: String,
    pub sort: String,
    pub desc: bool,
    pub all: bool,
    /// Also list removed downloads (hidden by default).
    pub removed: bool,
    /// Segment: `""` (all), `active`, `done` or `failed`.
    pub f: String,
    /// Just added: highlighted once (not part of the query string).
    pub fresh: Vec<u64>,
}

impl TableQuery {
    /// Reads `q`, `sort` (`key` or `key.dir`), `dir` and `all`.
    pub fn from_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        let mut t = TableQuery::default();
        for (k, v) in pairs {
            match k {
                "q" => t.q = v.trim().to_string(),
                "sort" => match v.split_once('.') {
                    Some((key, dir)) => {
                        t.sort = key.to_string();
                        t.desc = dir == "desc";
                    }
                    None => t.sort = v.to_string(),
                },
                "dir" => t.desc = v == "desc",
                "all" => t.all = v == "1",
                "removed" => t.removed = v == "1",
                "f" if matches!(v, "active" | "done" | "failed") => t.f = v.to_string(),
                _ => {}
            }
        }
        t
    }
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
            Section::Waiting => "Queued",
            Section::Paused => "Paused",
            Section::Stopped => "Finished and failed",
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

/// The job belongs to the segment `f` (`""` = all).
fn in_segment(j: &Job, f: &str) -> bool {
    match f {
        "active" => matches!(
            j.state,
            JobState::Active | JobState::Seeding | JobState::Waiting | JobState::Paused
        ),
        "done" => j.state == JobState::Complete,
        "failed" => j.state == JobState::Error,
        _ => true,
    }
}

fn matches_filter(j: &Job, q: &str) -> bool {
    let hay = format!("{} {} {}", j.name, state_label(j), j.source).to_lowercase();
    q.split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

fn sort_jobs(jobs: &mut [&Job], q: &TableQuery) {
    use std::cmp::Ordering;
    let key = q.sort.as_str();
    jobs.sort_by(|a, b| {
        let o = match key {
            "name" => display_name(a)
                .to_lowercase()
                .cmp(&display_name(b).to_lowercase()),
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
    if q.removed {
        s.append_pair("removed", "1");
    }
    if !q.f.is_empty() {
        s.append_pair("f", &q.f);
    }
    let s = s.finish();
    if s.is_empty() {
        String::new()
    } else {
        format!("?{s}")
    }
}

/// Secondary line under the name: size and connections, depending on the state.
fn figures(j: &Job) -> Vec<String> {
    let mut v = Vec::new();
    let size = if j.total_bytes == 0 {
        None
    } else if j.completed_bytes >= j.total_bytes {
        Some(fmt::bytes(j.total_bytes))
    } else {
        Some(format!(
            "{} of {}",
            fmt::bytes(j.completed_bytes),
            fmt::bytes(j.total_bytes)
        ))
    };
    match j.state {
        JobState::Active => {
            v.extend(size);
            v.push(peers_text(j));
        }
        JobState::Seeding => {
            v.extend(size);
            v.push(format!("ratio {:.2}", j.ratio()));
            v.push(peers_text(j));
        }
        JobState::Waiting => {
            if let Some(p) = j.queue_position {
                v.push(format!("#{} in queue", p + 1));
            }
            v.extend(size);
        }
        JobState::Complete => {
            v.extend(size);
            if is_torrent(j) {
                v.push(format!("ratio {:.2}", j.ratio()));
            }
            if let Some(c) = j.completed_at {
                v.push(format!("finished {} UTC", fmt::timestamp(c)));
            }
        }
        _ => v.extend(size),
    }
    v
}

fn peers_text(j: &Job) -> String {
    match (is_torrent(j), j.seeders) {
        (true, Some(s)) => format!("{} peers ({s} seeds)", j.connections),
        (true, None) => format!("{} peers", j.connections),
        (false, _) if j.connections == 1 => "1 connection".into(),
        (false, _) => format!("{} connections", j.connections),
    }
}

/// Icon name for the kind of file a download is (`k-<name>` colours the tile).
pub fn kind_icon(j: &Job) -> &'static str {
    if j.engine == Engine::YtDlp {
        return "video";
    }
    let name = display_name(j).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "ts" | "flv" | "wmv" => "video",
        "mp3" | "flac" | "m4a" | "ogg" | "opus" | "wav" | "aac" => "music",
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2" | "zst" | "tgz" | "deb" | "rpm" => {
            "archive"
        }
        "iso" | "img" | "dmg" => "disc",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" => "image",
        "pdf" | "epub" | "mobi" | "txt" | "doc" | "docx" | "odt" => "doc",
        _ if metadata_pending(j) => "magnet",
        _ if is_torrent(j) => "folder",
        _ => "file",
    }
}

/// The live part of a row: state pill, figures, progress, speed, time left, error.
/// Pushed over SSE (`job-<id>`, outer swap); `display: contents` lets its parts sit
/// in the row's grid columns. Fixed columns and tabular numbers: no layout shift.
pub fn job_live(j: &Job) -> Markup {
    let id = j.id.0;
    let st = state_name(j.state);
    let frac = if metadata_pending(j) {
        None
    } else {
        Some(j.progress())
    };
    let moving = matches!(j.state, JobState::Active | JobState::Seeding);
    html! {
        div.live id=(format!("live-{id}")) sse-swap=(format!("job-{id}")) hx-swap="outerHTML" {
            span class=(format!("pill {st}")) { span.dot {} (state_label(j)) }
            span.jsub { (figures(j).join(" \u{b7} ")) }
            span.jprog {
                (bar(Some(format!("pb-{id}")), &format!("bar {st}"), frac, "Progress"))
                @if let Some(f) = frac { span.pct { (format!("{:.1}%", f * 100.0)) } }
                @else { span.pct.muted { "file list\u{2026}" } }
            }
            span.jspd {
                @if moving {
                    @if j.state == JobState::Active { span title="Download speed" { "\u{2193} " (fmt::speed0(j.download_speed)) } }
                    @if j.upload_speed > 0 || j.state == JobState::Seeding { span.up title="Upload speed" { "\u{2191} " (fmt::speed0(j.upload_speed)) } }
                }
            }
            span.jeta title="Time left" {
                @if j.state == JobState::Active { (j.eta_secs().map_or_else(|| "\u{2014}".to_string(), |e| fmt::eta(Some(e)))) }
            }
            @if let Some(e) = &j.error { p.jerr { (e) } }
        }
    }
}

/// One download row.
fn job_card(base: &str, csrf: &str, j: &Job, fresh: bool) -> Markup {
    let id = j.id.0;
    let name = display_name(j);
    let url = |a: &str| format!("{base}/jobs/{id}/{a}");
    let act = |a: &str, label: &'static str, class: &'static str, ico: Option<&str>| {
        action_form(&ActionForm {
            url: url(a),
            label,
            class,
            target: "#jobs",
            csrf: Some(csrf),
            vals: &[],
            include: Some("#filter"),
            title: ico.map(|_| label.to_string()),
            icon: ico.map(|i| icon(base, i)),
        })
    };
    let reorder = |how: &'static str, label: &'static str, ico: &str| {
        action_form(&ActionForm {
            url: url("reorder"),
            label,
            class: "mi",
            target: "#jobs",
            csrf: Some(csrf),
            vals: &[("how", how)],
            include: Some("#filter"),
            title: None,
            icon: Some(icon(base, ico)),
        })
    };
    let primary = match j.state {
        JobState::Active | JobState::Seeding | JobState::Waiting => {
            Some(act("pause", "Pause", "ib", Some("pause")))
        }
        JobState::Paused => Some(act("resume", "Resume", "ib acc", Some("play"))),
        JobState::Error => Some(act("resume", "Retry", "ib acc", Some("retry"))),
        _ => None,
    };
    let kind = kind_icon(j);
    html! {
        li id=(format!("job-{id}")) class=(format!("job st-{}{}", state_name(j.state), if fresh { " fresh" } else { "" })) {
            input.sel type="checkbox" name="ids" value=(id) form="bulk" aria-label=(format!("Select {name}"));
            span class=(format!("ficon k-{kind}")) { (icon(base, kind)) }
            a.jname href=(format!("{base}/jobs/{id}")) title=(name) { (name) }
            (job_live(j))
            div.jact {
                @if let Some(p) = primary { (p) }
                details.menu {
                    summary.ib aria-label=(format!("More actions for {name}")) title="More actions" { (icon(base, "more")) }
                    div.pop {
                        a.mi href=(format!("{base}/jobs/{id}")) { (icon(base, "info")) span.bl { "Details" } }
                        @if j.state == JobState::Waiting {
                            (reorder("top", "Move to top", "top"))
                            (reorder("up", "Move up", "up"))
                            (reorder("down", "Move down", "down"))
                            (reorder("bottom", "Move to bottom", "bottom"))
                        }
                        @if j.state.is_stopped() {
                            (act("purge", "Clear from list", "mi", Some("check")))
                        }
                        @if j.state != JobState::Removed {
                            hr;
                            a.mi href=(url("remove")) hx-get=(url("remove")) hx-target="#modal-body" hx-swap="innerHTML" { (icon(base, "x")) span.bl { "Remove from list\u{2026}" } }
                            a.mi.danger href=(url("remove?delete=1")) hx-get=(url("remove?delete=1")) hx-target="#modal-body" hx-swap="innerHTML" { (icon(base, "trash")) span.bl { "Delete files\u{2026}" } }
                        }
                    }
                }
            }
        }
    }
}

/// Empty-state illustration (inline SVG; colours come from CSS classes).
pub fn illustration() -> Markup {
    html! {
        svg.illus viewBox="0 0 160 112" aria-hidden="true" focusable="false" {
            circle.il-glow cx="80" cy="44" r="34" {}
            rect.il-box x="28" y="58" width="104" height="42" rx="12" {}
            path.il-line d="M28 70h30l8 11h28l8-11h30" {}
            path.il-arrow d="M80 18v34M68 41l12 12 12-12" {}
            circle.il-dot cx="124" cy="22" r="3" {}
            circle.il-dot cx="36" cy="32" r="2" {}
            circle.il-dot cx="140" cy="48" r="2" {}
        }
    }
}

/// The `#jobs` container: refreshes itself when the server announces a changed layout.
pub fn jobs_table(base: &str, csrf: &str, jobs: &[Job], q: &TableQuery) -> Markup {
    const STOPPED_PAGE: usize = 50;
    let any = jobs.iter().any(|j| section_of(j).is_some());
    let mut shown: Vec<&Job> = jobs
        .iter()
        .filter(|j| section_of(j).is_some() && in_segment(j, &q.f) && matches_filter(j, &q.q))
        .collect();
    sort_jobs(&mut shown, q);
    let mut sections: BTreeMap<Section, Vec<&Job>> = BTreeMap::new();
    for j in shown {
        if let Some(s) = section_of(j) {
            sections.entry(s).or_default().push(j);
        }
    }
    let refresh = format!("{base}/downloads/table{}", table_qs(q));
    let removed: Vec<&Job> = jobs
        .iter()
        .filter(|j| j.state == JobState::Removed && matches_filter(j, &q.q))
        .collect();
    let toggle = table_qs(&TableQuery {
        removed: !q.removed,
        fresh: Vec::new(),
        ..q.clone()
    });
    let segment = match q.f.as_str() {
        "active" => "active",
        "done" => "finished",
        "failed" => "failed",
        _ => "",
    };
    html! {
        div #jobs.jobs hx-get=(refresh) hx-trigger="sse:jobs-changed" hx-swap="outerHTML" {
            @if !any {
                div.emptyst {
                    (illustration())
                    h2 { "No downloads yet" }
                    p { "Paste a magnet or link above, drop a .torrent file anywhere on this page, or find something with " a href=(format!("{base}/search")) { "Search" } "." }
                }
            } @else if sections.is_empty() {
                div.emptyst.small {
                    @if q.q.is_empty() {
                        p { "No " (segment) " downloads." }
                    } @else {
                        p { "No downloads match \u{201c}" (q.q) "\u{201d}." }
                    }
                    p { a.btn.small href=(format!("{base}/")) { "Show all" } }
                }
            } @else {
                @for (sec, list) in &sections {
                    section.sec {
                        h2.sech { (sec.label()) span.count { (list.len()) } }
                        @let limit = if *sec == Section::Stopped && !q.all { STOPPED_PAGE } else { usize::MAX };
                        ul.rows {
                            @for j in list.iter().take(limit) { (job_card(base, csrf, j, q.fresh.contains(&j.id.0))) }
                        }
                        @if list.len() > limit {
                            @let all = table_qs(&TableQuery { all: true, ..q.clone() });
                            p.showall { a.btn.small href=(format!("{base}/{all}")) hx-get=(format!("{base}/downloads/table{all}"))
                                hx-target="#jobs" hx-swap="outerHTML" { "Show all " (list.len()) } }
                        }
                    }
                }
            }
            @if q.removed && !removed.is_empty() {
                section.sec.removed {
                    h2.sech { "Removed" span.count { (removed.len()) } }
                    ul.rows {
                        @for j in removed.iter().take(200) { (job_card(base, csrf, j, false)) }
                    }
                }
            }
            @if !removed.is_empty() || q.removed {
                div.removedbar {
                    a.linkbtn href=(format!("{base}/{toggle}")) hx-get=(format!("{base}/downloads/table{toggle}"))
                        hx-target="#jobs" hx-swap="outerHTML" {
                        @if q.removed { "Hide removed" } @else { "Show removed (" (removed.len()) ")" }
                    }
                    @if !removed.is_empty() {
                        (action_form(&ActionForm {
                            url: format!("{base}/jobs/clear-removed"),
                            label: "Clear removed",
                            class: "linkbtn",
                            target: "#jobs",
                            csrf: Some(csrf),
                            vals: &[],
                            include: Some("#filter"),
                            title: Some("Forget removed downloads (files are not touched)".into()),
                            icon: None,
                        }))
                    }
                }
            }
        }
    }
}

const SORTS: [(&str, &str); 11] = [
    ("", "Queue order"),
    ("date.desc", "Newest first"),
    ("date.asc", "Oldest first"),
    ("name.asc", "Name A\u{2013}Z"),
    ("name.desc", "Name Z\u{2013}A"),
    ("size.desc", "Largest first"),
    ("size.asc", "Smallest first"),
    ("progress.desc", "Most complete"),
    ("speed.desc", "Fastest"),
    ("eta.asc", "Finishing soonest"),
    ("ratio.desc", "Highest ratio"),
];

/// The paste box: one field, one button; a file can be chosen too.
pub fn paste_box(base: &str, csrf: &str) -> Markup {
    let add = format!("{base}/add");
    html! {
        form #paste.paste method="post" action=(add) enctype="multipart/form-data"
            hx-post=(add) hx-encoding="multipart/form-data" hx-target="#jobs" hx-swap="outerHTML"
            hx-include="#filter" hx-disabled-elt="find button" hx-indicator="#paste-ind" {
            input type="hidden" name="_csrf" value=(csrf);
            input type="hidden" name="from" value="downloads";
            label.sr for="paste-in" { "Add a download" }
            div.pastebox {
                span.pico { (icon(base, "link")) }
                textarea #paste-in name="uris" rows="1" data-kinds="paste-kind"
                    placeholder="Paste a magnet or link\u{2026}"
                    autocomplete="off" spellcheck="false" aria-describedby="paste-help" {}
                span #paste-kind.kinds aria-live="polite" {}
                button.btn.primary type="submit" { "Add" }
            }
            (super::quality::checking("paste-ind", true))
            p.pastehelp #paste-help {
                label.filepick {
                    input.sr type="file" name="file" accept=".torrent,.metalink,.meta4,application/x-bittorrent" multiple;
                    (icon(base, "upload")) span { "Upload .torrent" }
                }
                a href=(add) { (icon(base, "sliders")) span { "More options" } }
                a href=(format!("{base}/search")) { (icon(base, "search")) span { "Search torrents" } }
                span.drophint.jsonly { "or drop .torrent files anywhere" }
            }
        }
    }
}

const SEGMENTS: [(&str, &str); 4] = [
    ("", "All"),
    ("active", "Active"),
    ("done", "Done"),
    ("failed", "Failed"),
];

pub fn downloads_page(base: &str, csrf: &str, jobs: &[Job], q: &TableQuery) -> Markup {
    let cur = if q.sort.is_empty() {
        String::new()
    } else {
        format!("{}.{}", q.sort, if q.desc { "desc" } else { "asc" })
    };
    let ib = |url: &str, label: &str, ico: &str| {
        html! {
            button.btn.small.ghost type="button" hx-post=(url) hx-target="#toasts" hx-swap="beforeend" {
                (icon(base, ico)) span { (label) }
            }
        }
    };
    html! {
        div.pagehead {
            h1 { "Downloads" }
            div.headact {
                (ib(&format!("{base}/queue/pause-all"), "Pause all", "pause"))
                (ib(&format!("{base}/queue/resume-all"), "Resume all", "play"))
            }
        }
        (paste_box(base, csrf))
        div.toolbar {
            form #filter.filter role="search" method="get" action=(format!("{base}/"))
                hx-get=(format!("{base}/downloads/table")) hx-target="#jobs" hx-swap="outerHTML"
                hx-trigger="input changed delay:250ms, change, submit" {
                div.seg role="radiogroup" aria-label="Show" {
                    @for (v, l) in SEGMENTS {
                        label { input type="radio" name="f" value=(v) checked[q.f == v]; span { (l) } }
                    }
                }
                div.fsearch {
                    (icon(base, "search"))
                    input type="search" name="q" value=(q.q) placeholder="Filter by name" aria-label="Filter downloads by name";
                }
                select name="sort" aria-label="Sort downloads" {
                    @for (v, l) in SORTS { option value=(v) selected[cur == v] { (l) } }
                }
                @if q.removed { input type="hidden" name="removed" value="1"; }
                button.btn.small.nojs type="submit" { "Apply" }
            }
            label.selall.jsonly { input #selall type="checkbox"; span { "Select all" } }
        }
        (jobs_table(base, csrf, jobs, q))
        form #bulk.bulkbar method="post" action=(format!("{base}/jobs/bulk"))
            hx-post=(format!("{base}/jobs/bulk")) hx-target="#jobs" hx-swap="outerHTML" hx-include="#filter" {
            input type="hidden" name="_csrf" value=(csrf);
            span #selcount.selcount { "Selected downloads:" }
            button.btn.small type="submit" name="action" value="pause" { (icon(base, "pause")) span { "Pause" } }
            button.btn.small type="submit" name="action" value="resume" { (icon(base, "play")) span { "Resume" } }
            button.btn.small type="submit" formmethod="get" formaction=(format!("{base}/jobs/bulk/remove"))
                hx-get=(format!("{base}/jobs/bulk/remove")) hx-include="#bulk" hx-target="#modal-body" hx-swap="innerHTML" { (icon(base, "x")) span { "Remove\u{2026}" } }
            button.btn.small.danger type="submit" formmethod="get" formaction=(format!("{base}/jobs/bulk/remove")) name="delete" value="1"
                hx-get=(format!("{base}/jobs/bulk/remove?delete=1")) hx-include="#bulk" hx-target="#modal-body" hx-swap="innerHTML" { (icon(base, "trash")) span { "Delete files\u{2026}" } }
            button.ib.jsonly type="button" data-clear-sel aria-label="Clear selection" title="Clear selection" { (icon(base, "x")) }
        }
    }
}

/// What a remove confirmation is about.
pub struct RemoveTarget<'a> {
    /// Posted to.
    pub action: String,
    pub jobs: Vec<&'a Job>,
    /// Where "Cancel" leads without JavaScript, and `back=detail` for the detail page.
    pub back: &'a str,
    pub cancel_href: String,
}

/// The one confirmation for removing: either "remove from list" (files stay) or,
/// when `delete` is set, "remove and permanently delete the files". The delete form
/// carries an explicit `delete_files=true`; nothing else ever deletes.
pub fn remove_form(base: &str, csrf: &str, t: &RemoveTarget<'_>, delete: bool) -> Markup {
    let n = t.jobs.len();
    let what = if n == 1 {
        format!("\u{201c}{}\u{201d}", display_name(t.jobs[0]))
    } else {
        format!("{n} downloads")
    };
    let ids: String = t.jobs.iter().map(|j| format!("ids={}&", j.id.0)).collect();
    let other = if n == 1 {
        format!(
            "{base}/jobs/{}/remove?back={}{}",
            t.jobs[0].id.0,
            t.back,
            if delete { "" } else { "&delete=1" }
        )
    } else {
        format!(
            "{base}/jobs/bulk/remove?{ids}{}",
            if delete { "" } else { "delete=1" }
        )
    };
    html! {
        form.confirm method="post" action=(t.action) hx-post=(t.action) hx-target="this" hx-swap="outerHTML" {
            input type="hidden" name="_csrf" value=(csrf);
            input type="hidden" name="back" value=(t.back);
            @for j in &t.jobs { input type="hidden" name="ids" value=(j.id.0); }
            @if delete {
                input type="hidden" name="delete_files" value="true";
                span.cicon.bad { (icon(base, "trash")) }
                h2 #modal-title { "Remove and permanently delete the files of " (what) "?" }
                p { "The download leaves the list " strong { "and its files are deleted from disk" } ". This cannot be undone." }
            } @else {
                span.cicon { (icon(base, "x")) }
                h2 #modal-title { "Remove " (what) " from the list?" }
                p { "Files already downloaded " strong { "stay on disk" } ". You can undo this for a few seconds." }
            }
            @if n > 1 {
                ul.names {
                    @for j in t.jobs.iter().take(8) { li { (display_name(j)) } }
                    @if n > 8 { li.muted { "and " (n - 8) " more" } }
                }
            }
            div.btnrow.end {
                a.btn.ghost href=(t.cancel_href) data-close { "Cancel" }
                @if delete { button.btn.danger type="submit" { "Delete files" } }
                @else { button.btn.primary type="submit" { "Remove from list" } }
            }
            p.help.alt {
                a href=(other) hx-get=(other) hx-target="#modal-body" hx-swap="innerHTML" {
                    @if delete { "Keep the files and only remove from the list instead" }
                    @else { "Delete the files too\u{2026}" }
                }
            }
        }
    }
}
