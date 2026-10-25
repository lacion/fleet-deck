//! The owner screenshot routes (R13 P5a task E, design
//! `docs/v2/design/omarchy-p5a.md` §2.2, §2.4):
//! - the `screen_capture` branch of `POST /api/stations/{station}/desktop/act`
//!   ([`act`], called by [`super::desktop_routes`]): owner, Admin, the header
//!   `X-FleetDeck-Capture-Id`; `200` with the result, `202 {"capture_id",
//!   "state":"pending"}` while the result is not indexed, or a capture error;
//! - `GET /api/stations/{station}/desktop/captures/{capture_id}` (owner,
//!   Admin): the [`CaptureStatus`] of the capture.
//!
//! The work, the audit row and the fixed texts are in
//! [`crate::desktop_capture`].

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{CAPTURE_ID_HEADER, CaptureStatus, valid_capture_id};
use fleetdeck_proto::link::Scope;

use super::Board;
use super::desktop_recording_routes as recording_routes;
use super::desktop_routes::{ctx, refused, relay_error, station_id};
use super::guard::{ApiError, owner_only};
use crate::desktop_capture::{self, CaptureError, CaptureOptions, Reply};
use crate::desktop_relay::RelayError;
use crate::store::Principal;
use crate::store::desktop_recordings::OperationKind;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route(
        "/api/stations/{station}/desktop/captures/{capture_id}",
        get(lookup),
    )
}

fn capture_error(e: CaptureError) -> ApiError {
    let (status, code, text) = e.http();
    ApiError::new(status, code, text)
}

/// The capture id of the request header: missing is `capture_id_required`;
/// not a canonical ULID, or sent twice, is `invalid_capture_id` (§2.2).
fn header_id(headers: &HeaderMap) -> Result<String, ApiError> {
    header_id_or(headers, "A screenshot needs a capture ID.")
}

/// [`header_id`] with the text of a missing header: a recording has its own
/// (`omarchy-p5b.md` §16).
pub(super) fn header_id_or(headers: &HeaderMap, missing: &'static str) -> Result<String, ApiError> {
    let mut values = headers.get_all(CAPTURE_ID_HEADER).iter();
    let Some(first) = values.next() else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "capture_id_required",
            missing,
        ));
    };
    first
        .to_str()
        .ok()
        .filter(|id| values.next().is_none() && valid_capture_id(id))
        .map(str::to_owned)
        .ok_or_else(invalid_id)
}

pub(super) fn invalid_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_capture_id",
        "The capture ID is not valid.",
    )
}

/// The `screen_capture` branch of `POST .../desktop/act`. The owner check
/// and the body parse are done.
///
/// # Errors
/// 400 `capture_id_required`, `invalid_capture_id`; 403 `desktop_forbidden`;
/// the codes of [`CaptureError`].
pub(super) async fn act(
    board: &Arc<Board>,
    principal: &Principal,
    station: StationId,
    headers: &HeaderMap,
    monitor: String,
) -> Result<Response, ApiError> {
    if !principal.has(Scope::Admin) {
        let e = relay_error(RelayError::Forbidden);
        return Err(refused(board, principal, station.as_str(), e).await);
    }
    let capture_id = match header_id(headers) {
        Ok(id) => id,
        Err(e) => return Err(refused(board, principal, station.as_str(), e).await),
    };
    match desktop_capture::act(
        &ctx(board),
        station,
        capture_id,
        monitor,
        CaptureOptions::default(),
    )
    .await
    {
        Ok(Reply::Done(result)) => Ok(Json(*result).into_response()),
        Ok(Reply::State(status, body)) => Ok((status, Json(body)).into_response()),
        Err(e) => Err(capture_error(e)),
    }
}

/// `GET /api/stations/{station}/desktop/captures/{capture_id}`.
///
/// # Errors
/// 400 `invalid_capture_id`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_capture`.
async fn lookup(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, capture_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    owner_only(&principal, "reads a screenshot result")?;
    if !principal.has(Scope::Admin) {
        return Err(relay_error(RelayError::Forbidden));
    }
    let id = station_id(&board, &station).await?;
    if !valid_capture_id(&capture_id) {
        return Err(invalid_id());
    }
    // A recording answers its own status (R13 P5b-1).
    let recording = board
        .store
        .recording_get(id.clone(), capture_id.clone())
        .await
        .map_err(ApiError::internal)?
        .filter(|r| r.kind == OperationKind::Recording);
    if let Some(row) = recording {
        let mut status = recording_routes::status(&board, &id, row).await?;
        recording_routes::project(&headers, &mut status);
        return Ok(Json(status).into_response());
    }
    match desktop_capture::lookup(&ctx(&board), &id, &capture_id, CaptureOptions::default())
        .await
        .map_err(ApiError::internal)?
    {
        Some(status) => Ok(Json::<CaptureStatus>(status).into_response()),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_capture",
            "No such capture.",
        )),
    }
}
