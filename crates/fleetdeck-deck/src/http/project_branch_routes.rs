//! The branches of other stations (design §13.1; FG-10, T4.4a).
//!
//! - `GET /api/projects/{id}/branches` (`read`): asks `project.branches` of the
//!   radar home and maps the answer to stations, sessions and the radar state.
//! - `POST /api/projects/{id}/fetch` (`drive`, CSRF, audited `project.fetch`):
//!   asks `project.fetch` of the radar home and returns its `FetchAnswer`.
//!
//! Both are closed to members (the method table has no member column).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{BranchesParams, FetchAnswer, FetchParams, RemoteBranches, code};
use fleetdeck_proto::integrations::Project;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde::de::DeserializeOwned;

use crate::http::Board;
use crate::http::guard::ApiError;
use crate::link::coordinator_git_reads::{BRANCHES_WAIT, FETCH_WAIT};
use crate::store::branch_map::{BranchesView, SYNC_OFF, UNVERIFIED};
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal};

fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

/// The project, its radar home station and the live link to it.
async fn target(board: &Board, id: &str) -> Result<(Project, StationId, Control), ApiError> {
    let found = board
        .store
        .branch_lookup(id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "unknown_project", "Unknown project.")
        })?;
    if !found.project.git.sync {
        return Err(conflict("sync_off", SYNC_OFF));
    }
    let Some((station, name)) = found.radar else {
        return Err(conflict("home_unverified", UNVERIFIED));
    };
    if board
        .store
        .link_feature(&station, feature::FLEET_GIT_BRANCHES)
        == LinkFeature::Without
    {
        return Err(conflict(
            code::FLEET_GIT_UNSUPPORTED,
            "This station does not support Fleet Git. Update it first.",
        ));
    }
    let control = board.link.control_for(&station).ok_or_else(|| {
        conflict(
            "station_unreachable",
            format!("station `{name}` has no live link to this deck"),
        )
    })?;
    Ok((found.project, station, control))
}

/// One request to the station and its parsed answer.
async fn ask<T: DeserializeOwned>(
    control: &Control,
    wait: Duration,
    method: &str,
    params: serde_json::Value,
) -> Result<T, ApiError> {
    let outcome = tokio::time::timeout(wait, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| conflict("station_unreachable", "the link to the station ended"))?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            format!("{}: {}", e.code, e.message),
        )),
    }
}

async fn branches_of(board: &Board, id: &str) -> Result<BranchesView, ApiError> {
    let (project, _, control) = target(board, id).await?;
    let params = serde_json::to_value(BranchesParams {
        project_id: id.to_owned(),
    })
    .map_err(ApiError::internal)?;
    let answer: RemoteBranches = ask(&control, BRANCHES_WAIT, "project.branches", params).await?;
    board
        .store
        .branch_view(&project, answer)
        .await
        .map_err(ApiError::internal)
}

/// `GET /api/projects/{id}/branches`.
///
/// # Errors
/// 404 `unknown_project`; 409 `sync_off` / `home_unverified` /
/// `fleet_git_unsupported` / `station_unreachable`; 502 `station_error`; 504
/// `timeout`.
pub async fn branches(
    State(board): State<Arc<Board>>,
    Path(id): Path<String>,
) -> Result<Json<BranchesView>, ApiError> {
    branches_of(&board, &id).await.map(Json)
}

async fn fetch_of(board: &Board, id: &str) -> Result<(FetchAnswer, StationId), ApiError> {
    let (_, station, control) = target(board, id).await?;
    let params = serde_json::to_value(FetchParams {
        project_id: id.to_owned(),
    })
    .map_err(ApiError::internal)?;
    let answer = ask(&control, FETCH_WAIT, "project.fetch", params).await?;
    Ok((answer, station))
}

/// `POST /api/projects/{id}/fetch`.
///
/// # Errors
/// As [`branches`]; the answer of the station carries a failed fetch in
/// `code` and `text`.
pub async fn fetch(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<FetchAnswer>, ApiError> {
    let result = fetch_of(&board, &id).await;
    let row = match &result {
        Ok((_, station)) => NewAudit::new(principal.actor(), "project.fetch", AuditResult::Ok)
            .target(id.clone())
            .detail(station.to_string()),
        Err(e) => NewAudit::new(principal.actor(), "project.fetch", AuditResult::Failed)
            .target(id.clone())
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(|(answer, _)| Json(answer))
}
