//! The video quality picker: a plain form (works without JavaScript). Probe text
//! (titles, labels, reasons) is untrusted and escaped by maud.

use dman_proto::VideoProbe;
use dman_proto::video::{QualityChoice, QualityChoiceKind};
use maud::{Markup, html};

use crate::fmt;
use crate::quality::{self, PRESETS, Selection};

/// What the picker page shows.
pub struct QualityPage<'a> {
    pub id: &'a str,
    pub probe: &'a VideoProbe,
    pub sel: Selection,
    pub use_default: bool,
    pub never_ask: bool,
    /// Why nothing was downloaded, or what a preset selected.
    pub message: Option<(&'a str, String)>,
    /// Where Cancel goes.
    pub cancel: String,
}

fn label_of(c: &QualityChoice) -> String {
    let mut label = c.label.clone();
    if !label.contains('~')
        && let Some(s) = c.size_estimate
    {
        label.push_str(&format!(" \u{b7} ~{}", fmt::bytes(s)));
    }
    label
}

fn row(name: &str, idx: usize, c: &QualityChoice, selected: bool) -> Markup {
    let kind = match c.kind {
        QualityChoiceKind::Muxed => "",
        QualityChoiceKind::VideoOnly => "video only, audio chosen below",
        QualityChoiceKind::AudioOnly => "",
    };
    html! {
        li class=(if c.available { "qrow" } else { "qrow off" }) {
            label.qopt {
                input type="radio" name=(name) value=(idx) checked[selected && c.available] disabled[!c.available];
                span {
                    span.qlabel { (label_of(c)) }
                    @if c.available && c.requires_ffmpeg { " " span.pill.paused title="ffmpeg merges the video and audio streams" { "needs ffmpeg" } }
                    @if !c.available {
                        " " span.pill.error { "unavailable" }
                        " " span.help { (c.unavailable_reason.as_deref().unwrap_or("needs ffmpeg")) }
                    } @else if c.kind == QualityChoiceKind::VideoOnly && !kind.is_empty() {
                        " " span.help { (kind) }
                    }
                }
            }
        }
    }
}

pub fn quality_page(base: &str, csrf: &str, p: &QualityPage<'_>) -> Markup {
    let probe = p.probe;
    let action = format!("{base}/add/quality/{}", p.id);
    let audio_only_row = quality::can_audio_only(probe);
    let applies = quality::audio_applies(probe, p.sel);
    let title = probe.title.clone().unwrap_or_else(|| probe.url.clone());
    html! {
        div.sheet {
        div.sheethead {
            span.eicon { (super::icon(base, "video")) }
            h1 { "Choose the quality" }
        }
        form #qualityform.card.sheetcard method="post" action=(action) {
            input type="hidden" name="_csrf" value=(csrf);
            // Enter in the form downloads (the preset buttons below also submit).
            button.sr type="submit" name="act" value="download" tabindex="-1" aria-hidden="true" { "Download" }
            div.qhead {
                h2.wrap { (title) }
                p.help {
                    @if let Some(u) = &probe.uploader { "by " (u) " " }
                    @if let Some(d) = probe.duration { "\u{b7} " (fmt::eta(Some(d.round().max(0.0) as u64))) " " }
                    span.wrap { (probe.url) }
                }
                @if !probe.ffmpeg { p.warnmsg { "ffmpeg is missing on the dman host, so qualities that need merging are unavailable." } }
            }
            @if let Some((level, m)) = &p.message {
                p class=(if *level == "error" { "ferr" } else { "okmsg" }) role=(if *level == "error" { "alert" } else { "status" }) { (m) }
            }
            div.qpresets role="group" aria-label="Quick choices" {
                span.help { "Quick choice:" }
                @for (v, l) in PRESETS {
                    button.btn.small type="submit" name="preset" value=(v) formnovalidate
                        disabled[quality::preset_row(probe, v).is_none()] { (l) }
                }
            }
            fieldset.group #qvideo {
                legend { "Video" }
                ul.qlist {
                    @for (i, c) in probe.video_choices.iter().enumerate() { (row("video", i, c, p.sel.video == i)) }
                    @if audio_only_row {
                        li.qrow { label.qopt {
                            input type="radio" name="video" value=(probe.video_choices.len()) checked[p.sel.video == probe.video_choices.len()];
                            span { span.qlabel { "Audio only (no video)" } }
                        } }
                    }
                }
            }
            @if !probe.audio_choices.is_empty() {
                fieldset.group #qaudio {
                    legend { "Audio" }
                    p.help { @if applies { "Goes with the video above." } @else { "Used for video-only qualities and for \u{201c}Audio only\u{201d}." } }
                    ul.qlist {
                        @for (i, c) in probe.audio_choices.iter().enumerate() { (row("audio", i, c, p.sel.audio == i)) }
                    }
                }
            }
            div.swgroup {
                (super::switch_row("use_default", p.use_default, "Use as my default", "Remember this quality for videos added without asking."))
                (super::switch_row("never_ask", p.never_ask, "Never ask again", "Always use my default; turn asking back on in Settings."))
            }
            div.formfoot {
                a.btn.ghost href=(p.cancel) { "Cancel" }
                button.btn.primary.lg type="submit" name="act" value="download" { "Download" }
            }
        }
        }
    }
}

/// "Checking available qualities" spinner. With `htmx` it is an htmx indicator
/// that only shows when the request takes a while; otherwise `app.js` reveals it
/// when a video form is submitted.
pub fn checking(id: &str, htmx: bool) -> Markup {
    html! {
        @if htmx {
            p id=(id) class="htmx-indicator checking loading slowind" role="status" { span.spin {} "Checking available qualities\u{2026}" }
        } @else {
            p id=(id) class="checking loading" role="status" hidden { span.spin {} "Checking available qualities\u{2026}" }
        }
    }
}
