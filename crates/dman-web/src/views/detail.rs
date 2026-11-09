//! Detail page: live header (name, status, progress, stat tiles), then tabs for
//! files, peers, trackers, options (with rename) and info.

use dman_proto::{Engine, Job, JobDetail, JobOptions, JobState};
use maud::{Markup, html};

use super::{ActionForm, Field, action_form, bar, display_name, field, icon, is_torrent};
use super::{metadata_pending, state_label, state_name};
use crate::fmt;

/// One plain-language sentence about where a download stands.
pub fn status_sentence(j: &Job) -> String {
    let mut p: Vec<String> = Vec::new();
    match j.state {
        JobState::Active if metadata_pending(j) => {
            return "Fetching torrent metadata\u{2026} (needs at least one peer)".into();
        }
        JobState::Active => {
            p.push(if j.engine == Engine::YtDlp {
                "Downloading video".into()
            } else {
                "Downloading".into()
            });
            p.push(if is_torrent(j) {
                format!("{} peers", j.connections)
            } else if j.connections == 1 {
                "1 connection".into()
            } else {
                format!("{} connections", j.connections)
            });
            match j.eta_secs() {
                Some(e) => p.push(format!("ETA {}", fmt::eta(Some(e)))),
                None if j.download_speed == 0 => p.push("waiting for data".into()),
                None => {}
            }
        }
        JobState::Seeding => {
            p.push("Seeding".into());
            p.push(format!("\u{2191} {}", fmt::speed0(j.upload_speed)));
            p.push(format!("ratio {:.2}", j.ratio()));
        }
        JobState::Waiting => {
            p.push("Queued".into());
            if let Some(q) = j.queue_position {
                p.push(format!("#{} in line", q + 1));
            }
            p.push("starts when a download slot is free".into());
        }
        JobState::Paused => return "Paused \u{2014} press Resume to continue".into(),
        JobState::Complete => {
            return match j.completed_at {
                Some(t) => format!("Completed {} UTC", fmt::timestamp(t)),
                None => "Completed".into(),
            };
        }
        JobState::Error => return "Failed \u{2014} press Retry to try again".into(),
        JobState::Removed => return "Removed from the list".into(),
    }
    p.join(" \u{b7} ")
}

fn tile(label: &str, value: String, sub: Option<String>) -> Markup {
    html! {
        div.tile {
            span.tl { (label) }
            span.tv { (value) }
            @if let Some(s) = sub { span.ts { (s) } }
        }
    }
}

