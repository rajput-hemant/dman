//! Compile-time HTML templates (maud). Everything interpolated is escaped by maud, so
//! titles from search providers and remote file names cannot inject markup.
//!
//! No inline scripts, no `style` attributes: the CSP is `default-src 'self'`. Small
//! enhancements (toasts, drag and drop, dialogs) live in `assets/app.js`; every core
//! flow also works as a plain form post without JavaScript.

mod add;
mod detail;
mod downloads;
mod quality;
mod search;
mod settings;

pub use add::*;
pub use detail::*;
pub use downloads::*;
pub use quality::*;
pub use search::*;
pub use settings::*;

use dman_proto::{GlobalStats, Job, JobState, SourceKind};
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
    /// Query string for `/events` (`rows=1`, `search=3`, `job=5`); empty = stats and
    /// messages only.
    pub sse: String,
    /// Toasts rendered with the page (results of the previous action).
    pub toasts: Vec<Markup>,
}

/// An icon from the embedded sprite (`static/icons.svg`), decorative.
pub fn icon(base: &str, name: &str) -> Markup {
    html! {
        svg.i aria-hidden="true" focusable="false" { use href=(format!("{}#{name}", crate::assets::url(base, "icons.svg"))) {} }
    }
}

fn nav_item(l: &Layout<'_>, which: Nav, path: &str, label: &str, ico: &str) -> Markup {
    let on = l.nav == which;
    html! {
        a href=(format!("{}{}", l.base, path)) class=[on.then_some("on")]
            aria-current=[on.then_some("page")] { (icon(l.base, ico)) span { (label) } }
    }
}

/// Wraps `content` in the app shell: sidebar (bottom tab bar on phones), top bar
/// with live totals and speed sparkline, toast stack, dialogs and the SSE
/// connection. `content` brings its own `<h1>`.
pub fn page(l: &Layout<'_>, title: &str, content: Markup) -> Markup {
    let headers = serde_json::json!({ "X-CSRF-Token": l.csrf }).to_string();
    let sse_url = if l.sse.is_empty() {
        format!("{}/events", l.base)
    } else {
        format!("{}/events?{}", l.base, l.sse)
    };
    let b = l.base;
    let theme = |v: &str, label: &str, ico: &str| {
        html! { button.ib type="button" data-theme-set=(v) aria-pressed="false" aria-label=(label) title=(label) { (icon(b, ico)) } }
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
                meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false,"selfRequestsOnly":true,"scrollIntoViewOnBoost":false,"attributesToSettle":["class","value"]}"#;
                meta name="color-scheme" content="light dark";
                meta name="csrf-token" content=(l.csrf);
                meta name="dman-base" content=(b);
                title { (title) " \u{b7} " (l.title) }
                link rel="stylesheet" href=(crate::assets::url(b, "app.css"));
                // Tiny and blocking on purpose: applies the saved theme before paint.
                script src=(crate::assets::url(b, "theme.js")) {}
                script src=(crate::assets::url(b, "htmx.min.js")) defer {}
                script src=(crate::assets::url(b, "sse.js")) defer {}
                script src=(crate::assets::url(b, "app.js")) defer {}
            }
            body hx-ext="sse" hx-headers=(headers) {
                a.skip href="#main" { "Skip to content" }
                div.shell sse-connect=(sse_url) {
                    aside.side {
                        a.brand href=(format!("{b}/")) aria-label=(format!("{} home", l.title)) {
                            span.logo { (icon(b, "dl")) } span.bname { (l.title) }
                        }
                        nav.nav aria-label="Main" {
                            (nav_item(l, Nav::Downloads, "/", "Downloads", "dl"))
                            (nav_item(l, Nav::Add, "/add", "Add", "plus"))
                            (nav_item(l, Nav::Search, "/search", "Search", "search"))
                            (nav_item(l, Nav::Settings, "/settings", "Settings", "sliders"))
                        }
                        div.sidefoot {
                            div.themes.jsonly role="group" aria-label="Theme" {
                                (theme("system", "Use the system theme", "monitor"))
                                (theme("light", "Light theme", "sun"))
                                (theme("dark", "Dark theme", "moon"))
                            }
                            @if l.show_logout {
                                form.logout method="post" action=(format!("{b}/logout")) {
                                    input type="hidden" name="_csrf" value=(l.csrf);
                                    button.navbtn type="submit" { (icon(b, "logout")) span { "Log out" } }
                                }
                            }
                        }
                    }
                    div.page {
                        header.top {
                            a.brand.mbrand href=(format!("{b}/")) aria-label=(format!("{} home", l.title)) { span.logo { (icon(b, "dl")) } }
                            div.totals {
                                svg #spark.spark.jsonly viewBox="0 0 120 32" preserveAspectRatio="none" aria-hidden="true" {
                                    path.sa d="M0 32H120" {} path.sl d="M0 31H120" {}
                                }
                                div #stats.stats sse-swap="stats" hx-swap="innerHTML" aria-label="Totals" {
                                    @if let Some(s) = l.stats { (stats_inner(s)) }
                                }
                            }
                            div.topact {
                                button.cmdbtn.jsonly type="button" data-cmdk aria-haspopup="dialog" aria-keyshortcuts="Control+K Meta+K" {
                                    (icon(b, "search")) span.cl { "Quick actions" } kbd { "Ctrl K" }
                                }
                                a.btn.primary.addbtn href=(format!("{b}/add")) { (icon(b, "plus")) span { "Add download" } }
                            }
                        }
                        @if l.exposed_warning {
                            div.banner role="alert" {
                                (icon(b, "alert"))
                                p {
                                    strong { "No access token. " }
                                    "This web UI is reachable from the network and anyone who can reach it can control your downloads. Set "
                                    code { "[web] token_ref" } " in the config and restart."
                                }
                            }
                        }
                        main #main { (content) }
                    }
                    div #toasts.toasts sse-swap="message" hx-swap="beforeend" aria-live="polite" {
                        @for t in &l.toasts { (t) }
                    }
                }
                dialog #modal.modal aria-labelledby="modal-title" {
                    div #modal-body {}
                }
                dialog #cmdk.cmdk aria-label="Quick actions" {
                    div.cmdin {
                        (icon(b, "search"))
                        input #cmdk-q type="text" role="combobox" aria-controls="cmdk-list" aria-expanded="true"
                            aria-autocomplete="list" autocomplete="off" spellcheck="false"
                            placeholder="Type a command, paste a link or search\u{2026}" aria-label="Command";
                        kbd { "Esc" }
                    }
                    ul #cmdk-list role="listbox" aria-label="Actions" {}
                    div.cmdfoot { span { kbd { "\u{2191}" } kbd { "\u{2193}" } " move" } span { kbd { "\u{21b5}" } " run" } }
                }
                div.dropzone aria-hidden="true" { (icon(b, "upload")) strong { "Drop .torrent files to add them" } }
                form #dropform.jsonly hidden method="post" action=(format!("{b}/add")) enctype="multipart/form-data"
                    hx-post=(format!("{b}/add")) hx-encoding="multipart/form-data" hx-target="#toasts" hx-swap="beforeend" {
                    input type="hidden" name="_csrf" value=(l.csrf);
                    input type="hidden" name="from" value="drop";
                    input type="file" name="file" multiple aria-hidden="true" tabindex="-1";
                }
            }
        }
    }
}

