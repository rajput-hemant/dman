//! Authentication (access token, session cookie, bearer), CSRF protection and
//! security headers. One middleware ([`guard`]) covers every route.
//!
//! * Cookie: `dman_session=<32 random bytes, base64url>`, `HttpOnly; SameSite=Strict`,
//!   `Secure` when the request came in over https, 7 days. Sessions live in memory.
//! * CSRF, two independent layers on every state-changing request: the `Origin`
//!   (or `Referer`, or `Sec-Fetch-Site`) must not point at another site, and a
//!   per-session secret must come back in `X-CSRF-Token` (htmx adds it through
//!   `hx-headers`) or the `_csrf` form field. Bearer-authenticated requests need
//!   no CSRF token: browsers never attach that header on their own.
//! * The token is compared in constant time and never logged.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use secrecy::ExposeSecret;
use subtle::ConstantTimeEq;

use crate::pages::{Pairs, html};
use crate::{AppState, views};

pub const COOKIE: &str = "dman_session";
const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_SESSIONS: usize = 1000;
const CSP: &str = "default-src 'self'; img-src 'self' data:; object-src 'none'; \
                   base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub struct Session {
    pub expires: Instant,
    pub csrf: String,
}

/// Per-request auth outcome, handed to handlers as an extension.
#[derive(Clone)]
pub struct Ctx {
    /// CSRF token to embed in pages.
    pub csrf: String,
    /// An access token is configured (shows the log-out button).
    pub auth_enabled: bool,
}

/// 32 random bytes, base64url.
pub fn random_id() -> String {
    let mut b = [0u8; 32];
    // A failing system RNG leaves no safe fallback for a credential.
    getrandom::fill(&mut b).expect("system random number generator unavailable");
    URL_SAFE_NO_PAD.encode(b)
}

fn eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn header_str(h: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn bearer(h: &HeaderMap) -> Option<&str> {
    let v = header_str(h, header::AUTHORIZATION)?;
    let (scheme, rest) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(rest.trim())
}

fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    header_str(h, header::COOKIE)?.split(';').find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

fn is_https(h: &HeaderMap) -> bool {
    header_str(h, "x-forwarded-proto").is_some_and(|p| p.eq_ignore_ascii_case("https"))
        || header_str(h, header::ORIGIN).is_some_and(|o| o.starts_with("https://"))
}

fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    Some(rest.split(['/', '?', '#']).next().unwrap_or(rest))
}

/// Cross-site request check for state-changing methods.
fn origin_ok(h: &HeaderMap) -> bool {
    if header_str(h, "sec-fetch-site").is_some_and(|s| s == "cross-site") {
        return false;
    }
    let src = header_str(h, header::ORIGIN).or_else(|| header_str(h, header::REFERER));
    let Some(src) = src else {
        return true; // not a browser form/fetch; the CSRF token still applies
    };
    let Some(a) = authority(src) else {
        return false; // "null" and garbage
    };
    [header::HOST.as_str(), "x-forwarded-host"]
        .into_iter()
        .filter_map(|n| header_str(h, n))
        .any(|host| host.eq_ignore_ascii_case(a))
}

fn plain(status: StatusCode, msg: &'static str) -> Response {
    (status, msg).into_response()
}

fn unauthorized(st: &AppState, path: &str, method: &Method, h: &HeaderMap) -> Response {
    let login = format!("{}/login", st.base);
    let ajax = h.contains_key("hx-request") || path == "/events" || method != Method::GET;
    if ajax {
        let mut r = plain(StatusCode::UNAUTHORIZED, "login required");
        if let Ok(v) = HeaderValue::from_str(&login) {
            r.headers_mut().insert("hx-redirect", v);
        }
        r
    } else {
        let mut r = StatusCode::SEE_OTHER.into_response();
        if let Ok(v) = HeaderValue::from_str(&login) {
            r.headers_mut().insert(header::LOCATION, v);
        }
        r
    }
}

fn add_security_headers(resp: &mut Response, no_store: bool) {
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    if no_store && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
}

pub async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let full = req.uri().path().to_string();
    let path = full
        .strip_prefix(st.base.as_str())
        .unwrap_or(&full)
        .to_string();
    let is_static = path.starts_with("/static/");
    let mut resp = guarded(&st, req, next, &path).await;
    add_security_headers(&mut resp, !is_static);
    resp
}

