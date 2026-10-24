//! The project of a session (DL-176). Scope `drive`, CSRF, audited, closed to
//! members (the method table has no member column for them).
//!
//! - `POST /api/projects/{id}/sessions/assign { station, session_id }`: the
//!   session belongs to the project.
//! - `POST /api/sessions/{station}/{session}/project/clear`: the session has
//!   no project, whatever its folder.
//! - `POST /api/sessions/{station}/{session}/project/auto`: back to the
//!   automatic match.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::guard::ApiError;
use super::projects_routes::{bad_body, load};
use crate::store::{AuditResult, NewAudit, Principal};

/// The body of the assign route. `station` is a station id.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignBody {
    pub station: String,
    pub session_id: String,
}

fn unknown_session() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_session", "no such session")
}

/// Runs one change and writes its audit row.
async fn audited(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    target: String,
    detail: String,
    result: Result<Value, ApiError>,
) -> Result<Json<Value>, ApiError> {
    let row = match &result {
        Ok(_) => NewAudit::new(principal.actor(), action, AuditResult::Ok)
            .target(target)
            .detail(detail),
        Err(e) => NewAudit::new(principal.actor(), action, AuditResult::Failed)
            .target(target)
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(Json)
}

async fn check_session(board: &Board, station: &str, session: &str) -> Result<(), ApiError> {
    let known = board
        .store
        .session_known(station.to_owned(), session.to_owned())
        .await
        .map_err(ApiError::internal)?;
    if known {
        Ok(())
    } else {
        Err(unknown_session())
    }
}

async fn assign_inner(board: &Board, id: &str, body: &AssignBody) -> Result<Value, ApiError> {
    let project = load(board, id).await?;
    // The project needs a home on the station of the session (design §8.3).
    let has_home = if project.homes.is_empty() {
        project.station_id == body.station
    } else {
        project
            .homes
            .iter()
            .any(|h| h.station_id.as_str() == body.station)
    };
    if !has_home {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "wrong_station",
            "This project has no home on that station.",
        ));
    }
    check_session(board, &body.station, &body.session_id).await?;
    board
        .store
        .set_session_project(
            body.station.clone(),
            body.session_id.clone(),
            Some(project.project_id.clone()),
        )
        .await
        .map_err(ApiError::internal)?;
    Ok(json!({ "project_id": project.project_id }))
}

/// `POST /api/projects/{id}/sessions/assign`.
///
/// # Errors
/// 400 `invalid_body` / `wrong_station`; 404 `unknown_project` /
/// `unknown_session`.
pub async fn assign(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<AssignBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let (target, result, detail) = match body {
        Err(e) => (id.clone(), Err(bad_body(&e)), String::new()),
        Ok(Json(b)) => {
            let detail = format!("{}/{}", b.station, b.session_id);
            (id.clone(), assign_inner(&board, &id, &b).await, detail)
        }
    };
    audited(
        &board,
        &principal,
        "sessions.project_assign",
        target,
        detail,
        result,
    )
    .await
}

/// `POST /api/sessions/{station}/{session}/project/clear`.
///
/// # Errors
/// 404 `unknown_session`.
pub async fn clear(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let result = match check_session(&board, &station, &session).await {
        Ok(()) => board
            .store
            .set_session_project(station.clone(), session.clone(), None)
            .await
            .map(|()| json!({ "project_id": null }))
            .map_err(ApiError::internal),
        Err(e) => Err(e),
    };
    audited(
        &board,
        &principal,
        "sessions.project_clear",
        session,
        station,
        result,
    )
    .await
}

/// `POST /api/sessions/{station}/{session}/project/auto`.
///
/// # Errors
/// 404 `unknown_session`.
pub async fn auto(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let result = match check_session(&board, &station, &session).await {
        Ok(()) => board
            .store
            .reset_session_project(station.clone(), session.clone())
            .await
            .map(|_| json!({ "automatic": true }))
            .map_err(ApiError::internal),
        Err(e) => Err(e),
    };
    audited(
        &board,
        &principal,
        "sessions.project_auto",
        session,
        station,
        result,
    )
    .await
}