/// Inner HTML of the totals in the top bar. `data-down` feeds the sparkline.
pub fn stats_inner(s: &GlobalStats) -> Markup {
    html! {
        span.st.dn title="Total download speed" { span.ar { "\u{2193}" } b data-down=(s.download_speed) { (fmt::speed0(s.download_speed)) } }
        span.st.upl title="Total upload speed" { span.ar { "\u{2191}" } b { (fmt::speed0(s.upload_speed)) } }
        span.st.cnt { b { (s.num_active) } " active" }
        span.st.cnt { b { (s.num_waiting) } " queued" }
        @if let Some(d) = s.disk_free { span.st.cnt.free title="Free space in the download folder" { b { (fmt::bytes(d)) } " free" } }
        @if let Some(sch) = &s.schedule {
            @if sch.paused { span.st.warn { "Schedule: paused" } }
            @else if let Some(a) = &sch.active { span.st.cnt { "Schedule: " (a) } }
        }
    }
}

/// One toast. `level` is `info`, `ok`, `warn` or `error`.
pub fn toast(level: &str, text: &str) -> Markup {
    toast_link(level, text, None)
}

/// A toast with an optional follow-up action link (`(href, label)`, e.g. "View").
pub fn toast_link(level: &str, text: &str, link: Option<(&str, &str)>) -> Markup {
    let role = if level == "error" { "alert" } else { "status" };
    html! {
        div class=(format!("toast {level}")) role=(role) {
            span.tdot {}
            span.tmsg { (text) }
            @if let Some((href, label)) = link { a.tact href=(href) { (label) } }
            button.x type="button" data-dismiss aria-label="Dismiss" { "\u{d7}" }
        }
    }
}

/// "Removed ..." toast with an Undo button (`POST /jobs/restore`). A plain form,
/// so it works without JavaScript; with htmx the toast is replaced in place.
pub fn toast_undo(base: &str, csrf: &str, text: &str, ids: &[dman_proto::JobId]) -> Markup {
    let url = format!("{base}/jobs/restore");
    let list = ids
        .iter()
        .map(|i| i.0.to_string())
        .collect::<Vec<_>>()
        .join(",");
    html! {
        div class="toast ok" role="status" {
            span.tdot {}
            span.tmsg { (text) }
            form.undoform method="post" action=(url) hx-post=(url) hx-target="closest .toast" hx-swap="outerHTML" {
                input type="hidden" name="_csrf" value=(csrf);
                input type="hidden" name="ids" value=(list);
                button.tact.undo type="submit" { "Undo" }
            }
            button.x type="button" data-dismiss aria-label="Dismiss" { "\u{d7}" }
        }
    }
}

