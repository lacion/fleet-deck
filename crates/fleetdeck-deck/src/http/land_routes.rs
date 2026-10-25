//! The land routes of the board (design §11.2, §11.8, §11.9; FG-8). Every
//! POST needs the owner's board login and CSRF, is audited, and is closed to
//! members and tokens. Scope `admin` (decisions Q9).
//!
//! - `POST /api/sessions/{station}/{session}/land { command_id }`
//!   (`session.land`): 202 `{ land_id, position }`.
//! - `GET /api/projects/{id}/lands` (`read`): `{ queue, recent }`.
//! - `POST /api/lands/{land}/cancel` (`land.cancel`), `.../abandon`
//!   (`land.abandon`), `.../forget` (`land.forget`), `.../send { command_id }`
//!   (`land.send_to_session`).

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::command::{CommandOrigin, SessionCommand, UserInput};
use fleetdeck_proto::{CommandId, SessionId, StationId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::Board;
use crate::http::command_routes::RECEIPT_WAIT;
use crate::http::guard::{ApiError, owner_only};
use crate::http::project_home_routes::{audit, check_command_id};
use crate::http::projects_routes::bad_body;
use crate::link::router::CommandKind;
use crate::store::land_queue::{LandOutcome, LandRefusal, LandRequest, LandRequested};
use crate::store::{DeckError, Principal};

const LAND: &str = "session.land";
const CANCEL: &str = "land.cancel";
const ABANDON: &str = "land.abandon";
const FORGET: &str = "land.forget";
const SEND: &str = "land.send_to_session";

/// The body of the land and the send routes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandBody {
    pub command_id: String,
}

/// The error of a refusal of the land flow.
fn refusal(r: LandRefusal) -> ApiError {
    let status = match r.code {
        "unknown_session" | "unknown_land" | "unknown_station" => StatusCode::NOT_FOUND,
        _ => StatusCode::CONFLICT,
    };
    ApiError::new(status, r.code, r.message)
}

fn outcome(out: Result<LandOutcome, DeckError>) -> Result<Value, ApiError> {
    let view = out.map_err(ApiError::internal)?.map_err(refusal)?;
    serde_json::to_value(view).map_err(ApiError::internal)
}

async fn land_inner(
    board: &Board,
    station: &str,
    session: &str,
    body: Result<Json<CommandBody>, JsonRejection>,
) -> (Option<CommandId>, Result<(String, u32), ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        let rec = board
            .store
            .find_station(station.to_owned())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_station",
                    format!("no station is named `{station}`"),
                )
            })?;
        let asked = board
            .store
            .land_request(LandRequest {
                requested_by: "owner".to_owned(),
                request_command_id: body.command_id.clone(),
                station_id: rec.station_id.0,
                session_id: session.to_owned(),
                authority: None,
                required_classes: None,
            })
            .await
            .map_err(ApiError::internal)?;
        match asked {
            LandRequested::Queued { land_id, position } => Ok((land_id, position)),
            LandRequested::Refused(r) => Err(refusal(r)),
        }
    };
    (Some(command), run.await)
}

/// `POST /api/sessions/{station}/{session}/land`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
/// `unknown_station` / `unknown_session`; 409 `land_not_ready` / `land_open`
/// / `land_pr_needs_forge_branch` / `home_unverified` /
/// `fleet_git_unsupported` / `command_id_reused`.
pub async fn land(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<CommandBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    owner_only(&principal, "lands a session")?;
    let (command, result) = land_inner(&board, &station, &session, body).await;
    audit(
        &board,
        &principal,
        LAND,
        &session,
        command,
        &result,
        |(id, _)| format!("land {id} on {station}"),
    )
    .await;
    let (land_id, position) = result?;
    board.link.land_kick.kick();
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "land_id": land_id, "position": position })),
    ))
}

/// `GET /api/projects/{id}/lands`.
///
/// # Errors
/// 404 `unknown_project`.
pub async fn project_lands(
    State(board): State<Arc<Board>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let lands = board
        .store
        .project_lands(id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "unknown_project", "Unknown project.")
        })?;
    Ok(Json(
        serde_json::to_value(lands).map_err(ApiError::internal)?,
    ))
}

