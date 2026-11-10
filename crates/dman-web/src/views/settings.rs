//! Settings: speed limits, queue, schedule, search providers, web access, engines.

use dman_proto::method::{EngineStatus, ReloadResult, ScheduleStatus};
use dman_proto::search::ProviderInfo;
use dman_proto::{Engine, Limits, VideoPreferences};
use maud::{Markup, html};

use super::{Field, field, icon, switch_row};
use crate::fmt;

/// How the browser reached this UI.
pub struct WebInfo {
    pub url: String,
    pub auth: bool,
    pub exposed: bool,
    pub tls: bool,
    /// Form token for the log-out button.
    pub csrf: String,
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
    /// Quality picker preferences; `None` hides the Video section.
    pub video: Option<&'a VideoPreferences>,
    pub web: WebInfo,
    /// Outcome of a just-pressed "Reload configuration" (error text on failure).
    pub reload: Option<&'a Result<ReloadResult, String>>,
}

pub fn provider_row(base: &str, csrf: &str, idx: usize, p: &ProviderInfo) -> Markup {
    let row = format!("prov-{idx}");
    let set = format!("{base}/settings/provider");
    let test = format!("{base}/settings/provider/test");
    html! {
        li.prov id=(row) {
            div.pinfo {
                div.pname {
                    strong { (p.name) } span.muted { (p.kind) }
                    @if p.verified { span.pill.complete title="Checked against the live site" { "verified" } }
                    @else { span.pill.paused title="Not checked against the live site; it may need fixes" { "unverified" } }
                }
                @if let Some(d) = &p.description { p.help { (d) } }
                @if !p.url.is_empty() { p.help.wrap.mono { (p.url) } }
            }
            div.pact {
                form.inline method="post" action=(test) hx-post=(test) hx-target="#toasts" hx-swap="beforeend" hx-disabled-elt="find button" {
                    input type="hidden" name="_csrf" value=(csrf);
                    input type="hidden" name="name" value=(p.name);
                    button.btn.small.ghost type="submit" { "Test" }
                }
                form.inline method="post" action=(set) hx-post=(set) hx-target=(format!("#{row}")) hx-swap="outerHTML" {
                    input type="hidden" name="_csrf" value=(csrf);
                    input type="hidden" name="name" value=(p.name);
                    input type="hidden" name="enabled" value=(if p.enabled { "false" } else { "true" });
                    input type="hidden" name="idx" value=(idx);
                    button.switch.on[p.enabled] type="submit" role="switch" aria-checked=(if p.enabled { "true" } else { "false" })
                        aria-label=(format!("Use {} for searches", p.name)) {
                        span.knob {} span.swst { (if p.enabled { "On" } else { "Off" }) }
                    }
                }
            }
        }
    }
}

/// Outcome of `config.reload`: what applies now and what needs a restart.
pub fn reload_result(r: &Result<ReloadResult, String>) -> Markup {
    html! {
        div #reload-result role="status" aria-live="polite" {
            @match r {
                Err(m) => { p.ferr role="alert" { "Could not reload: " (m) } }
                Ok(r) => {
                    @if r.applied.is_empty() && r.restart_required.is_empty() {
                        p.okmsg { "Reloaded. Nothing had changed." }
                    }
                    @if !r.applied.is_empty() {
                        p.okmsg { "Applied now: " (r.applied.join(", ")) "." }
                    }
                    @if !r.restart_required.is_empty() {
                        p.warnmsg { "Needs a restart of dman to take effect: " (r.restart_required.join(", ")) "." }
                    }
                }
            }
        }
    }
}

fn config_section(base: &str, csrf: &str, last: Option<&Result<ReloadResult, String>>) -> Markup {
    let url = format!("{base}/settings/reload");
    html! {
        section #config.card.set {
            (head("Configuration file", html! { "Edited the config file by hand? Reload it to apply the changes that do not need a restart." }))
            form.stack method="post" action=(url) hx-post=(url) hx-target="#reload-result" hx-swap="outerHTML" {
                input type="hidden" name="_csrf" value=(csrf);
                @match last { Some(r) => { (reload_result(r)) } None => { div #reload-result role="status" aria-live="polite" {} } }
                div.cardfoot { button.btn type="submit" { (icon(base, "retry")) span { "Reload configuration" } } }
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
                        @if s.available { span.pill.complete { span.dot {} "Ready" } } @else { span.pill.error { span.dot {} "Not available" } }
                        @if let Some(v) = &s.version { span.muted.mono { (v) } }
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
                            Some(v) => { span.pill.complete { span.dot {} "Ready" } span.muted.mono { (v.trim_start_matches("ffmpeg").trim()) } }
                            None => { span.pill.paused { span.dot {} "Not found" } }
                        }
                    }
                    p.help { "Optional. Lets yt-dlp merge separate video and audio streams. Without it, dman picks single-file formats or offers separate streams." }
                }
            }
        }
    }
}

