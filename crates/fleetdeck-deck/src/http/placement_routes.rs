//! `POST /api/placement` (FG-6, design §9.3, decision D3): the stations for
//! `count` new sessions, by the requirements and preferences of the board.
//! Scope `drive`, not mutating, not audited, closed to members. The board
//! then sends the starts as today.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::board::StationView;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{GitHomeState, Prefer};
use fleetdeck_proto::ids::ProviderInstanceId;
use fleetdeck_proto::integrations::{Project, check_placement};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::guard::ApiError;
use super::projects_routes::bad_body;
use crate::local::station_view;
use crate::routing_place::{PlaceError, PlaceNeeds, place};
use crate::store::Principal;

/// The body of `POST /api/placement`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceBody {
    pub count: u32,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub prefer: Vec<Prefer>,
    #[serde(default)]
    pub spread: bool,
    #[serde(default)]
    pub provider: Option<ProviderKind>,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// The 409 `no_station`.
pub(super) fn no_station(e: &PlaceError) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "no_station", e.text())
}

/// The stations of the deck as the read model shows them.
pub(super) async fn views(board: &Board) -> Result<Vec<StationView>, ApiError> {
    let records = board.store.stations().await.map_err(ApiError::internal)?;
    Ok(records
        .iter()
        .map(|st| station_view(st, &board.store))
        .collect())
}

/// The stations with a `ready` home of the project (homes count only when
/// ready, decision D3).
pub(super) fn ready_homes(project: &Project) -> Vec<StationId> {
    project
        .homes
        .iter()
        .filter(|h| h.state == GitHomeState::Ready)
        .map(|h| h.station_id.clone())
        .collect()
}

/// Keeps the stations that have the provider instance signed in (decision
/// D5).
pub(super) fn with_instance(views: &mut Vec<StationView>, instance: &ProviderInstanceId) {
    views.retain(|st| {
        st.info.as_ref().is_some_and(|i| {
            i.providers
                .iter()
                .any(|p| p.instance == *instance && p.auth == "signed_in")
        })
    });
}

/// `POST /api/placement`.
///
/// # Errors
/// 400 `invalid_body`; 404 `unknown_project`; 409 `no_station`.
pub async fn placement(
    State(board): State<Arc<Board>>,
    Extension(_principal): Extension<Principal>,
    body: Result<Json<PlaceBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(req) = body.map_err(|e| bad_body(&e))?;
    let invalid = |text: &str| ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", text);
    if !(1..=20).contains(&req.count) {
        return Err(invalid("Count is 1 to 20."));
    }
    check_placement(&req.requires, &req.prefer).map_err(invalid)?;
    let homes = match &req.project_id {
        Some(id) => {
            let project = board
                .store
                .project(id.clone())
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| {
                    ApiError::new(StatusCode::NOT_FOUND, "unknown_project", "Unknown project.")
                })?;
            Some(ready_homes(&project))
        }
        None => None,
    };
    let views = views(&board).await?;
    let sessions = board.store.sessions().await.map_err(ApiError::internal)?;
    let needs = PlaceNeeds {
        requires: &req.requires,
        prefer: &req.prefer,
        provider: req.provider,
        homes: homes.as_deref(),
    };
    let stations =
        place(&views, &sessions, &needs, req.count, req.spread).map_err(|e| no_station(&e))?;
    Ok(Json(json!({ "stations": stations })))
}
