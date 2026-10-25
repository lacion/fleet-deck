//! The homes of a project (design §8.4; FG-5 phase 2): add, retry, remove.
//! Owner's board login only, CSRF, audited, closed to members and tokens.
//!
//! - `POST /api/projects/{id}/homes { station, command_id }` (`project.home_add`):
//!   202 `{ generation, attempt_id }`. The same `command_id` for the same
//!   request answers the first answer again.
//! - `POST /api/projects/{id}/homes/{station}/retry { command_id }`
//!   (`project.home_retry`): 202 `{ generation, attempt_id }`.
//! - `DELETE /api/projects/{id}/homes/{station}` (`project.home_remove`): 204.
//!
//! The station is a station id. The deck never calls the station from here:
//! the attempt is stored and `fleet_git_homes` sends it.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::{CommandId, StationId};
use serde::Deserialize;

use super::Board;
use super::guard::{ApiError, owner_only};
use super::projects_routes::bad_body;
use crate::link::fleet_git_sync;
use crate::store::project_home_ops::HomeAttemptStarted;
use crate::store::project_homes::HomeRefusal;
use crate::store::{AuditResult, NewAudit, Principal};

const ADD: &str = "project.home_add";
const RETRY: &str = "project.home_retry";
const REMOVE: &str = "project.home_remove";

/// The body of the add route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddBody {
    pub station: String,
    pub command_id: String,
    /// An absolute folder on the station to adopt instead of a new clone.
    #[serde(default)]
    pub path: Option<String>,
}

/// The body of the retry route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryBody {
    pub command_id: String,
}

/// The status of a refusal of the homes (design §8.7).
fn status_of(code: &str) -> StatusCode {
    match code {
        "unknown_project" | "unknown_station" | "unknown_home" => StatusCode::NOT_FOUND,
        "needs_remote" | "invalid_git_settings" | "invalid_body" | "invalid_path" => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::CONFLICT,
    }
}

/// The error of a refusal of the homes.
pub(super) fn refusal(r: HomeRefusal) -> ApiError {
    let error = ApiError::new(status_of(r.code), r.code, r.message);
    match r.field {
        Some(field) => {
            let mut details = serde_json::Map::new();
            details.insert("field".to_owned(), field.into());
            error.with_details(details)
        }
        None => error,
    }
}

/// 400 `invalid_command_id` for a command id outside the rule.
pub(super) fn check_command_id(id: &str) -> Result<CommandId, ApiError> {
    let id = CommandId::from(id.to_owned());
    if fleetdeck_proto::board::valid_command_id(&id) {
        Ok(id)
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_command_id",
            "a command id has 1 to 64 characters from [0-9A-Za-z_-]",
        ))
    }
}

/// 404 `unknown_project` ("Unknown project.") unless the project exists.
pub(super) async fn known_project(board: &Board, id: &str) -> Result<(), ApiError> {
    match board
        .store
        .project(id.to_owned())
        .await
        .map_err(ApiError::internal)?
    {
        Some(_) => Ok(()),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_project",
            "Unknown project.",
        )),
    }
}

/// 404 `unknown_station` unless `station` is a paired station id.
async fn known_station(board: &Board, station: &str) -> Result<(), ApiError> {
    match board
        .store
        .station(StationId::from(station.to_owned()))
        .await
        .map_err(ApiError::internal)?
    {
        Some(_) => Ok(()),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_station",
            "Unknown station.",
        )),
    }
}

/// Writes the audit row of one route: `Ok` with the detail, or `Refused`
/// with the code.
pub(super) async fn audit<T>(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    target: &str,
    command: Option<CommandId>,
    result: &Result<T, ApiError>,
    detail: impl FnOnce(&T) -> String,
) {
    let row = match result {
        Ok(done) => NewAudit::new(principal.actor(), action, AuditResult::Ok)
            .target(target.to_owned())
            .detail(detail(done)),
        Err(e) => NewAudit::new(principal.actor(), action, AuditResult::Refused)
            .target(target.to_owned())
            .detail(e.code),
    };
    let row = match command {
        Some(id) => row.command(id),
        None => row,
    };
    board.store.audit_or_log(row).await;
}