/// Live header (`#dhead`): name, pill, status sentence, big bar, stat tiles, actions.
/// Pushed over SSE as `detail` (outer swap); `csrf` is `None` there.
pub fn detail_head(base: &str, csrf: Option<&str>, j: &Job) -> Markup {
    let id = j.id.0;
    let st = state_name(j.state);
    let frac = if metadata_pending(j) {
        None
    } else {
        Some(j.progress())
    };
    let act = |a: &str, label: &'static str, class: &'static str, ico: &str| {
        action_form(&ActionForm {
            url: format!("{base}/jobs/{id}/{a}"),
            label,
            class,
            target: "#dhead",
            csrf,
            vals: &[("back", "detail")],
            include: None,
            title: None,
            icon: Some(icon(base, ico)),
        })
    };
    let remove = format!("{base}/jobs/{id}/remove?back=detail");
    let kind = super::kind_icon(j);
    let torrent = is_torrent(j);
    let peers = if torrent {
        tile(
            "Peers",
            j.connections.to_string(),
            j.seeders.map(|s| format!("{s} seeds")),
        )
    } else {
        tile("Connections", j.connections.to_string(), None)
    };
    html! {
        header #dhead.dhead sse-swap="detail" hx-swap="outerHTML" {
            div.dtop {
                span class=(format!("ficon lg k-{kind}")) { (icon(base, kind)) }
                div.dtitle {
                    h1 { (display_name(j)) }
                    p.dstatus { span class=(format!("pill {st}")) { span.dot {} (state_label(j)) } span.status { (status_sentence(j)) } }
                }
                div.dact {
                    @match j.state {
                        JobState::Paused => { (act("resume", "Resume", "btn primary", "play")) }
                        JobState::Error => { (act("resume", "Retry", "btn primary", "retry")) }
                        JobState::Complete | JobState::Removed => {}
                        _ => { (act("pause", "Pause", "btn", "pause")) }
                    }
                    a.btn href=(remove) hx-get=(remove) hx-target="#modal-body" hx-swap="innerHTML" { (icon(base, "x")) span { "Remove from list\u{2026}" } }
                    a.btn.danger-outline href=(format!("{remove}&delete=1")) hx-get=(format!("{remove}&delete=1")) hx-target="#modal-body" hx-swap="innerHTML" { (icon(base, "trash")) span { "Delete files\u{2026}" } }
                }
            }
            @if let Some(e) = &j.error { p.jerr role="alert" { (e) } }
            div.dprog {
                (bar(Some("dpb".into()), &format!("bar big {st}"), frac, "Progress"))
                @if let Some(f) = frac { span.pct { (format!("{:.1}%", f * 100.0)) } }
            }
            div.tiles {
                (tile("Download", fmt::speed0(j.download_speed), Some(format!("\u{2191} {}", fmt::speed0(j.upload_speed)))))
                (tile("Time left", if j.state == JobState::Active { fmt::eta(j.eta_secs()) } else { "\u{2014}".into() }, None))
                (tile("Size", if j.total_bytes > 0 { fmt::bytes(j.total_bytes) } else { "\u{2014}".into() },
                    (j.total_bytes > 0 && j.completed_bytes < j.total_bytes).then(|| format!("{} done", fmt::bytes(j.completed_bytes)))))
                (peers)
                @if torrent {
                    (tile("Ratio", format!("{:.2}", j.ratio()), Some(format!("{} up", fmt::bytes(j.uploaded_bytes)))))
                } @else {
                    (tile("Engine", (if j.engine == Engine::YtDlp { "yt-dlp" } else { "aria2" }).to_string(), None))
                }
            }
        }
    }
}

/// Files and peers (the part refreshed over SSE as `extra`, inner swap).
pub fn detail_extra(d: &JobDetail) -> Markup {
    const MAX_FILES: usize = 1000;
    let torrent = is_torrent(&d.job);
    html! {
        @if !d.files.is_empty() {
            section #files.panel role="tabpanel" aria-labelledby="t-files" {
                h2.ph { "Files " span.count { (d.files.len()) } }
                div.tblwrap { table.tbl {
                    thead { tr { th { "File" } th.num { "Size" } th.progcol { "Progress" } } }
                    tbody {
                        @for f in d.files.iter().take(MAX_FILES) {
                            @let frac = if f.length == 0 { 0.0 } else { f.completed as f64 / f.length as f64 };
                            tr.skipped[!f.selected] {
                                td.name data-label="File" { (f.path) }
                                td.num data-label="Size" { (fmt::bytes(f.length)) }
                                td.prog data-label="Progress" {
                                    @if f.selected {
                                        (bar(None, "bar small", Some(frac), "File progress"))
                                        span.pct { (format!("{:.0}%", frac * 100.0)) }
                                    } @else { span.muted { "skipped" } }
                                }
                            }
                        }
                    }
                } }
                @if d.files.len() > MAX_FILES { p.help { "Showing the first " (MAX_FILES) " files." } }
            }
        }
        @if torrent {
            section #peers.panel role="tabpanel" aria-labelledby="t-peers" {
                h2.ph { "Peers " span.count { (d.peers.len()) } }
                @if d.peers.is_empty() {
                    p.muted.pempty { "No peers connected right now." }
                } @else {
                    div.tblwrap { table.tbl {
                        thead { tr { th { "Address" } th.num { "Down" } th.num { "Up" } th.num { "Has" } th { "Seeder" } } }
                        tbody { @for p in &d.peers { tr {
                            td.mono data-label="Address" { (p.ip) ":" (p.port) }
                            td.num data-label="Down" { (fmt::speed(p.download_speed)) }
                            td.num data-label="Up" { (fmt::speed(p.upload_speed)) }
                            td.num data-label="Has" { (format!("{:.0}%", p.progress * 100.0)) }
                            td data-label="Seeder" { (if p.seeder { "yes" } else { "no" }) }
                        } } }
                    } }
                }
            }
        }
    }
}

/// Values shown in the options form (as typed).
#[derive(Debug, Clone, Default)]
pub struct OptValues {
    pub download: String,
    pub upload: String,
    pub seed_ratio: String,
    pub seed_time: String,
}