const QUALITY_OPTIONS: [(&str, &str); 9] = [
    ("best", "Best available"),
    ("2160p", "2160p (4K)"),
    ("1440p", "1440p"),
    ("1080p", "1080p"),
    ("720p", "720p"),
    ("480p", "480p"),
    ("audio:m4a", "Audio only (m4a)"),
    ("audio:mp3", "Audio only (mp3)"),
    ("audio:opus", "Audio only (opus)"),
];
const AUDIO_OPTIONS: [(&str, &str); 4] = [
    ("best", "Best available"),
    ("m4a", "m4a (AAC)"),
    ("opus", "Opus"),
    ("mp3", "mp3"),
];
const CONTAINER_OPTIONS: [(&str, &str); 4] = [
    ("", "yt-dlp\u{2019}s choice"),
    ("mp4", "mp4"),
    ("mkv", "mkv"),
    ("webm", "webm"),
];

/// A select; a current value that is not in the list stays selectable.
fn select(id: &str, name: &str, label: &str, options: &[(&str, &str)], current: &str) -> Markup {
    let known = options.iter().any(|(v, _)| *v == current);
    html! {
        div.field {
            label for=(id) { (label) }
            select #(id) name=(name) {
                @for (v, l) in options { option value=(v) selected[*v == current] { (l) } }
                @if !known { option value=(current) selected { (current) } }
            }
        }
    }
}

fn head(title: &str, help: Markup) -> Markup {
    html! { div.chead2 { h2 { (title) } p.help { (help) } } }
}

