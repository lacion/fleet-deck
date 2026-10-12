//! `GET /api/stations/{station}/worktrees` (DL-142): the read-only
//! inventory of the worktrees that the station created for sessions. The
//! deck forwards the call to the station's `workspace.worktrees` method on
//! a live link and returns the answer; it keeps no copy. The paths and the
//! branch names are the owner's: the route needs the `admin` scope and the
//! owner's board login (DL-138).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::link::{Outcome, WorktreeList, WorktreesQuery};
use serde_json::json;

use super::Board;
use super::guard::ApiError;
use crate::store::Principal;

/// The longest the board waits for the station's answer. The station stops
/// its own inventory after 45 s.
pub const WORKTREES_WAIT: Duration = Duration::from_secs(60);

/// `GET /api/stations/{station}/worktrees`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 502
/// `station_error` (also a station without `workspace.worktrees.v1`); 504
/// `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
) -> Result<Json<WorktreeList>, ApiError> {
    super::guard::owner_only(&principal, "lists the worktrees of a station")?;
    let rec = board
        .store
        .find_station(station.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
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
        WORKTREES_WAIT,
        control.request("workspace.worktrees", None, json!(WorktreesQuery {})),
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
            .map(Json)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(match e.code.as_str() {
            "timeout" => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", e.message),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{}: {}", e.code, e.message),
            ),
        }),
    }
}
