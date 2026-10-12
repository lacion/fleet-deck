//! The command methods of the board API (M2 P3-9a; PLAN §9 "REST for
//! commands", §8.4, L15). `POST /api/commands/<method>` routes one session
//! command to a station through the deck's command router; the board
//! chooses the `command_id`, so a resend after a lost response returns the
//! stored command and sends nothing new. `GET /api/commands/{command_id}`
//! polls one command. Every call is audited with the caller.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, MatchedPath, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::board::{CommandRequest, valid_command_id};
use fleetdeck_proto::command::{CommandOrigin, UserInput};

use super::Board;
use super::guard::ApiError;
use super::methods::COMMAND_PREFIX;
use crate::link::CommandKind;
use crate::store::{AuditResult, DeckCommand, DeckError, NewAudit, Principal};

/// How long a command call waits for the station's receipt before it
/// answers with the `pending` command (the board then polls).
pub const RECEIPT_WAIT: Duration = Duration::from_secs(5);

/// `POST /api/commands/<method>`.
///
/// # Errors
/// 400 `invalid_body` / `wrong_method` / `invalid_command_id`, 404
/// `unknown_station`, 409 `not_synced` / `command_id_reused`.
pub async fn command(
    State(board): State<Arc<Board>>,
    matched: MatchedPath,
    Extension(principal): Extension<Principal>,
    body: Result<Json<CommandRequest>, JsonRejection>,
) -> Result<Json<DeckCommand>, ApiError> {
    let method = matched
        .as_str()
        .strip_prefix(COMMAND_PREFIX)
        .unwrap_or_default()
        .to_owned();
    let audit = NewAudit::new(principal.actor(), method.clone(), AuditResult::Refused);
    match route(&board, &method, body).await {
        Ok((cmd, new)) => {
            let mut row = NewAudit::new(principal.actor(), method, AuditResult::Ok)
                .target(cmd.station_id.0.clone())
                .command(cmd.command_id.clone());
            if !new {
                row = row.detail("duplicate");
            }
            board.store.audit_or_log(row).await;
            let cmd = board
                .link
                .wait_command(cmd.command_id, RECEIPT_WAIT)
                .await
                .map_err(ApiError::internal)?;
            Ok(Json(cmd))
        }
        Err((station, id, e)) => {
            let mut row = audit.detail(e.code);
            if let Some(station) = station {
                row = row.target(station);
            }
            if let Some(id) = id {
                row = row.command(id);
            }
            board.store.audit_or_log(row).await;
            Err(e)
        }
    }
}

type Refusal = (Option<String>, Option<CommandId>, ApiError);

async fn route(
    board: &Arc<Board>,
    method: &str,
    body: Result<Json<CommandRequest>, JsonRejection>,
) -> Result<(DeckCommand, bool), Refusal> {
    let Json(req) = body.map_err(|e| {
        (
            None,
            None,
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", e.body_text()),
        )
    })?;
    let id = Some(req.command_id.clone());
    let station = Some(req.station.clone());
    let refuse = |status, code, message: String| {
        (
            station.clone(),
            id.clone(),
            ApiError::new(status, code, message),
        )
    };
    if req.command.method() != method {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "wrong_method",
            format!(
                "the body holds a {} command; this route takes {method}",
                req.command.method()
            ),
        ));
    }
    if !valid_command_id(&req.command_id) {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "invalid_command_id",
            "a command id has 1 to 64 characters from [0-9A-Za-z_-]".to_owned(),
        ));
    }
    if let Some(Err(message)) = req.command.user_input().map(UserInput::check) {
        return Err(refuse(StatusCode::BAD_REQUEST, "invalid_input", message));
    }
    let found = board
        .store
        .find_station(req.station.clone())
        .await
        .map_err(|e| (station.clone(), id.clone(), ApiError::internal(e)))?;
    let Some(rec) = found else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "unknown_station",
            format!("no station is named `{}`", req.station),
        ));
    };
    let station = Some(rec.station_id.0.clone());
    let kind = CommandKind::Session {
        command: req.command,
    };
    board
        .link
        .submit_as(rec.station_id, CommandOrigin::Board, kind, req.command_id)
        .await
        .map_err(|e| {
            let api = match &e {
                DeckError::NotSynced(_) => {
                    ApiError::new(StatusCode::CONFLICT, "not_synced", e.to_string())
                }
                DeckError::CommandIdReused(_) => {
                    ApiError::new(StatusCode::CONFLICT, "command_id_reused", e.to_string())
                }
                DeckError::UnknownStation(_) => {
                    ApiError::new(StatusCode::NOT_FOUND, "unknown_station", e.to_string())
                }
                _ => ApiError::internal(&e),
            };
            (station, id, api)
        })
}

/// `GET /api/commands/{command_id}`.
///
/// # Errors
/// 404 `unknown_command`.
pub async fn get_command(
    State(board): State<Arc<Board>>,
    Path(command_id): Path<String>,
) -> Result<Json<DeckCommand>, ApiError> {
    board
        .store
        .command(CommandId::from(command_id))
        .await
        .map_err(ApiError::internal)?
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_command", "no such command"))
}
