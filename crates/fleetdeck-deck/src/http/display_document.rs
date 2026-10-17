//! The document of a wall display (R13 §5E item 3, §5F item 3).
//!
//! The deck serves the board `index.html` at exactly `/display` (no trailing
//! slash), so the relative `./assets/...` URLs of the bundle resolve to
//! `/assets/...`. The response has a stricter content security policy: its
//! `frame-src` allows only `/api/displays/self/artifact/` of the origin that
//! the browser used. `/display/` answers `308` to `/display`. Every display
//! link, pair page and kiosk URL is `/display#/...`.
//!
//! The route is outside the guard, like the board files: the document holds
//! no data.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::HOST;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::Board;
use super::assets::display_csp;

/// The path of the document.
pub const PATH: &str = "/display";

/// `GET /display`.
pub async fn document(State(board): State<Arc<Board>>, headers: HeaderMap) -> Response {
    // The origin that the browser used, when it is an origin of this board.
    let origin = headers
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| format!("https://{}", h.to_ascii_lowercase()))
        .filter(|o| board.origins.allows(o));
    board.assets.index_with_csp(&display_csp(origin.as_deref()))
}

/// `GET /display/`: `308` to `/display`.
pub async fn redirect() -> Response {
    let mut res = StatusCode::PERMANENT_REDIRECT.into_response();
    res.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static(PATH));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}
