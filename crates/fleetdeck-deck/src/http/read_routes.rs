//! The read methods of the board API that P3-8 needs for its tests and
//! that the board uses: stations, cards, commands, the audit log. P3-9
//! adds the mutating methods and the event stream.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, State};
use fleetdeck_proto::board::FleetExtras;

use super::Board;
use super::guard::ApiError;
use crate::local::{StationView, station_view};
use crate::store::{AuditEntry, DeckCommand, DeckSession, Principal};

/// The newest audit rows that `GET /api/audit` returns.
pub const AUDIT_PAGE: u32 = 200;

/// `GET /api/stations`.
pub async fn stations(State(board): State<Arc<Board>>) -> Result<Json<Vec<StationView>>, ApiError> {
    let list = board.store.stations().await.map_err(ApiError::internal)?;
    Ok(Json(
        list.iter()
            .map(|st| station_view(st, &board.store))
            .collect(),
    ))
}

/// `GET /api/sessions`: every card, with the unread count of the caller.
pub async fn sessions(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<DeckSession>>, ApiError> {
    Ok(Json(
        board
            .store
            .sessions_for(&principal)
            .await
            .map_err(ApiError::internal)?,
    ))
}

/// `GET /api/fleet/extras`: the pending mail and the recent files of the cards.
pub async fn fleet_extras(State(board): State<Arc<Board>>) -> Result<Json<FleetExtras>, ApiError> {
    Ok(Json(
        board
            .store
            .fleet_extras()
            .await
            .map_err(ApiError::internal)?,
    ))
}

/// `GET /api/commands`.
pub async fn commands(State(board): State<Arc<Board>>) -> Result<Json<Vec<DeckCommand>>, ApiError> {
    Ok(Json(
        board.store.commands().await.map_err(ApiError::internal)?,
    ))
}

/// `GET /api/audit` (admin).
pub async fn audit(State(board): State<Arc<Board>>) -> Result<Json<Vec<AuditEntry>>, ApiError> {
    Ok(Json(
        board
            .store
            .audit_log(AUDIT_PAGE)
            .await
            .map_err(ApiError::internal)?,
    ))
}
