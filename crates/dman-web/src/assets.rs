//! Embedded static files (stylesheet, htmx, htmx SSE extension).

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

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
        _ => "application/octet-stream",
    };
    let mut resp = file.data.into_owned().into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    resp
}
