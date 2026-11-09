//! Downloads page: paste box, compact filter row, cards with live progress, bulk bar,
//! and the remove confirmation (single and bulk).

use std::collections::BTreeMap;

use dman_proto::{Job, JobState};
use maud::{Markup, html};

use super::{ActionForm, action_form, bar, display_name, is_torrent, metadata_pending};
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
    let s = s.finish();
    if s.is_empty() {
        String::new()
    } else {
        format!("?{s}")
    }
}

/// Figures under the progress bar, depending on the state.
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
            v.push(format!("\u{2193} {}", fmt::speed0(j.download_speed)));
            if j.upload_speed > 0 {
                v.push(format!("\u{2191} {}", fmt::speed0(j.upload_speed)));
            }
            if let Some(e) = j.eta_secs() {
                v.push(format!("{} left", fmt::eta(Some(e))));
            }
            v.push(peers_text(j));
        }
        JobState::Seeding => {
            v.extend(size);
            v.push(format!("\u{2191} {}", fmt::speed0(j.upload_speed)));
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

/// The live part of a card: badge, progress bar, figures, error. The same markup is
/// pushed over SSE (`job-<id>`, outer swap); ids let htmx animate the bar.
pub fn job_live(j: &Job) -> Markup {
    let id = j.id.0;
    let st = state_name(j.state);
    let frac = if metadata_pending(j) {
        None
    } else {
        Some(j.progress())
    };
    html! {
        div.live id=(format!("live-{id}")) sse-swap=(format!("job-{id}")) hx-swap="outerHTML" {
            div.lrow {
                span class=(format!("badge {st}")) { (state_label(j)) }
                @if let Some(f) = frac { span.pct { (format!("{:.1}%", f * 100.0)) } }
                @else { span.pct.muted { "getting file list\u{2026}" } }
            }
            (bar(Some(format!("pb-{id}")), &format!("bar {st}"), frac, "Progress"))
            div.meta { @for f in figures(j) { span { (f) } } }
            @if let Some(e) = &j.error { p.jerr { (e) } }
        }
    }
}

/// One download card.
fn job_card(base: &str, csrf: &str, j: &Job, fresh: bool) -> Markup {
    let id = j.id.0;
    let name = display_name(j);
    let url = |a: &str| format!("{base}/jobs/{id}/{a}");
    let act = |a: &str, label: &'static str, class: &'static str| {
        action_form(&ActionForm {
            url: url(a),
            label,
            class,
            target: "#jobs",
            csrf: Some(csrf),
            vals: &[],
            include: Some("#filter"),
            title: None,
        })
    };
    let reorder = |how: &'static str, label: &'static str| {
        action_form(&ActionForm {
            url: url("reorder"),
            label,
            class: "mi",
            target: "#jobs",
            csrf: Some(csrf),
            vals: &[("how", how)],
            include: Some("#filter"),
            title: None,
        })
    };
    let primary = match j.state {
        JobState::Active | JobState::Seeding | JobState::Waiting => {
            Some(act("pause", "Pause", "btn small"))
        }
        JobState::Paused => Some(act("resume", "Resume", "btn small primary")),
        JobState::Error => Some(act("resume", "Retry", "btn small primary")),
        _ => None,
    };
    html! {
        li id=(format!("job-{id}")) class=(format!("job st-{}{}", state_name(j.state), if fresh { " fresh" } else { "" })) {
            input.sel type="checkbox" name="ids" value=(id) form="bulk" aria-label=(format!("Select {name}"));
            div.jmain {
                a.jname href=(format!("{base}/jobs/{id}")) { (name) }
                (job_live(j))
            }
            div.jact {
                @if let Some(p) = primary { (p) }
                details.menu {
                    summary.btn.small.ghost aria-label=(format!("More actions for {name}")) title="More actions" { "\u{22ef}" }
                    div.pop {
                        a.mi href=(format!("{base}/jobs/{id}")) { "Details" }
                        @if j.state == JobState::Waiting {
                            (reorder("top", "Move to top"))
                            (reorder("up", "Move up"))
                            (reorder("down", "Move down"))
                            (reorder("bottom", "Move to bottom"))
                        }
                        @if j.state.is_stopped() {
                            (act("purge", "Clear from list", "mi"))
                        }
                        @if j.state != JobState::Removed {
                            a.mi href=(url("remove")) hx-get=(url("remove")) hx-target="#modal-body" hx-swap="innerHTML" { "Remove from list\u{2026}" }
                            a.mi.danger href=(url("remove?delete=1")) hx-get=(url("remove?delete=1")) hx-target="#modal-body" hx-swap="innerHTML" { "Delete files\u{2026}" }
                        }
                    }
                }
            }
        }
    }
}

/// The `#jobs` container: refreshes itself when the server announces a changed layout.
pub fn jobs_table(base: &str, csrf: &str, jobs: &[Job], q: &TableQuery) -> Markup {
    const STOPPED_PAGE: usize = 50;
    let any = jobs.iter().any(|j| section_of(j).is_some());
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
    let removed: Vec<&Job> = jobs
        .iter()
        .filter(|j| j.state == JobState::Removed && matches_filter(j, &q.q))
        .collect();
    let toggle = table_qs(&TableQuery {
        removed: !q.removed,
        fresh: Vec::new(),
        ..q.clone()
    });
    html! {
        div #jobs.jobs hx-get=(refresh) hx-trigger="sse:jobs-changed" hx-swap="outerHTML" {
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
                        }))
                    }
                }
            }
            @if !any {
                div.card.empty {
                    h2 { "No downloads yet" }
                    p { "Paste a magnet or link above, drop a .torrent file here, or " a href=(format!("{base}/search")) { "search for torrents" } "." }
                }
            } @else if sections.is_empty() {
                div.card.empty {
                    p { "No downloads match \u{201c}" (q.q) "\u{201d}." }
                    p { a.btn.small href=(format!("{base}/")) { "Clear filter" } }
                }
            } @else {
                @for (sec, list) in &sections {
                    section.sec {
                        h2 { (sec.label()) " " span.count { (list.len()) } }
                        @let limit = if *sec == Section::Stopped && !q.all { STOPPED_PAGE } else { usize::MAX };
                        ul.cards {
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
                section.sec {
                    h2 { "Removed " span.count { (removed.len()) } }
                    ul.cards {
                        @for j in removed.iter().take(200) { (job_card(base, csrf, j, false)) }
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
        form #paste.paste.card method="post" action=(add) enctype="multipart/form-data"
            hx-post=(add) hx-encoding="multipart/form-data" hx-target="#jobs" hx-swap="outerHTML"
            hx-include="#filter" hx-disabled-elt="find button" {
            input type="hidden" name="_csrf" value=(csrf);
            input type="hidden" name="from" value="downloads";
            label.plabel for="paste-in" { "Add a download" }
            div.prow {
                textarea #paste-in name="uris" rows="1" required
                    placeholder="Paste a magnet, link or drop a .torrent file"
                    autocomplete="off" spellcheck="false" aria-describedby="paste-help" {}
                button.btn.primary.big type="submit" { "Add" }
            }
            p.help #paste-help {
                label.filepick {
                    input.sr type="file" name="file" accept=".torrent,.metalink,.meta4,application/x-bittorrent" multiple;
                    span { "Choose a .torrent file" }
                }
                " \u{b7} " a href=(add) { "More options" }
                " \u{b7} " a href=(format!("{base}/search")) { "Search torrents" }
            }
        }
    }
}

pub fn downloads_page(base: &str, csrf: &str, jobs: &[Job], q: &TableQuery) -> Markup {
    let cur = if q.sort.is_empty() {
        String::new()
    } else {
        format!("{}.{}", q.sort, if q.desc { "desc" } else { "asc" })
    };
    let filtering = !q.q.is_empty() || !q.sort.is_empty();
    html! {
        h1.sr { "Downloads" }
        (paste_box(base, csrf))
        div.toolbar {
            input #ft.ftoggle type="checkbox" checked[filtering] aria-controls="filter";
            label.btn.small.ftbtn for="ft" { "Filter" }
            form #filter.filter role="search" method="get" action=(format!("{base}/"))
                hx-get=(format!("{base}/downloads/table")) hx-target="#jobs" hx-swap="outerHTML"
                hx-trigger="input changed delay:250ms, change, submit" {
                input type="search" name="q" value=(q.q) placeholder="Filter by name" aria-label="Filter downloads by name";
                select name="sort" aria-label="Sort downloads" {
                    @for (v, l) in SORTS { option value=(v) selected[cur == v] { (l) } }
                }
                @if q.removed { input type="hidden" name="removed" value="1"; }
                button.btn.small.nojs type="submit" { "Apply" }
            }
            div.qbtns {
                label.check.jsonly { input #selall type="checkbox"; " Select all" }
                button.btn.small type="button" hx-post=(format!("{base}/queue/pause-all")) hx-target="#toasts" hx-swap="beforeend" { "Pause all" }
                button.btn.small type="button" hx-post=(format!("{base}/queue/resume-all")) hx-target="#toasts" hx-swap="beforeend" { "Resume all" }
            }
        }
        (jobs_table(base, csrf, jobs, q))
        form #bulk.bulkbar method="post" action=(format!("{base}/jobs/bulk"))
            hx-post=(format!("{base}/jobs/bulk")) hx-target="#jobs" hx-swap="outerHTML" hx-include="#filter" {
            input type="hidden" name="_csrf" value=(csrf);
            span #selcount.selcount { "Selected downloads:" }
            button.btn.small type="submit" name="action" value="pause" { "Pause" }
            button.btn.small type="submit" name="action" value="resume" { "Resume" }
            button.btn.small type="submit" formmethod="get" formaction=(format!("{base}/jobs/bulk/remove"))
                hx-get=(format!("{base}/jobs/bulk/remove")) hx-include="#bulk" hx-target="#modal-body" hx-swap="innerHTML" { "Remove\u{2026}" }
            button.btn.small.danger type="submit" formmethod="get" formaction=(format!("{base}/jobs/bulk/remove")) name="delete" value="1"
                hx-get=(format!("{base}/jobs/bulk/remove?delete=1")) hx-include="#bulk" hx-target="#modal-body" hx-swap="innerHTML" { "Delete files\u{2026}" }
            button.btn.small.ghost.jsonly type="button" data-clear-sel { "Clear selection" }
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
                h2 #modal-title { "Remove and permanently delete the files of " (what) "?" }
                p { "The download leaves the list " strong { "and its files are deleted from disk" } ". This cannot be undone." }
            } @else {
                h2 #modal-title { "Remove " (what) " from the list?" }
                p { "Files already downloaded " strong { "stay on disk" } "." }
            }
            @if n > 1 {
                ul.names {
                    @for j in t.jobs.iter().take(8) { li { (display_name(j)) } }
                    @if n > 8 { li.muted { "and " (n - 8) " more" } }
                }
            }
            div.btnrow {
                @if delete { button.btn.danger type="submit" { "Delete files" } }
                @else { button.btn.primary type="submit" { "Remove from list" } }
                a.btn href=(t.cancel_href) data-close { "Cancel" }
            }
            p.help {
                a href=(other) hx-get=(other) hx-target="#modal-body" hx-swap="innerHTML" {
                    @if delete { "Keep the files and only remove from the list instead" }
                    @else { "Delete the files too\u{2026}" }
                }
            }
        }
    }
}