fn video_section(base: &str, csrf: &str, v: &VideoPreferences) -> Markup {
    let action = format!("{base}/settings/video");
    html! {
        section #video.card.set {
            (head("Video", html! { "Downloads from video sites (YouTube and many others). When asking is on, dman shows the available qualities before it downloads." }))
            form.stack method="post" action=(action) {
                input type="hidden" name="_csrf" value=(csrf);
                div.swgroup { (switch_row("ask_quality", v.ask_quality, "Ask which quality to download", "Off: always use the defaults below.")) }
                div.grid3 {
                    (select("v-quality", "default_quality", "Default quality", &QUALITY_OPTIONS, &v.default_quality))
                    (select("v-audio", "default_audio", "Preferred audio", &AUDIO_OPTIONS, &v.default_audio))
                    (select("v-container", "default_container", "Container", &CONTAINER_OPTIONS, v.default_container.as_deref().unwrap_or("")))
                }
                div.cardfoot { button.btn.primary type="submit" { "Save video settings" } }
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
    let mut sections: Vec<(&str, &str)> = vec![
        ("limits", "Speed"),
        ("queue", "Queue"),
        ("schedule", "Schedule"),
    ];
    if s.video.is_some() {
        sections.push(("video", "Video"));
    }
    sections.extend([
        ("providers", "Search providers"),
        ("appearance", "Appearance"),
        ("web", "Web access"),
        ("config", "Configuration"),
        ("engines", "Engines"),
    ]);
    html! {
        div.pagehead { h1 { "Settings" } }
        div.setlayout {
            nav.setnav aria-label="Sections" {
                @for (id, l) in &sections { a href=(format!("#{id}")) { (l) } }
            }
            div.setcards {
                section #limits.card.set {
                    (head("Speed limits", html! { "Caps for all downloads together. They apply right away and are saved to the config file." }))
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
                        div.cardfoot { button.btn.primary type="submit" { "Save speed limits" } }
                    }
                }
                section #queue.card.set {
                    (head("Queue", html! { "How many downloads run at the same time. The rest wait in line and start in order." }))
                    form.stack method="post" action=(action) {
                        input type="hidden" name="_csrf" value=(csrf);
                        input type="hidden" name="o_max_concurrent" value=(orig.max_concurrent);
                        (field(&Field { id: "s-max", name: "max_concurrent", label: "Downloads at the same time", value: &v.max_concurrent,
                            placeholder: "3", help: "Seeding torrents do not count.",
                            error: err("max_concurrent"), kind: "number" }))
                        @if let Some((f, m)) = &s.error { @if f.is_empty() { p.ferr role="alert" { (m) } } }
                        div.cardfoot { button.btn.primary type="submit" { "Save queue size" } }
                    }
                }
                section #schedule.card.set {
                    (head("Schedule", html! { "Windows that pause downloads or change limits at set times. Edited in the config file (" code { "[[schedule]]" } ")." }))
                    @match s.schedule {
                        Some(sch) if !sch.windows.is_empty() => {
                            p.help {
                                "Times are in " (sch.time_zone) ". "
                                @if sch.paused { strong { "Downloads are held by a pause window right now. " } }
                                @if let Some(a) = &sch.active { "Active window: " (a) ". " }
                                @if let Some(n) = sch.next_change { "Next change " (fmt::timestamp(n)) " UTC" @if let Some(w) = &sch.next_window { " (" (w) ")" } "." }
                            }
                            div.tblwrap { table.tbl {
                                thead { tr { th { "Window" } th { "Days" } th { "Time" } th { "Does" } th.num { "Down" } th.num { "Up" } } }
                                tbody { @for w in &sch.windows { tr {
                                    td data-label="Window" { (w.name) }
                                    td data-label="Days" { (w.days.join(", ")) }
                                    td data-label="Time" { (w.start) "\u{2013}" (w.end) }
                                    td data-label="Does" { (if w.action == "pause" { "pause downloads" } else { "run downloads" }) }
                                    td.num data-label="Down" { (w.download_limit.map_or("\u{2013}".to_string(), rate)) }
                                    td.num data-label="Up" { (w.upload_limit.map_or("\u{2013}".to_string(), rate)) }
                                } } }
                            } }
                        }
                        _ => { p.muted { "No schedule: downloads run any time." } }
                    }
                }
                @if let Some(v) = s.video { (video_section(base, csrf, v)) }
                section #providers.card.set {
                    (head("Search providers", html! { "Sites the Search page asks. All are off by default; only enable sites that are legal where you live. " strong { "Test" } " runs a sample search." }))
                    @if s.providers.is_empty() { p.muted { "No providers are configured." } }
                    ul.provs { @for (i, p) in s.providers.iter().enumerate() { (provider_row(base, csrf, i, p)) } }
                }
                section #appearance.card.set {
                    (head("Appearance", html! { "Saved in this browser only." }))
                    div.themepick.jsonly role="group" aria-label="Theme" {
                        @for (v, l, i) in [("system", "System", "monitor"), ("light", "Light", "sun"), ("dark", "Dark", "moon")] {
                            button.btn type="button" data-theme-set=(v) aria-pressed="false" { (icon(base, i)) span { (l) } }
                        }
                    }
                    p.help.nojs { "Follows your system setting (light or dark). Turn on JavaScript to choose." }
                }
                section #web.card.set {
                    (head("Web access", html! { "Bind address, port, login and allowed host names are set in the " code { "[web]" } " section of the config file." }))
                    dl.kv {
                        div { dt { "Address" } dd.wrap { code { (s.web.url) } } }
                        div { dt { "Login" } dd {
                            @if s.web.auth { "Access token required. Show it on the dman host with " code { "dman serve --print-token" } "." }
                            @else if s.web.exposed { span.warnmsg { "No access token: anyone who can reach this address controls dman." } }
                            @else { "Not needed: this web UI only listens on this computer. Other websites cannot use it." }
                        } }
                        div { dt { "Network" } dd {
                            @if s.web.exposed { "Reachable from other machines." } @else { "Only reachable from this computer (loopback)." }
                            @if s.web.tls { " Encrypted (HTTPS)." }
                        } }
                    }
                    @if s.web.auth {
                        form.cardfoot method="post" action=(format!("{base}/logout")) {
                            input type="hidden" name="_csrf" value=(s.web.csrf);
                            button.btn type="submit" { (icon(base, "logout")) span { "Log out" } }
                        }
                    }
                }
                (config_section(base, csrf, s.reload))
                section #engines.card.set {
                    (head("Engines", html! { "Programs dman drives on the host." }))
                    (engines(s.engines))
                }
            }
        }
    }
}