/// A toast delivered out of band next to another swap.
pub fn oob_toast(t: Markup) -> Markup {
    html! { div hx-swap-oob="beforeend:#toasts" { (t) } }
}

/// Human label for a state, as shown in badges.
pub fn state_label(j: &Job) -> &'static str {
    match j.state {
        JobState::Waiting => "Queued",
        JobState::Active if metadata_pending(j) => "Fetching info",
        JobState::Active => "Downloading",
        JobState::Seeding => "Seeding",
        JobState::Paused => "Paused",
        JobState::Complete => "Done",
        JobState::Error => "Failed",
        JobState::Removed => "Removed",
    }
}

/// CSS class name of a state.
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

/// A magnet whose metadata (file list, size) is still being fetched.
pub fn metadata_pending(j: &Job) -> bool {
    j.kind == SourceKind::Magnet && j.total_bytes == 0 && !j.state.is_stopped()
}

pub fn is_torrent(j: &Job) -> bool {
    matches!(j.kind, SourceKind::Magnet | SourceKind::Torrent)
}

/// Display name: the job name, else its source.
pub fn display_name(j: &Job) -> &str {
    if j.name.is_empty() {
        &j.source
    } else {
        &j.name
    }
}

/// A progress bar (`<progress>`, styled in CSS). `None` = indeterminate.
pub fn bar(id: Option<String>, class: &str, frac: Option<f64>, label: &str) -> Markup {
    html! {
        @match frac {
            Some(f) => {
                progress id=[id] class=(class) max="1000" value=((f.clamp(0.0, 1.0) * 1000.0).round() as u32)
                    aria-label=(label) { (format!("{:.0}%", f * 100.0)) }
            }
            None => { progress id=[id] class=(class) aria-label=(label) {} }
        }
    }
}

/// A small form that posts one action. With htmx it swaps `target`; without
/// JavaScript it is a plain form post. `csrf` is `None` for SSE-rendered markup (htmx
/// sends the token as a header there).
pub struct ActionForm<'a> {
    pub url: String,
    pub label: &'a str,
    pub class: &'a str,
    pub target: &'a str,
    pub csrf: Option<&'a str>,
    pub vals: &'a [(&'a str, &'a str)],
    pub include: Option<&'a str>,
    pub title: Option<String>,
    /// Icon shown before the label (the label stays as accessible text).
    pub icon: Option<Markup>,
}

pub fn action_form(a: &ActionForm<'_>) -> Markup {
    html! {
        form.inline method="post" action=(a.url) hx-post=(a.url) hx-target=(a.target)
            hx-swap="outerHTML" hx-include=[a.include] {
            @if let Some(c) = a.csrf { input type="hidden" name="_csrf" value=(c); }
            @for (k, v) in a.vals { input type="hidden" name=(k) value=(v); }
            button type="submit" class=(a.class) title=[a.title.as_deref()] {
                @if let Some(i) = &a.icon { (i) span.bl { (a.label) } } @else { (a.label) }
            }
        }
    }
}

/// Labeled input with helper text and an inline error.
pub struct Field<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub label: &'a str,
    pub value: &'a str,
    pub placeholder: &'a str,
    pub help: &'a str,
    pub error: Option<&'a str>,
    pub kind: &'a str,
}

pub fn field(f: &Field<'_>) -> Markup {
    let help_id = format!("{}-help", f.id);
    html! {
        div.field.bad[f.error.is_some()] {
            label for=(f.id) { (f.label) }
            input id=(f.id) type=(f.kind) name=(f.name) value=(f.value) placeholder=(f.placeholder)
                autocomplete="off" aria-describedby=(help_id) aria-invalid=[f.error.map(|_| "true")];
            @if let Some(e) = f.error { p.ferr role="alert" { (e) } }
            @if !f.help.is_empty() { p.help id=(help_id) { (f.help) } }
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
                title { "Log in \u{b7} " (title) }
                link rel="stylesheet" href=(crate::assets::url(base, "app.css"));
                script src=(crate::assets::url(base, "theme.js")) {}
            }
            body.center {
                main.login {
                    span.logo.big { (icon(base, "dl")) }
                    h1 { "Sign in to " (title) }
                    p.sub { "This web UI is reachable from the network, so it asks for the access token. Show it on the dman host with " code { "dman serve --print-token" } "." }
                    @if let Some(e) = error { p.ferr role="alert" { (e) } }
                    form.stack method="post" action=(format!("{base}/login")) {
                        div.field {
                            label for="token" { "Access token" }
                            input #token type="password" name="token" autocomplete="current-password" required autofocus;
                        }
                        button.btn.primary.lg.wide type="submit" { "Log in" }
                    }
                }
            }
        }
    }
}

pub fn error_page(base: &str, msg: &str) -> Markup {
    html! {
        div.emptyst {
            span.eicon.bad { (icon(base, "alert")) }
            h1 { "Something went wrong" }
            p.ferr role="alert" { (msg) }
            p { a.btn href=(format!("{base}/")) { (icon(base, "arrow-left")) span { "Back to downloads" } } }
        }
    }
}