async fn guarded(st: &Arc<AppState>, mut req: Request, next: Next, path: &str) -> Response {
    let method = req.method().clone();
    let public = path == "/login" || path.starts_with("/static/");
    let changing = !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if changing && !origin_ok(req.headers()) {
        return plain(StatusCode::FORBIDDEN, "cross-origin request refused");
    }

    let mut ctx = Ctx {
        csrf: st.open_csrf.clone(),
        auth_enabled: st.token.is_some(),
    };
    let mut bearer_ok = false;
    if let Some(token) = &st.token {
        let h = req.headers();
        let mut ok = false;
        if let Some(b) = bearer(h)
            && eq(b, token.expose_secret())
        {
            ok = true;
            bearer_ok = true;
        }
        if !ok
            && let Some(id) = cookie(h, COOKIE)
            && let Ok(mut s) = st.sessions.lock()
        {
            match s.get(id) {
                Some(sess) if sess.expires > Instant::now() => {
                    ctx.csrf.clone_from(&sess.csrf);
                    ok = true;
                }
                Some(_) => {
                    s.remove(id);
                }
                None => {}
            }
        }
        if !ok && !public {
            return unauthorized(st, path, &method, req.headers());
        }
    }

    if changing && !public && !bearer_ok {
        let from_header = header_str(req.headers(), "x-csrf-token").map(str::to_owned);
        let urlencoded = header_str(req.headers(), header::CONTENT_TYPE)
            .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
        let provided = match from_header {
            Some(t) => Some(t),
            None if urlencoded => {
                let (parts, body) = req.into_parts();
                let Ok(bytes) = axum::body::to_bytes(body, 1024 * 1024).await else {
                    return plain(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
                };
                let t = url::form_urlencoded::parse(&bytes)
                    .find(|(k, _)| k == "_csrf")
                    .map(|(_, v)| v.into_owned());
                req = Request::from_parts(parts, Body::from(bytes));
                t
            }
            None => None,
        };
        if !provided.is_some_and(|p| eq(&p, &ctx.csrf)) {
            return plain(StatusCode::FORBIDDEN, "missing or invalid CSRF token");
        }
    }

    req.extensions_mut().insert(ctx);
    next.run(req).await
}

fn set_cookie(st: &AppState, h: &HeaderMap, value: &str, max_age: u64) -> HeaderValue {
    let path = if st.base.is_empty() { "/" } else { &st.base };
    let secure = if is_https(h) { "; Secure" } else { "" };
    let c = format!(
        "{COOKIE}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}"
    );
    HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn redirect(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

pub async fn login_form(State(st): State<Arc<AppState>>) -> Response {
    html(views::login_page(&st.base, &st.title, None))
}

pub async fn login(State(st): State<Arc<AppState>>, headers: HeaderMap, pairs: Pairs) -> Response {
    let Some(token) = &st.token else {
        return redirect(&format!("{}/", st.base));
    };
    let given = pairs.get("token").unwrap_or("");
    if !eq(given, token.expose_secret()) {
        // Slow down guessing.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let mut r = html(views::login_page(&st.base, &st.title, Some("Wrong token.")));
        *r.status_mut() = StatusCode::UNAUTHORIZED;
        return r;
    }
    let id = random_id();
    if let Ok(mut s) = st.sessions.lock() {
        let now = Instant::now();
        s.retain(|_, v| v.expires > now);
        if s.len() >= MAX_SESSIONS
            && let Some(oldest) = s
                .iter()
                .min_by_key(|(_, v)| v.expires)
                .map(|(k, _)| k.clone())
        {
            s.remove(&oldest);
        }
        s.insert(
            id.clone(),
            Session {
                expires: now + SESSION_TTL,
                csrf: random_id(),
            },
        );
    }
    let mut r = redirect(&format!("{}/", st.base));
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&st, &headers, &id, SESSION_TTL.as_secs()),
    );
    r
}

pub async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, COOKIE)
        && let Ok(mut s) = st.sessions.lock()
    {
        s.remove(id);
    }
    let mut r = redirect(&format!("{}/login", st.base));
    r.headers_mut()
        .append(header::SET_COOKIE, set_cookie(&st, &headers, "", 0));
    r
}
