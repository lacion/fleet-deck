//! The routes of a session move (design §12.2, §12.4A; FG-9, T4.4b). Every
//! POST needs the owner's board login and CSRF, is audited, and is closed to
//! members and tokens. Scope `admin`.
//!
//! - `POST /api/sessions/{station}/{session}/move { to_station, command_id }`
//!   (`session.move`): 202 `{ move_id }`.
//! - `POST /api/moves/{move}/undo { command_id }` (`session.move_undo`): 202
//!   `{ move_id, state }`.
//! - `GET /api/sessions/{station}/{session}/move` (`read`, owner only): the
//!   newest move of the session and the stations of the dialog.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::fleet_git::StartKind;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, StationId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::Board;
use crate::http::guard::{ApiError, owner_only};
use crate::http::project_git_routes::attach_start;
use crate::http::project_home_routes::{audit, check_command_id};
use crate::http::projects_routes::bad_body;
use crate::store::session_moves::{MoveEnv, MoveRefusal, MoveRequest, MoveRequested};
use crate::store::{Principal, StationRecord};

const MOVE: &str = "session.move";
const UNDO: &str = "session.move_undo";

/// The body of the move route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveBody {
    pub to_station: String,
    pub command_id: String,
}

/// The body of the undo route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndoBody {
    pub command_id: String,
}

fn refusal(r: &MoveRefusal) -> ApiError {
    let status = match r.code {
        "unknown_session" | "unknown_station" | "unknown_move" => StatusCode::NOT_FOUND,
        _ => StatusCode::CONFLICT,
    };
    ApiError::new(status, r.code, r.message.clone())
}

fn unknown_station(name: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_station",
        format!("no station is named `{name}`"),
    )
}

async fn station_of(board: &Board, name: &str) -> Result<StationRecord, ApiError> {
    board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| unknown_station(name))
}

/// The link state of a station and its provider instances, as a target.
fn env_of(board: &Board, station: &StationId) -> MoveEnv {
    MoveEnv {
        src: board.store.link_feature(station, feature::FLEET_GIT_MOVE),
        dst: board.store.link_feature(station, feature::FLEET_GIT_MOVE),
        dst_instances: board.store.station_info(station).map(|(info, _)| {
            info.providers
                .iter()
                .map(|p| p.instance.to_string())
                .collect()
        }),
    }
}

async fn move_inner(
    board: &Board,
    station: &str,
    session: &str,
    body: Result<Json<MoveBody>, JsonRejection>,
) -> (Option<CommandId>, Result<(String, String), ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        let src = station_of(board, station).await?;
        let dst = station_of(board, &body.to_station).await?;
        // A target that cannot start the session in its home: the start
        // refusals of the Git name and of the station (T4.4b Q11).
        let probe = board
            .store
            .move_probe(
                src.station_id.0.clone(),
                session.to_owned(),
                dst.station_id.0.clone(),
            )
            .await
            .map_err(ApiError::internal)?;
        if let Some((project, folder)) = probe
            && src.station_id != dst.station_id
        {
            match attach_start(board, &dst, &folder, StartKind::Worktree, Some(project)).await {
                Err(e)
                    if matches!(
                        e.code,
                        "slug_taken" | "station_info_missing" | "fleet_git_unsupported"
                    ) =>
                {
                    return Err(e);
                }
                _ => {}
            }
        }
        let mut env = env_of(board, &dst.station_id);
        env.src = board
            .store
            .link_feature(&src.station_id, feature::FLEET_GIT_MOVE);
        let req = MoveRequest {
            requested_by: "owner".to_owned(),
            request_command_id: body.command_id.clone(),
            src_station: src.station_id.0.clone(),
            src_session: session.to_owned(),
            dst_station: dst.station_id.0.clone(),
            authority: None,
        };
        match board
            .store
            .move_request(req, env)
            .await
            .map_err(ApiError::internal)?
        {
            MoveRequested::Started { move_id } => Ok((move_id, dst.name.clone())),
            MoveRequested::Refused(r) => Err(refusal(&r)),
        }
    };
    (Some(command), run.await)
}

/// `POST /api/sessions/{station}/{session}/move`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
/// `unknown_station` / `unknown_session`; 409 `move_not_ready` /
/// `fleet_git_unsupported` / `home_unverified` / `home_not_ready` /
/// `slug_taken` / `station_info_missing` / `command_id_reused`.
pub async fn start(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<MoveBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    owner_only(&principal, "moves a session")?;
    let (command, result) = move_inner(&board, &station, &session, body).await;
    let target = format!("{station}/{session}");
    audit(
        &board,
        &principal,
        MOVE,
        &target,
        command,
        &result,
        |(id, to)| format!("{station}/{session} -> {to} ({id})"),
    )
    .await;
    let (move_id, _) = result?;
    board.link.move_kick.kick();
    Ok((StatusCode::ACCEPTED, Json(json!({ "move_id": move_id }))))
}

async fn undo_inner(
    board: &Board,
    move_id: &str,
    body: Result<Json<UndoBody>, JsonRejection>,
) -> (Option<CommandId>, String, Result<String, ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, move_id.to_owned(), Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, move_id.to_owned(), Err(e)),
    };
    let row = match board.store.move_row(move_id.to_owned()).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let r = MoveRefusal::new("unknown_move", "Unknown move.");
            return (Some(command), move_id.to_owned(), Err(refusal(&r)));
        }
        Err(e) => {
            return (
                Some(command),
                move_id.to_owned(),
                Err(ApiError::internal(e)),
            );
        }
    };
    let target = format!(
        "{}/{} <- {}",
        row.src_station, row.src_session, row.dst_station
    );
    let result = match board
        .store
        .move_undo_request(move_id.to_owned(), body.command_id)
        .await
    {
        Ok(Ok(row)) => Ok(row.move_id),
        Ok(Err(r)) => Err(refusal(&r)),
        Err(e) => Err(ApiError::internal(e)),
    };
    (Some(command), target, result)
}

/// `POST /api/moves/{move}/undo`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
/// `unknown_move`; 409 `move_not_undoable` / `command_id_reused`.
pub async fn undo(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(move_id): Path<String>,
    body: Result<Json<UndoBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    owner_only(&principal, "undoes a move")?;
    let (command, target, result) = undo_inner(&board, &move_id, body).await;
    audit(&board, &principal, UNDO, &target, command, &result, |id| {
        format!("move {id}")
    })
    .await;
    let id = result?;
    board.link.move_kick.kick();
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "move_id": id, "state": "undoing" })),
    ))
}

/// `GET /api/sessions/{station}/{session}/move`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`.
pub async fn read(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    owner_only(&principal, "reads a move")?;
    let src = station_of(&board, &station).await?;
    let id = src.station_id.0.clone();
    let current = board
        .store
        .move_of_session(id.clone(), session.clone())
        .await
        .map_err(ApiError::internal)?;
    let envs = board
        .store
        .stations()
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|s| (s.station_id.0.clone(), env_of(&board, &s.station_id)))
        .collect();
    let targets = board
        .store
        .move_targets(id, session, envs)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "move": current, "targets": targets })))
}
