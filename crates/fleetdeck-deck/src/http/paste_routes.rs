//! `POST /api/stations/{station}/paste` (DL-147): the board stores a pasted
//! image on a station and gets its path. The deck forwards the body to the
//! station's `files.write_temp` method on a live link. Owner only; the
//! station decides what an image is and keeps the quota. The deck keeps no
//! copy of the data and never logs or audits it.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::link::{Outcome, PasteImage, PastedImage};
use serde_json::json;

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, NewAudit, Principal};

/// The longest the board waits for the station's answer.
pub const PASTE_WAIT: Duration = Duration::from_secs(15);
/// The largest request body of this route: 1 MiB.
pub const MAX_BODY: usize = 1_048_576;

/// Maps a station `files.write_temp` error to an HTTP error.
fn station_error(code: &str, message: String) -> ApiError {
    let (status, known) = match code {
        "invalid_params" => (StatusCode::BAD_REQUEST, "invalid_params"),
        "too_large" => (StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
        "conflict" => (StatusCode::CONFLICT, "conflict"),
        "no_space" => (StatusCode::INSUFFICIENT_STORAGE, "no_space"),
        _ => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{code}: {message}"),
            );
        }
    };
    ApiError::new(status, known, message)
}

async fn relay(board: &Board, name: &str, body: &PasteImage) -> Result<PastedImage, ApiError> {
    let rec = board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })?;
    let unreachable = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            format!("station `{}` has no live link to this deck", rec.name),
        )
    };
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(unreachable)?;
    let outcome = tokio::time::timeout(
        PASTE_WAIT,
        control.request("files.write_temp", None, json!(body)),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the station did not answer in time",
        )
    })?
    .map_err(|_| unreachable())?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(station_error(&e.code, e.message)),
    }
}

/// `POST /api/stations/{station}/paste { upload_id, ext, data_base64 }`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`; 409
/// `station_unreachable`, `conflict`; 413 `too_large`; 502 `station_error`
/// (also a station without `files.write_temp.v1`); 504 `timeout`; 507
/// `no_space`.
pub async fn paste(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    Json(body): Json<PasteImage>,
) -> Result<Json<PastedImage>, ApiError> {
    let result = match owner_only(&principal, "pastes images") {
        Ok(()) => relay(&board, &name, &body).await,
        Err(e) => Err(e),
    };
    let mut row = NewAudit::new(
        principal.actor(),
        "files.paste",
        if result.is_ok() {
            AuditResult::Ok
        } else {
            AuditResult::Refused
        },
    )
    .target(name);
    if let Err(e) = &result {
        row = row.detail(e.code);
    }
    board.store.audit_or_log(row).await;
    result.map(Json)
}