impl OptValues {
    pub fn from_options(o: &JobOptions) -> Self {
        Self {
            download: fmt::rate_input(o.max_download_limit.unwrap_or(0)),
            upload: fmt::rate_input(o.max_upload_limit.unwrap_or(0)),
            seed_ratio: o.seed_ratio.map(|r| format!("{r}")).unwrap_or_default(),
            seed_time: o.seed_time.map(|t| t.to_string()).unwrap_or_default(),
        }
    }
}

/// Result line shown under the options form.
pub enum Note<'a> {
    None,
    Saved,
    /// `(field name or "", message)`.
    Error(&'a str, &'a str),
}

pub fn options_form(
    base: &str,
    csrf: &str,
    j: &Job,
    orig: &OptValues,
    cur: &OptValues,
    note: Note<'_>,
) -> Markup {
    let id = j.id.0;
    let url = format!("{base}/jobs/{id}/limits");
    let err = |name: &str| match note {
        Note::Error(f, m) if f == name => Some(m),
        _ => None,
    };
    let locked = j.state.is_stopped();
    let rate_help = "Per second, e.g. 500K or 2M. Empty = no limit.";
    html! {
        form #optform.stack method="post" action=(url) hx-post=(url) hx-target="this" hx-swap="outerHTML" {
            input type="hidden" name="_csrf" value=(csrf);
            input type="hidden" name="o_download" value=(orig.download);
            input type="hidden" name="o_upload" value=(orig.upload);
            input type="hidden" name="o_seed_ratio" value=(orig.seed_ratio);
            input type="hidden" name="o_seed_time" value=(orig.seed_time);
            @if locked { p.help { "This download has finished, so its options can no longer be changed." } }
            @if j.engine == Engine::YtDlp && j.state == JobState::Active {
                p.help { "Pause the video download first: yt-dlp only takes options when it starts." }
            }
            fieldset.grid2 disabled[locked] {
                legend.sr { "Speed limits" }
                (field(&Field { id: "o-down", name: "download", label: "Download limit", value: &cur.download,
                    placeholder: "no limit", help: rate_help, error: err("download"), kind: "text" }))
                (field(&Field { id: "o-up", name: "upload", label: "Upload limit", value: &cur.upload,
                    placeholder: "no limit", help: rate_help, error: err("upload"), kind: "text" }))
                @if is_torrent(j) {
                    (field(&Field { id: "o-ratio", name: "seed_ratio", label: "Stop seeding at ratio", value: &cur.seed_ratio,
                        placeholder: "default", help: "Uploaded \u{f7} downloaded, e.g. 1.0. Setting a ratio or a time turns seeding on for this download.",
                        error: err("seed_ratio"), kind: "text" }))
                    (field(&Field { id: "o-time", name: "seed_time", label: "Stop seeding after (minutes)", value: &cur.seed_time,
                        placeholder: "default", help: "Empty = use the default from the config.", error: err("seed_time"), kind: "text" }))
                }
            }
            div.btnrow {
                button.btn.primary type="submit" disabled[locked] { "Save options" }
                @match note {
                    Note::Saved => { span.okmsg role="status" { "Saved." } }
                    Note::Error("", m) => { p.ferr role="alert" { (m) } }
                    _ => {}
                }
            }
        }
    }
}

