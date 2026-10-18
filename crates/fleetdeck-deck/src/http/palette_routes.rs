//! The palette routes (R13 P4 task F1, design `omarchy-p4.md` §3.7):
//! - `GET /api/stations/{station}/palette` (owner login, scope Read) answers
//!   the palette of one station. `{station}` is a station id; a name is a
//!   miss.
//! - `GET /api/displays/self/palette` (a display login only) answers the
//!   palette of the station that the display is bound to.
//!
//! Every refusal of both routes is the same `404 no_palette`, so a caller
//! cannot tell a missing station from a missing palette. The display route
//! follows the display rules of R13 (five steps, see
//! [`palette_for_display`]); it answers `Cache-Control: no-store`.

use std::future::Future;
use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use fleetdeck_proto::StationId;

use super::Board;
use super::display_view::scope_of;
use super::guard::{ApiError, owner_only};
use crate::store::station_palettes::StoredPalette;
use crate::store::{DisplayLogin, Principal};

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route("/api/stations/{station}/palette", get(owner_palette))
        .route("/api/displays/self/palette", get(display_palette))
}

/// The one answer for every refusal and every miss.
fn no_palette() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_palette",
        "No palette for this station.",
    )
}

/// `GET /api/stations/{station}/palette`.
///
/// # Errors
/// 403 `owner_only`; 404 `no_palette`.
async fn owner_palette(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
) -> Result<Json<StoredPalette>, ApiError> {
    owner_only(&principal, "reads the palette of a station")?;
    board
        .store
        .station_palette(StationId::new(station))
        .await
        .map_err(ApiError::internal)?
        .map(Json)
        .ok_or_else(no_palette)
}

/// The station that the display is bound to now, or `None` for an unbound or
/// revoked display.
async fn bound_station(board: &Board, login: &DisplayLogin) -> Result<Option<StationId>, ApiError> {
    Ok(board
        .store
        .display_get(login.display_id.clone())
        .await
        .map_err(ApiError::internal)?
        .and_then(|d| d.binding)
        .map(|b| b.station_id))
}

/// The palette that the display may read, by the five steps of §3.7:
/// 1. `scope_of` first: the live display session (an ended login is `401
///    login_required`);
/// 2. the bound station must be in the current effective scope;
/// 3. read the palette of that station;
/// 4. read the binding and the scope again: the same station must still be
///    bound and still in scope;
/// 5. answer the palette; every other case is `Ok(None)` (the caller answers
///    the one `404 no_palette`).
///
/// `between` runs after step 3 and before step 4. The route passes a future
/// that does nothing; a test passes one that changes the binding, the
/// ceiling or the login to prove that step 4 reads them again.
///
/// # Errors
/// 401 `login_required` from [`scope_of`]; a database error.
pub async fn palette_for_display<F: Future<Output = ()>>(
    board: &Arc<Board>,
    login: &DisplayLogin,
    between: F,
) -> Result<Option<StoredPalette>, ApiError> {
    let scope = scope_of(board, login).await?;
    let Some(station) = bound_station(board, login).await? else {
        return Ok(None);
    };
    if !scope.stations.contains(&station) {
        return Ok(None);
    }
    let read = board
        .store
        .station_palette(station.clone())
        .await
        .map_err(ApiError::internal)?;
    between.await;
    let scope = scope_of(board, login).await?;
    if bound_station(board, login).await?.as_ref() != Some(&station)
        || !scope.stations.contains(&station)
    {
        return Ok(None);
    }
    Ok(read)
}

/// `GET /api/displays/self/palette`.
///
/// # Errors
/// 401 `login_required`; 403 `display_only`; 404 `no_palette`.
async fn display_palette(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Response, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    let palette = palette_for_display(&board, &login, async {})
        .await?
        .ok_or_else(no_palette)?;
    let mut response = Json(palette).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
