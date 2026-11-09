//! Add page: links + upload + one big button first; everything else behind
//! "More options". Magnet file selection and the daemon-side folder picker.

use dman_proto::method::{FsListing, MagnetPreview};
use maud::{Markup, html};

use super::icon;
use crate::fmt;

/// What the user typed, kept across a failed submit.
#[derive(Debug, Clone, Default)]
pub struct AddValues {
    pub uris: String,
    pub dir: String,
    pub download_limit: String,
    pub paused: bool,
    pub seed: bool,
    pub as_video: bool,
    pub quality: String,
    pub playlist: bool,
}

impl AddValues {
    /// Any option behind "More options" differs from its default.
    fn has_options(&self) -> bool {
        !self.dir.is_empty()
            || !self.download_limit.is_empty()
            || self.paused
            || self.seed
            || self.as_video
            || self.playlist
    }
}

/// Field errors of the add form.
#[derive(Debug, Clone, Default)]
pub struct AddErrors {
    pub uris: Option<String>,
    pub dir: Option<String>,
    pub limit: Option<String>,
    /// Errors not tied to one field (one per failed source).
    pub general: Vec<String>,
}

impl AddErrors {
    pub fn any(&self) -> bool {
        self.uris.is_some()
            || self.dir.is_some()
            || self.limit.is_some()
            || !self.general.is_empty()
    }
}

/// Everything the add page shows.
pub struct AddPage<'a> {
    pub video: bool,
    pub default_dir: Option<&'a str>,
    pub values: &'a AddValues,
    pub errors: &'a AddErrors,
    /// Already rendered file chooser (after "Choose files…" without JavaScript).
    pub preview: Option<Markup>,
    /// Shown above the form, e.g. "Added 2 downloads; the lines below failed."
    pub note: Option<String>,
}

const QUALITIES: [(&str, &str); 9] = [
    ("", "Ask me, or use my default"),
    ("best", "Best available"),
    ("2160p", "2160p (4K)"),
    ("1440p", "1440p"),
    ("1080p", "1080p"),
    ("720p", "720p"),
    ("480p", "480p"),
    ("360p", "360p"),
    ("audio", "Audio only"),
];

/// A labelled switch row (a checkbox styled as a switch).
pub fn switch_row(name: &str, checked: bool, label: &str, help: &str) -> Markup {
    html! {
        label.swrow {
            span.swtext { span.swl { (label) } @if !help.is_empty() { span.help { (help) } } }
            input.sw type="checkbox" name=(name) value="1" checked[checked] role="switch";
        }
    }
}

