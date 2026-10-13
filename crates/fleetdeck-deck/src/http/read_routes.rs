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

/// The stations that host a shared session, without what a member must not
/// see (the key, the dial address, the station info).
#[must_use]
pub fn member_stations(stations: Vec<StationView>, sessions: &[DeckSession]) -> Vec<StationView> {
    stations
        .into_iter()
        .filter(|st| sessions.iter().any(|s| s.station_id == st.station_id))
        .map(|mut st| {
            st.key = String::new();
            st.dial_url = None;
            st.info = None;
            st.info_at = None;
            st
        })
        .collect()
}

/// The cards that a member may see.
#[must_use]
pub fn member_sessions(
    grant: &crate::store::ShareGrant,
    sessions: Vec<DeckSession>,
) -> Vec<DeckSession> {
    sessions
        .into_iter()
        .filter(|s| {
            grant
                .scope
                .covers(s.station_id.as_str(), s.summary.session_id.as_str())
        })
        .collect()
}

/// The newest audit rows that `GET /api/audit` returns.
pub const AUDIT_PAGE: u32 = 200;

/// `GET /api/stations`.
pub async fn stations(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<StationView>>, ApiError> {
    let list = board.store.stations().await.map_err(ApiError::internal)?;
    let views: Vec<StationView> = list
        .iter()
        .map(|st| station_view(st, &board.store))
        .collect();
    Ok(Json(match &principal {
        Principal::Member(_, grant) => {
            let sessions = member_sessions(
                grant,
                board.store.sessions().await.map_err(ApiError::internal)?,
            );
            member_stations(views, &sessions)
        }
        _ => views,
    }))
}

/// `GET /api/sessions`: every card.
pub async fn sessions(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<DeckSession>>, ApiError> {
    let all = board.store.sessions().await.map_err(ApiError::internal)?;
    Ok(Json(match &principal {
        Principal::Member(_, grant) => member_sessions(grant, all),
        _ => all,
    }))
}

/// `GET /api/fleet/extras`: the pending mail and the recent files of the cards.
pub async fn fleet_extras(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<FleetExtras>, ApiError> {
    let mut extras = board
        .store
        .fleet_extras()
        .await
        .map_err(ApiError::internal)?;
    if let Principal::Member(_, grant) = &principal {
        // The touched files of a session can name a hidden file, and the
        // deck does not know the project's own deny list: a member gets none.
        extras.sessions.retain(|s| {
            grant
                .scope
                .covers(s.station_id.as_str(), s.session_id.as_str())
        });
        for s in &mut extras.sessions {
            s.files.clear();
        }
    }
    Ok(Json(extras))
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