/// `POST /api/lands/{land}/cancel`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_land`; 409 `land_running` /
/// `land_not_queued`.
pub async fn cancel(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(land): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owner_only(&principal, "cancels a land")?;
    let result = outcome(board.store.land_cancel(land.clone()).await);
    audit(&board, &principal, CANCEL, &land, None, &result, |_| {
        String::new()
    })
    .await;
    result?;
    board.link.land_kick.kick();
    Ok(Json(json!({ "state": "cancelled" })))
}

/// `POST /api/lands/{land}/abandon`: "Stop checking".
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_land`; 409 `land_not_uncertain`.
pub async fn abandon(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(land): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    owner_only(&principal, "stops checking a land")?;
    let result = outcome(board.store.land_abandon_request(land.clone()).await);
    audit(&board, &principal, ABANDON, &land, None, &result, |_| {
        String::new()
    })
    .await;
    result?;
    board.link.land_kick.kick();
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "abandon_state": "requested" })),
    ))
}

/// `POST /api/lands/{land}/forget`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_land`; 409 `land_not_uncertain` /
/// `station_paired`.
pub async fn forget(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(land): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owner_only(&principal, "forgets a land")?;
    let result = outcome(board.store.land_forget(land.clone()).await);
    audit(&board, &principal, FORGET, &land, None, &result, |_| {
        String::new()
    })
    .await;
    board.link.land_kick.kick();
    Ok(Json(result?))
}

async fn send_inner(
    board: &Board,
    land: &str,
    body: Result<Json<CommandBody>, JsonRejection>,
) -> (Option<CommandId>, Result<Value, ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        let row = board
            .store
            .land_row(land.to_owned())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| refusal(crate::store::land_queue::unknown_land()))?;
        let text = (row.state == "conflict")
            .then(|| crate::store::land_text::conflict_prompt(&row))
            .flatten()
            .filter(|_| !row.files.is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "land_not_conflict",
                    "Only a land with a conflict can be sent to its session.",
                )
            })?;
        let station = StationId::from(row.station_id.clone());
        let session = SessionId::from(row.session_id.clone());
        let known = board
            .store
            .session_known(station.0.clone(), session.0.clone())
            .await
            .map_err(ApiError::internal)?;
        if !known {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_session",
                "The station does not know this session.",
            ));
        }
        let idle = board
            .store
            .session_idle(station.0.clone(), session.0.clone())
            .await
            .map_err(ApiError::internal)?;
        match idle {
            Some(true) => {}
            Some(false) => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "session_not_ready",
                    "The session is working.",
                ));
            }
            None => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "session_not_ready",
                    "Resume the session first.",
                ));
            }
        }
        let command = SessionCommand::Prompt {
            session_id: session,
            input: UserInput::new(text),
        };
        let (cmd, _) = board
            .link
            .submit_as(
                station,
                CommandOrigin::Board,
                CommandKind::Session { command },
                CommandId::from(body.command_id.clone()),
            )
            .await
            .map_err(|e| match &e {
                DeckError::NotSynced(_) => {
                    ApiError::new(StatusCode::CONFLICT, "not_synced", e.to_string())
                }
                DeckError::CommandIdReused(_) => {
                    ApiError::new(StatusCode::CONFLICT, "command_id_reused", e.to_string())
                }
                _ => ApiError::internal(&e),
            })?;
        let cmd = board
            .link
            .wait_command(cmd.command_id, RECEIPT_WAIT)
            .await
            .map_err(ApiError::internal)?;
        Ok(json!({ "command_id": cmd.command_id, "state": cmd.state }))
    };
    (Some(command), run.await)
}

/// `POST /api/lands/{land}/send`: "Send to session".
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
/// `unknown_land` / `unknown_session`; 409 `land_not_conflict` /
/// `session_not_ready` / `command_id_reused`.
pub async fn send(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(land): Path<String>,
    body: Result<Json<CommandBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    owner_only(&principal, "sends a land to its session")?;
    let (command, result) = send_inner(&board, &land, body).await;
    audit(&board, &principal, SEND, &land, command, &result, |_| {
        String::new()
    })
    .await;
    Ok(Json(result?))
}
