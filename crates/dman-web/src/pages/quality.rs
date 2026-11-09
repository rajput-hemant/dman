//! The video quality picker flow of the web UI (see `dman_proto::video`): when a
//! single video URL is added, ask the daemon for the quality preferences; if it
//! wants a choice, probe the URL and show the picker as a normal form page. The
//! server keeps the probe, so the browser only sends row numbers and the
//! `format_id` is computed here, never trusted from the client.

use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use dman_proto::method;
use dman_proto::{
    AddSource, JobOptions, VideoOptions, VideoPreferences, VideoProbe, VideoProbeParams,
};

use super::add::add_sources;
use super::{Pairs, St, fail, full_page, redirect};
use crate::AppState;
use crate::auth::{Ctx, random_id};
use crate::quality::{self, Selection};
use crate::views::{self, Nav, QualityPage};

/// A probed video waiting for the user's quality choice.
#[derive(Clone)]
pub struct StoredQuality {
    at: Instant,
    /// The video options of the add (nothing decided yet).
    video: VideoOptions,
    url: String,
    options: JobOptions,
    paused: bool,
    probe: VideoProbe,
    /// The page the add came from (`downloads` or `add`), for Cancel.
    from: String,
}

const TTL: Duration = Duration::from_secs(30 * 60);
const MAX_STORED: usize = 20;

/// Result of [`gate`].
pub enum Gate {
    /// Add right away (no single video, no asking, or nothing to choose).
    Direct,
    /// Show the picker stored under this id.
    Picker(String),
    /// The probe failed.
    Failed(String),
}

/// The video options decide nothing about quality or playlist yet.
fn undecided(v: &VideoOptions) -> bool {
    v.quality.is_none()
        && v.format_id.is_none()
        && v.audio_only.is_none()
        && !v.playlist
        && v.playlist_items.is_none()
}

/// The quality flow for the sources of an add: only a single video URL without
/// an explicit quality is checked.
pub async fn gate(
    st: &AppState,
    sources: &[AddSource],
    options: &JobOptions,
    paused: bool,
    from: &str,
) -> Gate {
    let [AddSource::Video { url, video }] = sources else {
        return Gate::Direct;
    };
    if !undecided(video) {
        return Gate::Direct;
    }
    // Without readable preferences the daemon's defaults apply.
    let Ok(prefs) = st
        .call::<VideoPreferences>(method::VIDEO_PREFERENCES_GET, ())
        .await
    else {
        return Gate::Direct;
    };
    if !prefs.ask_quality {
        return Gate::Direct;
    }
    let probe = match st
        .call::<VideoProbe>(
            method::VIDEO_PROBE,
            VideoProbeParams {
                url: url.clone(),
                playlist: false,
                cookies_from_browser: video.cookies_from_browser.clone(),
            },
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return Gate::Failed(format!(
                "Could not check the available qualities of {url}: {}",
                e.message
            ));
        }
    };
    // Playlists keep the defaults; one quality or one format needs no question.
    if probe.playlist || !probe.needs_choice {
        return Gate::Direct;
    }
    let id = random_id();
    if let Ok(mut m) = st.qualities.lock() {
        m.retain(|_, s| s.at.elapsed() < TTL);
        if m.len() >= MAX_STORED
            && let Some(old) = m.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone())
        {
            m.remove(&old);
        }
        m.insert(
            id.clone(),
            StoredQuality {
                at: Instant::now(),
                video: video.as_ref().clone(),
                url: url.clone(),
                options: options.clone(),
                paused,
                probe,
                from: from.to_owned(),
            },
        );
    }
    Gate::Picker(id)
}

fn lookup(st: &AppState, id: &str) -> Option<StoredQuality> {
    let m = st.qualities.lock().ok()?;
    m.get(id).filter(|s| s.at.elapsed() < TTL).cloned()
}

async fn expired(st: &AppState, c: &Ctx, h: &HeaderMap) -> Response {
    fail(
        st,
        c,
        h,
        StatusCode::NOT_FOUND,
        "This quality choice has expired. Add the link again.",
    )
    .await
}

