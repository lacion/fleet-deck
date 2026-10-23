//! The document of a wall display (R13 §5E item 3, §5F item 3).
//!
//! The deck serves the board `index.html` at exactly `/display` (no trailing
//! slash), so the relative `./assets/...` URLs of the bundle resolve to
//! `/assets/...`. The response has a stricter content security policy: its
//! `frame-src` allows only `/api/displays/self/artifact/` of the origin that
//! the browser used. `/display/` answers `308` to `/display`. Every display
//! link, pair page and kiosk URL is `/display#/...`.
//!
//! The route is outside the guard, like the board files. The answer to an
//! anonymous request holds no data: it is the same for every caller. A
//! request with the valid `fd_display` login of a display that shows a wall
//! page now adds the origin of that one page to `frame-src` (B17 §3.2.6); the
//! owner's login, a member's login and the login of another display never
//! get it.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::HOST;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::Board;
use super::assets::display_csp;
use super::guard::{self, DISPLAY_COOKIE};
use crate::coordinator_desktop::effective_directive;
use fleetdeck_proto::display_directive::DirectiveBody;

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
    let page_origin = page_origin(&board, &headers).await;
    // A document with a page origin is made for one display: no cache.
    let cache = if page_origin.is_some() {
        "no-store"
    } else {
        "no-cache"
    };
    board.assets.index_with_csp(
        &display_csp(origin.as_deref(), page_origin.as_deref()),
        cache,
    )
}

/// The origin of the wall page that the display of the request shows now.
/// `None` for every miss or error: no cookie, an unknown or revoked login,
/// another mode, a deleted entry, an origin that fails the grammar, a
/// database error. Only the `fd_display` cookie plays a part.
async fn page_origin(board: &Board, headers: &HeaderMap) -> Option<String> {
    let value = guard::cookie(headers, DISPLAY_COOKIE)?;
    let login = match board.store.display_session(&value).await {
        Ok(found) => found?,
        Err(e) => {
            tracing::warn!("the display document could not read the display login: {e}");
            return None;
        }
    };
    let view = match effective_directive(&board.link, &login.display_id).await {
        Ok(found) => found?,
        Err(e) => {
            tracing::warn!("the display document could not read the directive: {e}");
            return None;
        }
    };
    let DirectiveBody::Page { page, .. } = view.directive else {
        return None;
    };
    match board.store.wall_page(page.page_id).await {
        Ok(entry) => entry.map(|e| e.origin).filter(|o| origin_ok(o)),
        Err(e) => {
            tracing::warn!("the display document could not read the wall page: {e}");
            None
        }
    }
}

/// The grammar of an origin that may enter the header:
/// `^https?://([a-z0-9.-]+|\[[0-9a-f:]+\])(:[0-9]{1,5})?$`.
fn origin_ok(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return false;
    };
    let port = if rest.starts_with('[') {
        let Some(end) = rest.find(']') else {
            return false;
        };
        let inner = &rest[1..end];
        if inner.is_empty()
            || !inner
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b':'))
        {
            return false;
        }
        &rest[end + 1..]
    } else {
        let end = rest.find(':').unwrap_or(rest.len());
        let host = &rest[..end];
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        {
            return false;
        }
        &rest[end..]
    };
    match port.strip_prefix(':') {
        None => port.is_empty(),
        Some(digits) => {
            (1..=5).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        }
    }
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
