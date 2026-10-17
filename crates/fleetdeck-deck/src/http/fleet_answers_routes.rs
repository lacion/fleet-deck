//! What a fleet-control coordinator answered (ADR-008 §5.3 "Owner
//! visibility"): `GET /api/sessions/{station}/{session}/fleet-answers`. Only
//! the owner's board login reads it. A read writes no audit row.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::SessionId;

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::Principal;
use crate::store::grants::FleetAnswers;

/// `GET /api/sessions/{station}/{session}/fleet-answers`.
///
/// # Errors
/// 403 `owner_only`, 404 `unknown_station`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<FleetAnswers>, ApiError> {
    owner_only(&principal, "reads the answers of a coordinator")?;
    let found = board
        .store
        .find_station(station.clone())
        .await
        .map_err(ApiError::internal)?;
    let Some(rec) = found else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_station",
            format!("no station is named `{station}`"),
        ));
    };
    board
        .store
        .fleet_answers(rec.station_id, SessionId(session))
        .await
        .map(Json)
        .map_err(ApiError::internal)
}