/// What the picker shows besides the probe.
struct View {
    sel: Selection,
    /// "Use as my default" and "Never ask again".
    checks: (bool, bool),
    message: Option<(&'static str, String)>,
}

async fn render(
    st: &AppState,
    c: &Ctx,
    id: &str,
    s: &StoredQuality,
    v: View,
    status: StatusCode,
) -> Response {
    let cancel = if s.from == "add" {
        format!("{}/add", st.base)
    } else {
        format!("{}/", st.base)
    };
    let page = QualityPage {
        id,
        probe: &s.probe,
        sel: v.sel,
        use_default: v.checks.0,
        never_ask: v.checks.1,
        message: v.message,
        cancel,
    };
    let mut r = full_page(
        st,
        c,
        Nav::Add,
        "Choose the quality",
        "",
        Vec::new(),
        views::quality_page(&st.base, &c.csrf, &page),
    )
    .await;
    *r.status_mut() = status;
    r
}

/// `GET /add/quality/{id}`: the picker, preselecting the daemon's suggestion.
pub async fn quality_form(
    State(st): St,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    RawQuery(_raw): RawQuery,
) -> Response {
    let Some(s) = lookup(&st, &id) else {
        return expired(&st, &c, &HeaderMap::new()).await;
    };
    let sel = quality::initial(&s.probe);
    let v = View {
        sel,
        checks: (false, false),
        message: None,
    };
    render(&st, &c, &id, &s, v, StatusCode::OK).await
}

fn index(p: &Pairs, key: &str) -> Option<usize> {
    p.text(key).and_then(|v| v.parse().ok())
}

/// `POST /add/quality/{id}`: a preset button re-renders with that selection;
/// "Download" computes the format id, saves the ticked preferences and adds.
pub async fn quality_submit(
    State(st): St,
    Extension(c): Extension<Ctx>,
    h: HeaderMap,
    Path(id): Path<String>,
    p: Pairs,
) -> Response {
    let Some(s) = lookup(&st, &id) else {
        return expired(&st, &c, &h).await;
    };
    let initial = quality::initial(&s.probe);
    let sel = Selection {
        video: index(&p, "video").unwrap_or(initial.video),
        audio: index(&p, "audio").unwrap_or(initial.audio),
    };
    let checks = (p.flag("use_default"), p.flag("never_ask"));
    if let Some(preset) = p.text("preset") {
        let sel = quality::apply_preset(&s.probe, preset, sel);
        let note = match quality::chosen(&s.probe, sel) {
            Ok(_) => (
                "ok",
                "Selected. Check the choice below, then press Download.".into(),
            ),
            Err(m) => ("error", m),
        };
        let v = View {
            sel,
            checks,
            message: Some(note),
        };
        return render(&st, &c, &id, &s, v, StatusCode::OK).await;
    }
    let bad = |m: String| {
        let v = View {
            sel,
            checks,
            message: Some(("error", m)),
        };
        render(&st, &c, &id, &s, v, StatusCode::BAD_REQUEST)
    };
    let chosen = match quality::chosen(&s.probe, sel) {
        Ok(ch) => ch,
        Err(m) => return bad(m).await,
    };
    if let Some(update) = quality::prefs_update(&chosen, checks.0, checks.1)
        && let Err(e) = st
            .call::<VideoPreferences>(method::VIDEO_PREFERENCES_SET, update)
            .await
    {
        return bad(format!(
            "Could not save your quality settings: {}",
            e.message
        ))
        .await;
    }
    let video = VideoOptions {
        format_id: Some(chosen.format_id),
        ..s.video.clone()
    };
    let source = (
        s.url.clone(),
        AddSource::Video {
            url: s.url.clone(),
            video: Box::new(video),
        },
    );
    match add_sources(&st, vec![source], s.options.clone(), s.paused).await {
        Ok((ok, _)) if !ok.is_empty() => {
            if let Ok(mut m) = st.qualities.lock() {
                m.remove(&id);
            }
            let ids = ok.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            redirect(&format!("{}/?added={ids}", st.base))
        }
        Ok((_, failed)) => {
            let m = failed
                .first()
                .map_or_else(|| "Could not add the video.".to_owned(), |(_, m)| m.clone());
            let r = bad(format!("Could not add: {m}")).await;
            let mut r = r;
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            r
        }
        Err(e) => {
            let mut r = bad(format!("Could not add: {}", e.message)).await;
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            r
        }
    }
}
