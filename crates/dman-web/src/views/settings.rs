//! Settings: speed limits, queue, schedule, search providers, web access, engines.

use dman_proto::method::{EngineStatus, ScheduleStatus};
use dman_proto::search::ProviderInfo;
use dman_proto::{Engine, Limits};
use maud::{Markup, html};

use super::{Field, field};
use crate::fmt;

/// How the browser reached this UI.
pub struct WebInfo {
    pub url: String,
    pub auth: bool,
    pub exposed: bool,
    pub tls: bool,
}

/// Limit fields as typed, with an optional `(field, message)` error.
#[derive(Debug, Clone, Default)]
pub struct LimitValues {
    pub download: String,
    pub upload: String,
    pub max_concurrent: String,
}

impl LimitValues {
    pub fn from_limits(l: Option<&Limits>) -> Self {
        Self {
            download: l.map(|l| fmt::rate_input(l.download)).unwrap_or_default(),
            upload: l.map(|l| fmt::rate_input(l.upload)).unwrap_or_default(),
            max_concurrent: l.map(|l| l.max_concurrent.to_string()).unwrap_or_default(),
        }
    }
}

pub struct SettingsData<'a> {
    pub limits: Option<&'a Limits>,
    pub values: LimitValues,
    pub error: Option<(&'a str, String)>,
    pub providers: &'a [ProviderInfo],
    pub schedule: Option<&'a ScheduleStatus>,
    pub engines: &'a [EngineStatus],
    pub web: WebInfo,
}

pub fn provider_row(base: &str, csrf: &str, idx: usize, p: &ProviderInfo) -> Markup {
    let row = format!("prov-{idx}");
    let set = format!("{base}/settings/provider");
    let test = format!("{base}/settings/provider/test");
    html! {
        li.prov id=(row) {
            div.pinfo {
                strong { (p.name) } " " span.muted { (p.kind) }
                @if p.verified { " " span.badge.complete title="Checked against the live site" { "verified" } }
                @else { " " span.badge.paused title="Not checked against the live site; it may need fixes" { "unverified" } }
                @if let Some(d) = &p.description { p.help { (d) } }
                @if !p.url.is_empty() { p.help.wrap { (p.url) } }
            }
            div.btnrow {
                form.inline method="post" action=(set) hx-post=(set) hx-target=(format!("#{row}")) hx-swap="outerHTML" {
                    input type="hidden" name="_csrf" value=(csrf);
                    input type="hidden" name="name" value=(p.name);
                    input type="hidden" name="enabled" value=(if p.enabled { "false" } else { "true" });
                    input type="hidden" name="idx" value=(idx);
                    button.switch.on[p.enabled] type="submit" role="switch" aria-checked=(if p.enabled { "true" } else { "false" })
                        aria-label=(format!("Use {} for searches", p.name)) {
                        span.knob {} (if p.enabled { "On" } else { "Off" })
                    }
                }
                form.inline method="post" action=(test) hx-post=(test) hx-target="#toasts" hx-swap="beforeend" hx-disabled-elt="find button" {
                    input type="hidden" name="_csrf" value=(csrf);
                    input type="hidden" name="name" value=(p.name);
                    button.btn.small type="submit" { "Test" }
                }
            }
        }
    }
}

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::Aria2 => "aria2",
        Engine::YtDlp => "yt-dlp",
    }
}

fn engine_purpose(e: Engine) -> &'static str {
    match e {
        Engine::Aria2 => {
            "Required. Downloads torrents, magnets, HTTP/FTP/SFTP links and Metalink files."
        }
        Engine::YtDlp => "Optional. Downloads from YouTube and many other video sites.",
    }
}

fn engines(list: &[EngineStatus]) -> Markup {
    let ffmpeg = list
        .iter()
        .find(|s| s.engine == Engine::YtDlp && s.available)
        .map(|s| s.features.iter().find(|f| f.starts_with("ffmpeg")).cloned());
    html! {
        @if list.is_empty() { p.muted { "Engine status is not available." } }
        ul.engines {
            @for s in list {
                li {
                    div.ehead {
                        strong { (engine_name(s.engine)) }
                        @if s.available { span.badge.complete { "Ready" } } @else { span.badge.error { "Not available" } }
                        @if let Some(v) = &s.version { span.muted { (v) } }
                    }
                    p.help { (engine_purpose(s.engine)) }
                    @if let Some(r) = &s.reason { p.warnmsg { (r) } }
                }
            }
            @if let Some(ff) = ffmpeg {
                li {
                    div.ehead {
                        strong { "ffmpeg" }
                        @match &ff {
                            Some(v) => { span.badge.complete { "Ready" } span.muted { (v.trim_start_matches("ffmpeg").trim()) } }
                            None => { span.badge.paused { "Not found" } }
                        }
                    }
                    p.help { "Optional. Lets yt-dlp merge separate video and audio streams. Without it, dman picks single-file formats or offers separate streams." }
                }
            }
        }
    }
}