async fn add_inner(
    board: &Board,
    id: &str,
    body: Result<Json<AddBody>, JsonRejection>,
) -> (Option<CommandId>, Result<HomeAttemptStarted, ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        known_project(board, id).await?;
        known_station(board, &body.station).await?;
        board
            .store
            .home_add(
                id.to_owned(),
                body.station.clone(),
                body.command_id.clone(),
                body.path.clone(),
            )
            .await
            .map_err(ApiError::internal)?
            .map_err(refusal)
    };
    (Some(command), run.await)
}

/// `POST /api/projects/{id}/homes`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id` / `needs_remote` /
/// `invalid_path`; 403
/// `owner_only`; 404 `unknown_project` / `unknown_station`; 409
/// `command_id_reused` / `home_exists` / `station_offline` /
/// `fleet_git_unsupported` / `remote_local_refused` / `station_info_missing`
/// / `slug_taken`.
pub async fn add(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<AddBody>, JsonRejection>,
) -> Result<(StatusCode, Json<HomeAttemptStarted>), ApiError> {
    owner_only(&principal, "adds a home")?;
    let station = body.as_ref().ok().map(|b| b.station.clone());
    let path = body
        .as_ref()
        .ok()
        .and_then(|b| b.path.as_deref())
        .map(|p| p.trim().to_owned());
    let (command, result) = add_inner(&board, &id, body).await;
    audit(&board, &principal, ADD, &id, command, &result, |done| {
        let mut detail = format!(
            "station {} generation {}",
            station.as_deref().unwrap_or_default(),
            done.generation
        );
        if let Some(path) = &path {
            detail.push_str(" path ");
            detail.push_str(path);
        }
        detail
    })
    .await;
    let started = result?;
    if let Some(station) = station {
        // The sender delivers the stored request; the station learns the home.
        board.link.homes_kick.kick();
        fleet_git_sync::spawn_changed(&board.link, StationId::from(station));
    }
    Ok((StatusCode::ACCEPTED, Json(started)))
}

async fn retry_inner(
    board: &Board,
    id: &str,
    station: &str,
    body: Result<Json<RetryBody>, JsonRejection>,
) -> (Option<CommandId>, Result<HomeAttemptStarted, ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        board
            .store
            .home_retry(id.to_owned(), station.to_owned(), body.command_id.clone())
            .await
            .map_err(ApiError::internal)?
            .map_err(refusal)
    };
    (Some(command), run.await)
}

/// `POST /api/projects/{id}/homes/{station}/retry`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
/// `unknown_project` / `unknown_home`; 409 `command_id_reused` /
/// `home_not_failed`.
pub async fn retry(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((id, station)): Path<(String, String)>,
    body: Result<Json<RetryBody>, JsonRejection>,
) -> Result<(StatusCode, Json<HomeAttemptStarted>), ApiError> {
    owner_only(&principal, "retries a home")?;
    let (command, result) = retry_inner(&board, &id, &station, body).await;
    audit(&board, &principal, RETRY, &id, command, &result, |done| {
        format!("station {station} generation {}", done.generation)
    })
    .await;
    let started = result?;
    board.link.homes_kick.kick();
    Ok((StatusCode::ACCEPTED, Json(started)))
}

/// `DELETE /api/projects/{id}/homes/{station}`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_project` / `unknown_home`; 409 `last_home`
/// / `home_busy` / `no_ready_home`.
pub async fn remove(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((id, station)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    owner_only(&principal, "removes a home")?;
    let result = board
        .store
        .home_remove(id.clone(), station.clone())
        .await
        .map_err(ApiError::internal)
        .and_then(|r| r.map_err(refusal));
    audit(&board, &principal, REMOVE, &id, None, &result, |()| {
        format!("station {station}")
    })
    .await;
    result?;
    // The station sees the home absent and invalidates its attempt; no file
    // is deleted.
    fleet_git_sync::spawn_changed(&board.link, StationId::from(station));
    Ok(StatusCode::NO_CONTENT)
}
