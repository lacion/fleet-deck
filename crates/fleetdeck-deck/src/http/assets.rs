//! The board web client, served by the deck (PLAN §12 "Board assets:
//! rust-embed"; M2 P4-4, DL-061).
//!
//! A release build embeds `web/dist` (build the board first). A debug build
//! reads the same directory at run time (rust-embed's development mode). A
//! deck without `index.html` answers every board page with a plain 404 that
//! says how to build the board; the API works without it.
//!
//! The assets are public: they hold no data. Every answer has a strict
//! content security policy: the board loads only its own scripts and
//! styles and connects only to its own origin.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use super::guard::ApiError;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist/"]
#[allow_missing = true]
struct Embedded;

/// Where the deck finds the board files.
#[derive(Debug, Clone, Default)]
pub enum BoardAssets {
    /// The files built into the binary (or `web/dist` in a debug build).
    #[default]
    Embedded,
    /// A directory (tests).
    Dir(PathBuf),
    /// No board: every page is a 404.
    None,
}

/// The content security policy of every board answer.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
    img-src 'self' data:; font-src 'self'; connect-src 'self'; manifest-src 'self'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// The text of a 404 when the deck has no board files.
pub const NOT_BUILT: &str = "This deck has no board files. Build the board with \
    `bun run --cwd web build`, then build the deck again.";

impl BoardAssets {
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        match self {
            Self::Embedded => Embedded::get(path).map(|f| f.data),
            Self::Dir(dir) => {
                let rel = Path::new(path);
                if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
                    return None;
                }
                std::fs::read(dir.join(rel)).ok().map(Cow::Owned)
            }
            Self::None => None,
        }
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn with_headers(mut response: Response, cache: &'static str) -> Response {
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

fn file(path: &str, data: Cow<'static, [u8]>, cache: &'static str) -> Response {
    let response = (
        [(header::CONTENT_TYPE, content_type(path))],
        Body::from(data),
    )
        .into_response();
    with_headers(response, cache)
}

/// The router fallback: a board file, the board page for a client route,
/// or a 404. An unknown `/api/` path gets the API's JSON 404.
pub async fn serve(assets: &BoardAssets, method: &Method, uri: &Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such API method")
            .into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return with_headers(StatusCode::METHOD_NOT_ALLOWED.into_response(), "no-store");
    }
    let rel = path.trim_start_matches('/');
    if !rel.is_empty()
        && let Some(data) = assets.get(rel)
    {
        // Vite puts a content hash into every name under `assets/`.
        let cache = if rel.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return file(rel, data, cache);
    }
    // A path with a file extension is a missing file; any other path is a
    // page of the board.
    let last = rel.rsplit('/').next().unwrap_or_default();
    if last.contains('.') {
        return with_headers(StatusCode::NOT_FOUND.into_response(), "no-store");
    }
    match assets.get("index.html") {
        Some(data) => file("index.html", data, "no-cache"),
        None => with_headers(
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                NOT_BUILT,
            )
                .into_response(),
            "no-store",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_source_refuses_paths_that_leave_it() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("index.html"), "<p>board</p>").unwrap();
        std::fs::write(tmp.path().parent().unwrap().join("secret.txt"), "no").ok();
        let a = BoardAssets::Dir(tmp.path().to_path_buf());
        assert!(a.get("index.html").is_some());
        assert!(a.get("../secret.txt").is_none());
        assert!(a.get("/etc/passwd").is_none());
        assert!(a.get("./index.html").is_none());
        assert!(BoardAssets::None.get("index.html").is_none());
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/index-1.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("assets/a.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("x"), "application/octet-stream");
    }
}
