//! The owner's coordinator grant controls (M3 6-5, ADR-006 §4, §7):
//! `GET /api/coordinator/grants` lists the grants without any credential;
//! `POST /api/coordinator/grants/{grant_id}/revoke` commits `revoked` on the
//! deck first, so every later decision for the grant is `forbidden`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::orchestration::GrantView;

use super::Board;
use super::fleet_routes::{LIST_PAGE, audited};
use super::guard::ApiError;
use crate::store::{Grant, GrantClass, GrantState, Principal};

fn view(g: Grant) -> GrantView {
    let class = |c: &GrantClass| {
        match c {
            GrantClass::Read => "read",
            GrantClass::Mail => "mail",
            GrantClass::Drive => "drive",
            GrantClass::Spawn => "spawn",
        }
        .to_owned()
    };
    GrantView {
        classes: g.classes.iter().map(class).collect(),
        stations: g.stations.into_iter().collect(),
        state: match g.state {
            GrantState::Active => "active",
            GrantState::Revoked => "revoked",
            GrantState::Expired => "expired",
        }
        .to_owned(),
        generation: g.binding.map(|b| b.generation),
        grant_id: g.grant_id,
        station_id: g.station_id,
        session_id: g.session_id,
        spawn_cap: g.spawn_cap,
        spawn_used: g.spawn_used,
        revoked_reason: g.revoked_reason,
        expires_at: g.expires_at,
        last_used_at: g.last_used_at,
    }
}

/// `GET /api/coordinator/grants`: the newest grants.
///
/// # Errors
/// A store error.
pub async fn list(State(board): State<Arc<Board>>) -> Result<Json<Vec<GrantView>>, ApiError> {
    let grants = board
        .store
        .grants(LIST_PAGE)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(grants.into_iter().map(view).collect()))
}

/// `POST /api/coordinator/grants/{grant_id}/revoke`. A revoked grant stays
/// revoked; a second revoke returns it unchanged.
///
/// # Errors
/// 404 `unknown_grant`.
pub async fn revoke(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(grant_id): Path<String>,
) -> Result<Json<GrantView>, ApiError> {
    let result = async {
        let before = board
            .store
            .grant(grant_id.clone())
            .await
            .map_err(ApiError::internal)?;
        board
            .store
            .revoke_grant(grant_id.clone(), "owner".to_owned())
            .await
            .map_err(ApiError::internal)?;
        // The deck refuses at once; the station learns it through
        // `session.coordinator_revoke` when its link is up (ADR-006 §7,
        // DL-212). A second revoke sends nothing.
        if let Some(g) = before.filter(|g| g.state != GrantState::Revoked) {
            let command = fleetdeck_proto::SessionCommand::CoordinatorRevoke {
                session_id: g.session_id.clone(),
                grant_id: g.grant_id.clone(),
            };
            board
                .store
                .enqueue(crate::store::NewOutboxItem {
                    station_id: g.station_id.clone(),
                    origin: crate::link::router::origin_name(
                        fleetdeck_proto::command::CommandOrigin::Board,
                    )
                    .to_owned(),
                    payload: serde_json::to_value(crate::link::CommandKind::Session { command })
                        .map_err(ApiError::internal)?,
                    expires_at: board
                        .store
                        .now_ms()
                        .saturating_add(super::fleet_routes::DELIVERY_TTL_MS),
                })
                .await
                .map_err(ApiError::internal)?;
        }
        board
            .store
            .grant(grant_id.clone())
            .await
            .map_err(ApiError::internal)?
            .map(view)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_grant",
                    format!("no grant `{grant_id}`"),
                )
            })
    }
    .await;
    audited(
        &board,
        &principal,
        "coordinator.revoke",
        Some(&grant_id),
        result,
    )
    .await
    .map(Json)
}
