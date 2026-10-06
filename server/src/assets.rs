//! Static asset serving via rust-embed.
//!
//! In debug builds, assets are read from disk so UI edits show up on reload.
//! In release builds, everything is embedded into the binary.
//!
//! If `server.web_dir` is set in the config, that directory is served instead
//! of the embedded copy (useful for development).

use axum::{
    body::Body,
    http::{header, Response, StatusCode},
    response::IntoResponse,
};
use mime_guess::from_path;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../web"]
struct WebAssets;

// ── Route handlers ────────────────────────────────────────────────────────────

pub async fn serve_index() -> impl IntoResponse {
    serve_embedded("index.html")
}

pub async fn serve_sw() -> impl IntoResponse {
    serve_embedded_with_cache("sw.js", "no-cache, no-store")
}

pub async fn serve_static(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    serve_embedded(&path)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn serve_embedded(path: &str) -> Response<Body> {
    match WebAssets::get(path) {
        Some(content) => {
            let mime = from_path(path).first_or_octet_stream();
            let cache = if path == "index.html" || path == "sw.js" {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
    }
}

fn serve_embedded_with_cache(path: &str, cache: &str) -> Response<Body> {
    match WebAssets::get(path) {
        Some(content) => {
            let mime = from_path(path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
    }
}

/// Returns the login HTML. Embedded at compile time.
pub fn login_html() -> String {
    WebAssets::get("login.html")
        .map(|c| String::from_utf8_lossy(&c.data).into_owned())
        .unwrap_or_else(|| include_str!("fallback_login.html").to_string())
}