/// Inline rename form, or the reason it is not possible.
pub fn rename_form(
    base: &str,
    csrf: &str,
    d: &JobDetail,
    value: &str,
    error: Option<&str>,
) -> Markup {
    let j = &d.job;
    let id = j.id.0;
    let url = format!("{base}/jobs/{id}/rename");
    let can = j.state.is_renamable_in_place() || j.state == JobState::Complete;
    let pick_file = d.files.len() > 1 && j.state != JobState::Complete;
    html! {
        div #renamebox {
            @if can {
                form.stack method="post" action=(url) hx-post=(url) hx-target="#renamebox" hx-swap="outerHTML" {
                    input type="hidden" name="_csrf" value=(csrf);
                    @if pick_file {
                        div.field {
                            label for="r-file" { "File to rename" }
                            select #r-file name="file_index" {
                                @for f in &d.files { option value=(f.index) { (f.path) } }
                            }
                        }
                    }
                    div.field.bad[error.is_some()] {
                        label for="r-name" { "New name" }
                        div.prow {
                            input #r-name type="text" name="name" value=(value) required maxlength="255" autocomplete="off"
                                aria-invalid=[error.map(|_| "true")];
                            button.btn type="submit" { "Rename" }
                        }
                        @if let Some(e) = error { p.ferr role="alert" { (e) } }
                        p.help {
                            @if j.state == JobState::Complete { "The files are moved on disk and the rename is recorded." }
                            @else { "A plain name without slashes. Applied when the download starts." }
                        }
                    }
                }
            } @else {
                p.help { "A running download cannot be renamed. Pause it first, or rename it after it has finished." }
            }
            @if !d.renames.is_empty() {
                ul.history { @for r in &d.renames { li { (r.from) " \u{2192} " (r.to) " " span.muted { "(" (fmt::timestamp(r.at)) " UTC)" } } } }
            }
        }
    }
}

pub fn detail_page(base: &str, csrf: &str, d: &JobDetail) -> Markup {
    let j = &d.job;
    let torrent = is_torrent(j);
    let orig = OptValues::from_options(&d.options);
    let mut tabs: Vec<(&str, &str)> = Vec::new();
    if !d.files.is_empty() {
        tabs.push(("files", "Files"));
    }
    if torrent {
        tabs.push(("peers", "Peers"));
        tabs.push(("trackers", "Trackers"));
    }
    tabs.push(("options", "Options"));
    tabs.push(("info", "Info"));
    let first = tabs[0].0;
    html! {
        a.back href=(format!("{base}/")) { (icon(base, "arrow-left")) span { "All downloads" } }
        (detail_head(base, Some(csrf), j))
        nav.tabs role="tablist" aria-label="Sections" {
            @for (id, label) in &tabs {
                a #(format!("t-{id}")) href=(format!("#{id}")) role="tab" data-tab=(id)
                    aria-controls=(id) aria-selected=(if *id == first { "true" } else { "false" }) { (label) }
            }
        }
        div.panels data-tab=(first) {
            div #dextra sse-swap="extra" hx-swap="innerHTML" { (detail_extra(d)) }
            @if torrent {
                section #trackers.panel role="tabpanel" aria-labelledby="t-trackers" {
                    h2.ph { "Trackers " span.count { (d.trackers.len()) } }
                    p.help { "dman does not query trackers, so their status shows as unknown." }
                    @if d.trackers.is_empty() { p.muted { "No trackers (DHT and peer exchange only)." } }
                    @else {
                        div.tblwrap { table.tbl {
                            thead { tr { th { "URL" } th.num { "Tier" } th { "Status" } } }
                            tbody { @for t in &d.trackers { tr {
                                td.name data-label="URL" { (t.url) }
                                td.num data-label="Tier" { (t.tier) }
                                td data-label="Status" { span.muted { "unknown" } }
                            } } }
                        } }
                    }
                }
            }
            section #options.panel role="tabpanel" aria-labelledby="t-options" {
                div.card {
                    h2.ph { "Speed and seeding" }
                    (options_form(base, csrf, j, &orig, &orig, Note::None))
                }
                div.card {
                    h2.ph { "Rename" }
                    (rename_form(base, csrf, d, display_name(j), None))
                }
            }
            section #info.panel role="tabpanel" aria-labelledby="t-info" {
                h2.ph { "Info" }
                dl.kv {
                    div { dt { "Source" } dd.wrap.mono { (j.source) } }
                    div { dt { "Folder" } dd.wrap.mono { (j.dir) } }
                    @if let Some(h) = d.info_hash.as_ref().or(j.info_hash.as_ref()) { div { dt { "Info hash" } dd.wrap.mono { (h) } } }
                    @if let Some(c) = &d.comment { div { dt { "Comment" } dd.wrap { (c) } } }
                    div { dt { "Added" } dd { (fmt::timestamp(j.added_at)) " UTC" } }
                    @if let Some(c) = j.completed_at { div { dt { "Completed" } dd { (fmt::timestamp(c)) " UTC" } } }
                    div { dt { "Engine" } dd { (if j.engine == Engine::YtDlp { "yt-dlp" } else { "aria2" }) } }
                }
            }
        }
    }
}
