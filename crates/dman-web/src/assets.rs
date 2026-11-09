//! Embedded static files (stylesheet, scripts, icon sprite, htmx and its SSE extension).

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// URL of an embedded file with a content fingerprint (`?v=…`), so browsers
/// fetch a new copy whenever dman is upgraded instead of reusing a cached one.
pub fn url(base: &str, name: &str) -> String {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static VERSIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
    let versions = VERSIONS.get_or_init(|| {
        Assets::iter()
            .filter_map(|n| {
                let f = Assets::get(&n)?;
                let hash: String = f.metadata.sha256_hash()[..5]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                Some((n.to_string(), hash))
            })
            .collect()
    });
    match versions.get(name) {
        Some(v) => format!("{base}/static/{name}?v={v}"),
        None => format!("{base}/static/{name}"),
    }
}

pub fn total_bytes() -> usize {
    Assets::iter()
        .filter_map(|n| Assets::get(&n))
        .map(|f| f.data.len())
        .sum()
}

#[cfg(test)]
pub fn listing() -> Vec<(String, usize)> {
    Assets::iter()
        .filter_map(|n| Assets::get(&n).map(|f| (n.to_string(), f.data.len())))
        .collect()
}

pub async fn serve(Path(path): Path<String>) -> Response {
    let Some(file) = Assets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ctype = match path.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    let mut resp = file.data.into_owned().into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(
        header::CACHE_CONTROL,
        // URLs carry a content fingerprint (see `url`), so a cached copy can
        // never be stale.
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    resp
}
