//! Route handlers. Every state-changing handler answers htmx callers (`HX-Request`)
//! with a fragment plus a toast, and plain form posts with a redirect or a page, so
//! the core flows work without JavaScript.

mod add;
mod jobs;
mod search;
mod settings;

pub use add::*;
pub use jobs::*;
pub use search::*;
pub use settings::*;

use std::sync::Arc;

use axum::Extension;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use dman_proto::method;
use dman_proto::{ErrorCode, GlobalStats, RpcError};
use maud::Markup;

use crate::auth::Ctx;
use crate::views::{self, Layout, Nav};
use crate::{AppState, MAX_UPLOAD_BYTES};

pub(crate) type St = State<Arc<AppState>>;

// ------------------------------------------------------------------ form body

/// One uploaded file.
pub struct Upload {
    pub file_name: String,
    pub data: Vec<u8>,
}

/// Form body as ordered key/value pairs (repeated keys allowed), from either
/// `application/x-www-form-urlencoded` or `multipart/form-data` (with files).
pub struct Pairs {
    pairs: Vec<(String, String)>,
    pub files: Vec<Upload>,
    /// The body could not be read (too large, malformed multipart).
    pub error: Option<String>,
}

impl Pairs {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn all<'a>(&'a self, k: &'a str) -> impl Iterator<Item = &'a str> {
        self.pairs
            .iter()
            .filter(move |(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    /// Non-empty, trimmed value.
    pub fn text(&self, k: &str) -> Option<&str> {
        self.get(k).map(str::trim).filter(|v| !v.is_empty())
    }

    /// Trimmed value or `""`.
    pub fn str(&self, k: &str) -> &str {
        self.get(k).map_or("", str::trim)
    }

    pub fn flag(&self, k: &str) -> bool {
        self.get(k)
            .is_some_and(|v| !matches!(v, "" | "0" | "false" | "off"))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    fn from_query(q: &str) -> Self {
        Pairs {
            pairs: url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
            files: Vec::new(),
            error: None,
        }
    }
}

impl<S: Send + Sync> FromRequest<S> for Pairs {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let multipart = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("multipart/form-data"));
        if !multipart {
            let bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
                .await
                .map_err(|_| {
                    (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response()
                })?;
            return Ok(Pairs::from_query(&String::from_utf8_lossy(&bytes)));
        }
        let mut out = Pairs {
            pairs: Vec::new(),
            files: Vec::new(),
            error: None,
        };
        let mut mp = match Multipart::from_request(req, state).await {
            Ok(m) => m,
            Err(e) => {
                out.error = Some(format!("could not read the form: {e}"));
                return Ok(out);
            }
        };
        loop {
            let field = match mp.next_field().await {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => {
                    out.error = Some("The upload is too large (10 MiB at most).".into());
                    break;
                }
            };
            let name = field.name().unwrap_or("").to_string();
            match field.file_name().map(str::to_string) {
                Some(file_name) => match field.bytes().await {
                    // An empty file input still sends a part without a name or data.
                    Ok(d) if d.is_empty() && file_name.is_empty() => {}
                    Ok(d) if d.len() > MAX_UPLOAD_BYTES => {
                        out.error = Some("The upload is too large (10 MiB at most).".into());
                        break;
                    }
                    Ok(d) => out.files.push(Upload {
                        file_name,
                        data: d.to_vec(),
                    }),
                    Err(_) => {
                        out.error = Some("The upload is too large (10 MiB at most).".into());
                        break;
                    }
                },
                None => match field.text().await {
                    Ok(t) => out.pairs.push((name, t)),
                    Err(e) => {
                        out.error = Some(format!("could not read the form: {e}"));
                        break;
                    }
                },
            }
        }
        Ok(out)
    }
}

pub fn query_pairs(raw: &Option<String>) -> Pairs {
    Pairs::from_query(raw.as_deref().unwrap_or(""))
}

// ------------------------------------------------------------------ responses

pub fn html(m: Markup) -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        m.into_string(),
    )
        .into_response()
}

pub fn is_htmx(h: &HeaderMap) -> bool {
    h.contains_key("hx-request")
}

/// `HX-Target` without the leading `#`.
pub fn hx_target(h: &HeaderMap) -> &str {
    h.get("hx-target")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

pub fn redirect(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// Adds a response header (ignored when the value is not a valid header).
pub fn with_header(mut r: Response, name: &'static str, value: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(value) {
        r.headers_mut().insert(name, v);
    }
    r
}

/// A toast for htmx: always appended to `#toasts`, whatever the caller targeted.
pub fn toast_markup(m: Markup) -> Response {
    let mut r = html(m);
    let h = r.headers_mut();
    h.insert("hx-retarget", HeaderValue::from_static("#toasts"));
    h.insert("hx-reswap", HeaderValue::from_static("beforeend"));
    r
}

pub fn toast(level: &str, text: &str) -> Response {
    toast_markup(views::toast(level, text))
}

async fn stats(st: &AppState) -> Option<GlobalStats> {
    st.call(method::STATS_GET, ()).await.ok()
}

/// A full page with chrome. `sse` = query for `/events` (`""` = stats only).
pub async fn full_page(
    st: &AppState,
    ctx: &Ctx,
    nav: Nav,
    title: &str,
    sse: &str,
    toasts: Vec<Markup>,
    content: Markup,
) -> Response {
    let s = stats(st).await;
    let l = Layout {
        base: &st.base,
        title: &st.title,
        csrf: &ctx.csrf,
        nav,
        exposed_warning: st.exposed && st.token.is_none(),
        show_logout: ctx.auth_enabled,
        stats: s.as_ref(),
        sse: sse.to_string(),
        toasts,
    };
    html(views::page(&l, title, content))
}

/// Error outcome of a request: a toast for htmx, an error page otherwise.
pub async fn fail(
    st: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    status: StatusCode,
    msg: &str,
) -> Response {
    if is_htmx(headers) {
        return toast("error", msg);
    }
    let mut r = full_page(
        st,
        ctx,
        Nav::None,
        "Error",
        "",
        Vec::new(),
        views::error_page(&st.base, msg),
    )
    .await;
    *r.status_mut() = status;
    r
}

pub fn status_for(e: &RpcError) -> StatusCode {
    match e.code {
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::Refused => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
    }
}

pub async fn fail_rpc(st: &AppState, ctx: &Ctx, headers: &HeaderMap, e: &RpcError) -> Response {
    fail(st, ctx, headers, status_for(e), &e.message).await
}

/// Toasts for the one-shot `done=` code a redirect carries.
pub fn flash(p: &Pairs) -> Vec<Markup> {
    let text = match p.str("done") {
        "removed" => "Removed (files kept on disk).",
        "removed-files" => "Removed and deleted its files.",
        "renamed" => "Renamed.",
        "saved" => "Options saved.",
        "limits" => "Settings saved.",
        "provider" => "Provider updated.",
        "paused" => "Paused.",
        "resumed" => "Resumed.",
        "moved" => "Queue updated.",
        "cleared" => "Cleared from the list. Files were not touched.",
        _ => return Vec::new(),
    };
    vec![views::toast("ok", text)]
}

pub async fn not_found(State(st): St, Extension(ctx): Extension<Ctx>) -> Response {
    let mut r = full_page(
        &st,
        &ctx,
        Nav::None,
        "Not found",
        "",
        Vec::new(),
        views::error_page(&st.base, "This page does not exist."),
    )
    .await;
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}