pub fn add_page(base: &str, csrf: &str, p: &AddPage<'_>) -> Markup {
    let v = p.values;
    let e = p.errors;
    let open = v.has_options() || e.dir.is_some() || e.limit.is_some();
    let preview_url = format!("{base}/add/preview");
    html! {
        div.sheet {
            div.sheethead {
                span.eicon { (icon(base, "plus")) }
                h1 { "Add downloads" }
                p.sub { "Magnet links, web and FTP links, video pages, .torrent and .metalink files." }
            }
            @if let Some(n) = &p.note { p.notice role="status" { (n) } }
            form #addform.card.sheetcard method="post" action=(format!("{base}/add")) enctype="multipart/form-data"
                hx-encoding="multipart/form-data" {
                input type="hidden" name="_csrf" value=(csrf);
                input type="hidden" name="from" value="add";
                @if !e.general.is_empty() {
                    div.alert role="alert" {
                        p { strong { "Some links could not be added:" } }
                        ul { @for g in &e.general { li { (g) } } }
                    }
                }
                div.field.bad[e.uris.is_some()] {
                    label for="uris" { "Links" }
                    textarea #uris.big name="uris" rows="4" spellcheck="false" autocomplete="off" aria-describedby="uris-help"
                        data-kinds="uris-kind" aria-invalid=[e.uris.as_ref().map(|_| "true")]
                        placeholder="magnet:?xt=urn:btih:\u{2026}\nhttps://example.org/file.iso" { (v.uris) }
                    span #uris-kind.kinds aria-live="polite" {}
                    @if let Some(m) = &e.uris { p.ferr role="alert" { (m) } }
                    p.help #uris-help { "One per line: magnet links and http(s), ftp, sftp, .torrent or .metalink URLs." }
                }
                div.inlinerow {
                    button.btn.small type="submit" formaction=(preview_url) hx-post=(preview_url) hx-target="#preview"
                        hx-swap="innerHTML" hx-indicator="#pv-ind" hx-disabled-elt="this" { (icon(base, "magnet")) span { "Choose files\u{2026}" } }
                    span.help { "Pick which files of a single magnet link to download." }
                }
                div #pv-ind.htmx-indicator.skel role="status" aria-label="Fetching the file list" {
                    p.loading { span.spin {} "Fetching the file list\u{2026} this can take up to two minutes for magnets with few peers." }
                    span.sk {} span.sk {} span.sk.short {}
                }
                div #preview { @if let Some(pv) = &p.preview { (pv) } }
                div.field {
                    span.flabel { "Files" }
                    label.upload for="files" {
                        (icon(base, "upload"))
                        span { strong { "Choose .torrent or .metalink files" } span.help { "Up to 10 MiB \u{b7} you can also drop them anywhere" } }
                    }
                    input #files.sr type="file" name="file" accept=".torrent,.metalink,.meta4,application/x-bittorrent" multiple aria-describedby="files-help";
                    p.help #files-help.filenames {}
                }
                details.more open[open] {
                    summary { (icon(base, "chevron")) span { "More options" } span.sumhint { "Folder, speed limit, seeding" @if p.video { ", video" } } }
                    div.stack.moreb {
                        div.field.bad[e.dir.is_some()] {
                            label for="dir" { "Save to folder" }
                            div.inputrow {
                                input #dir type="text" name="dir" value=(v.dir) autocomplete="off"
                                    placeholder=(p.default_dir.map_or_else(|| "default download folder".to_string(), |d| format!("Default: {d}")))
                                    aria-describedby="dir-help" aria-invalid=[e.dir.as_ref().map(|_| "true")];
                                button.btn.jsonly type="button" hx-get=(format!("{base}/fs")) hx-include="#dir" hx-target="#fs-picker" hx-swap="innerHTML" { (icon(base, "folder")) span { "Browse\u{2026}" } }
                            }
                            @if let Some(m) = &e.dir { p.ferr role="alert" { (m) } }
                            p.help #dir-help { "A folder on the machine running dman. It is created if missing. Empty = default folder." }
                            div #fs-picker {}
                        }
                        div.field.bad[e.limit.is_some()] {
                            label for="limit" { "Download speed limit" }
                            input #limit type="text" name="download_limit" value=(v.download_limit) placeholder="no limit" autocomplete="off"
                                aria-describedby="limit-help" aria-invalid=[e.limit.as_ref().map(|_| "true")];
                            @if let Some(m) = &e.limit { p.ferr role="alert" { (m) } }
                            p.help #limit-help { "Per second for each new download, e.g. 500K or 2M. Empty = no limit." }
                        }
                        div.swgroup {
                            (switch_row("paused", v.paused, "Add paused", "Start it later yourself."))
                            (switch_row("seed", v.seed, "Seed after download", "Torrents: keep sharing with other peers. Off by default."))
                        }
                        @if p.video {
                            fieldset.group {
                                legend { "Video sites" }
                                div.swgroup {
                                    (switch_row("as_video", v.as_video, "The links are video pages", "YouTube and many other sites, downloaded with yt-dlp."))
                                    (switch_row("playlist", v.playlist, "Whole playlist", ""))
                                }
                                div.field {
                                    label for="quality" { "Quality" }
                                    select #quality name="quality" {
                                        @for (q, l) in QUALITIES { option value=(q) selected[v.quality == q] { (l) } }
                                    }
                                }
                            }
                        }
                    }
                }
                @if p.video { (super::quality::checking("add-checking", false)) }
                div.formfoot {
                    a.btn.ghost href=(format!("{base}/")) { "Cancel" }
                    button.btn.primary.lg type="submit" { (icon(base, "plus")) span { "Add" } }
                }
            }
        }
    }
}

/// File chooser for a previewed magnet; lives inside the add form.
pub fn preview_panel(id: &str, p: &MagnetPreview) -> Markup {
    html! {
        div.pick role="group" aria-labelledby="pick-title" {
            input type="hidden" name="preview_id" value=(id);
            div.pickhead {
                div {
                    h3 #pick-title.wrap { (p.name) }
                    p.help { (fmt::bytes(p.total_bytes)) " \u{b7} " (p.files.len()) " files \u{b7} untick what you do not want, then press Add." }
                }
                div.pickbtns.jsonly {
                    button.btn.small.ghost type="button" data-check="all" { "Select all" }
                    button.btn.small.ghost type="button" data-check="none" { "Select none" }
                }
            }
            ul.files {
                @for f in &p.files {
                    li { label.fileck {
                        input type="checkbox" name="select" value=(f.index) checked;
                        span.fpath { (f.path) }
                        span.fsize { (fmt::bytes(f.length)) }
                    } }
                }
            }
        }
    }
}

/// Inline message in place of the file chooser.
pub fn preview_error(msg: &str) -> Markup {
    html! { p.ferr role="alert" { (msg) } }
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn fs_url(base: &str, path: &str) -> String {
    let qs = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", path)
        .finish();
    format!("{base}/fs?{qs}")
}

/// Folder picker over the daemon's filesystem (`fs.list`).
pub fn fs_picker(base: &str, l: &FsListing) -> Markup {
    let dirs: Vec<_> = l.entries.iter().filter(|e| e.is_dir).collect();
    html! {
        div.picker role="group" aria-label="Choose a folder" {
            p.pickpath { (icon(base, "folder")) code { (l.path) } }
            ul.dirs {
                @if let Some(p) = &l.parent {
                    li { button.mi type="button" hx-get=(fs_url(base, p)) hx-target="#fs-picker" { (icon(base, "up")) span.bl { "Up one level" } } }
                }
                @for d in &dirs {
                    li { button.mi type="button" hx-get=(fs_url(base, &join(&l.path, &d.name))) hx-target="#fs-picker" { (icon(base, "folder")) span.bl { (d.name) "/" } } }
                }
                @if dirs.is_empty() { li.muted.none { "No subfolders." } }
            }
            @if l.truncated { p.help { "Long folder: not every entry is shown." } }
            div.btnrow {
                button.btn.primary.small type="button" data-pick=(l.path) { "Use this folder" }
                button.btn.small.ghost type="button" data-unpick { "Cancel" }
            }
        }
    }
}
