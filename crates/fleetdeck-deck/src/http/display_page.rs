//! The display page route (B17 §3.2.4): `GET /api/displays/self/page/{page_id}`.
//!
//! The route authenticates with `fd_display` only (the guard) and answers the
//! [`DisplayPage`] only when the effective directive of the display is mode
//! `page` with exactly this id and the entry exists. The directive is read
//! before and after the entry, and the two reads must be equal (the pattern of
//! the display artifact route). Every miss is the same `404 unknown_page`, so
//! there is no existence oracle. The answer is `no-store`.

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use fleetdeck_proto::display_directive::DirectiveBody;
use fleetdeck_proto::wall_pages::DisplayPage;

use super::Board;
use super::guard::ApiError;
use crate::coordinator_desktop::effective_directive;
use crate::store::{DisplayLogin, Principal};

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route("/api/displays/self/page/{page_id}", get(page))
}

/// The one answer for every miss.
fn miss() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_page", "No such page.")
}

/// The effective directive of this moment, or the login error of a display
/// that ended.
async fn directive_of(
    board: &Arc<Board>,
    login: &DisplayLogin,
) -> Result<fleetdeck_proto::display_directive::DirectiveView, ApiError> {
    effective_directive(&board.link, &login.display_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "login_required",
                "the display login ended",
            )
        })
}

/// `GET /api/displays/self/page/{page_id}`.
///
/// # Errors
/// 401 `login_required`; 403 `display_only`; 404 `unknown_page`.
async fn page(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(page_id): Path<String>,
) -> Result<Response, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    let before = directive_of(&board, &login).await?;
    match &before.directive {
        DirectiveBody::Page { page, .. } if page.page_id == page_id => {}
        _ => return Err(miss()),
    }
    let entry = board
        .store
        .wall_page(page_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(miss)?;
    if directive_of(&board, &login).await? != before {
        return Err(miss());
    }
    let mut response = Json(DisplayPage {
        page_id: entry.page_id,
        label: entry.label,
        url: entry.url,
        fit: entry.fit,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