pub fn settings_page(base: &str, csrf: &str, s: &SettingsData<'_>) -> Markup {
    let rate = |n: u64| {
        if n == 0 {
            "no limit".to_string()
        } else {
            fmt::bytes(n) + "/s"
        }
    };
    let err = |name: &str| match &s.error {
        Some((f, m)) if *f == name => Some(m.as_str()),
        _ => None,
    };
    let v = &s.values;
    let orig = LimitValues::from_limits(s.limits);
    let rate_help = "Per second, e.g. 500K, 2M or 1.5G. Empty = no limit.";
    let action = format!("{base}/settings/limits");
    html! {
        h1 { "Settings" }
        nav.tabs aria-label="Sections" {
            a href="#limits" { "Speed" } a href="#queue" { "Queue" } a href="#schedule" { "Schedule" }
            a href="#providers" { "Search providers" } a href="#web" { "Web access" } a href="#engines" { "Engines" }
        }
        section #limits.card {
            h2 { "Speed limits" }
            p.help { "Caps for all downloads together. They apply right away and are saved to the config file." }
            form.stack method="post" action=(action) {
                input type="hidden" name="_csrf" value=(csrf);
                input type="hidden" name="o_download" value=(orig.download);
                input type="hidden" name="o_upload" value=(orig.upload);
                div.grid2 {
                    (field(&Field { id: "s-down", name: "download", label: "Download limit", value: &v.download,
                        placeholder: "no limit", help: rate_help, error: err("download"), kind: "text" }))
                    (field(&Field { id: "s-up", name: "upload", label: "Upload limit", value: &v.upload,
                        placeholder: "no limit", help: rate_help, error: err("upload"), kind: "text" }))
                }
                div.btnrow { button.btn.primary type="submit" { "Save speed limits" } }
            }
        }
        section #queue.card {
            h2 { "Queue" }
            form.stack method="post" action=(action) {
                input type="hidden" name="_csrf" value=(csrf);
                input type="hidden" name="o_max_concurrent" value=(orig.max_concurrent);
                (field(&Field { id: "s-max", name: "max_concurrent", label: "Downloads at the same time", value: &v.max_concurrent,
                    placeholder: "3", help: "The rest wait in the queue and start in order. Seeding torrents do not count.",
                    error: err("max_concurrent"), kind: "number" }))
                @if let Some((f, m)) = &s.error { @if f.is_empty() { p.ferr role="alert" { (m) } } }
                div.btnrow { button.btn.primary type="submit" { "Save queue size" } }
            }
        }
        section #schedule.card {
            h2 { "Schedule" }
            @match s.schedule {
                Some(sch) if !sch.windows.is_empty() => {
                    p.help {
                        "Times are in " (sch.time_zone) ". "
                        @if sch.paused { strong { "Downloads are held by a pause window right now. " } }
                        @if let Some(a) = &sch.active { "Active window: " (a) ". " }
                        @if let Some(n) = sch.next_change { "Next change " (fmt::timestamp(n)) " UTC" @if let Some(w) = &sch.next_window { " (" (w) ")" } "." }
                    }
                    table.list {
                        thead { tr { th { "Window" } th { "Days" } th { "Time" } th { "Does" } th.num { "Down" } th.num { "Up" } } }
                        tbody { @for w in &sch.windows { tr {
                            td data-label="Window" { (w.name) }
                            td data-label="Days" { (w.days.join(", ")) }
                            td data-label="Time" { (w.start) "\u{2013}" (w.end) }
                            td data-label="Does" { (if w.action == "pause" { "pause downloads" } else { "run downloads" }) }
                            td.num data-label="Down" { (w.download_limit.map_or("\u{2013}".to_string(), rate)) }
                            td.num data-label="Up" { (w.upload_limit.map_or("\u{2013}".to_string(), rate)) }
                        } } }
                    }
                }
                _ => { p.muted { "No schedule: downloads run any time." } }
            }
            p.help { "Schedule windows are edited in the config file (" code { "[[schedule]]" } ")." }
        }
        section #providers.card {
            h2 { "Search providers" }
            p.help { "Sites the Search page asks. All are off by default; only enable sites that are legal where you live. " strong { "Test" } " runs a sample search." }
            @if s.providers.is_empty() { p.muted { "No providers are configured." } }
            ul.provs { @for (i, p) in s.providers.iter().enumerate() { (provider_row(base, csrf, i, p)) } }
        }
        section #web.card {
            h2 { "Web access" }
            table.kv {
                tr { th { "Address" } td.wrap { code { (s.web.url) } } }
                tr { th { "Login" } td {
                    @if s.web.auth { "An access token is required. Show it on the daemon host with " code { "dman serve --print-token" } "." }
                    @else { span.warnmsg { "No access token: anyone who can reach this address controls dman." } }
                } }
                tr { th { "Network" } td {
                    @if s.web.exposed { "Reachable from other machines." } @else { "Only reachable from this computer (loopback)." }
                    @if s.web.tls { " Encrypted (HTTPS)." }
                } }
            }
            p.help { "Bind address, port and token are set in the " code { "[web]" } " section of the config file." }
        }
        section #engines.card {
            h2 { "Engines" }
            (engines(s.engines))
        }
    }
}
