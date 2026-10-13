//! The worktree routes of the board.
//!
//! `GET /api/stations/{station}/worktrees` (DL-142): the read-only
//! inventory of the worktrees that the station created for sessions. The
//! deck forwards the call to the station's `workspace.worktrees` method on
//! a live link and returns the answer; it keeps no copy. The paths and the
//! branch names are the owner's: the route needs the `admin` scope and the
//! owner's board login (DL-138).
//!
//! `POST /api/stations/{station}/worktrees/{trash|restore|purge|release}`
//! (DL-150) sends one worktree command to the station. The answer is the
//! command, like the generic command route; the outcome is the state of the
//! trash row in the inventory. Only the owner's board login may call them:
//! the commands move and delete folders.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::board::valid_command_id;
use fleetdeck_proto::command::{CommandOrigin, SessionCommand};
use fleetdeck_proto::link::{Outcome, WorktreeList, WorktreesQuery};
use fleetdeck_proto::{CommandId, SessionId};
use serde::Deserialize;
use serde_json::json;

use super::Board;
use super::command_routes::RECEIPT_WAIT;
use super::guard::{ApiError, owner_only};
use crate::link::CommandKind;
use crate::store::{AuditResult, DeckCommand, DeckError, NewAudit, Principal};

/// The longest the board waits for the station's answer. The station stops
/// its own inventory after 45 s.
pub const WORKTREES_WAIT: Duration = Duration::from_secs(60);

/// `GET /api/stations/{station}/worktrees`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 502
/// `station_error` (also a station without `workspace.worktrees.v1`); 504
/// `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
) -> Result<Json<WorktreeList>, ApiError> {
    super::guard::owner_only(&principal, "lists the worktrees of a station")?;
    let rec = board
        .store
        .find_station(station.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
            )
        })?;
    let unreachable = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            format!("station `{}` has no live link to this deck", rec.name),
        )
    };
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(unreachable)?;
    let outcome = tokio::time::timeout(
        WORKTREES_WAIT,
        control.request("workspace.worktrees", None, json!(WorktreesQuery {})),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the station did not answer in time",
        )
    })?
    .map_err(|_| unreachable())?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map(Json)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(match e.code.as_str() {
            "timeout" => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", e.message),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{}: {}", e.code, e.message),
            ),
        }),
    }
}

/// The body of the four worktree command routes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeBody {
    pub session_id: SessionId,
    pub command_id: CommandId,
    /// Only `purge`: the name of the trash folder.
    #[serde(default)]
    pub confirm_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Trash,
    Restore,
    Purge,
    Release,
}

impl Action {
    fn audit_name(self) -> &'static str {
        match self {
            Self::Trash => "worktree.trash",
            Self::Restore => "worktree.restore",
            Self::Purge => "worktree.purge",
            Self::Release => "worktree.release",
        }
    }

    fn command(self, body: WorktreeBody) -> Result<SessionCommand, ApiError> {
        let WorktreeBody {
            session_id,
            confirm_name,
            ..
        } = body;
        if self != Self::Purge && confirm_name.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "confirm_name belongs to the purge route only",
            ));
        }
        Ok(match self {
            Self::Trash => SessionCommand::WorktreeTrash { session_id },
            Self::Restore => SessionCommand::WorktreeRestore { session_id },
            Self::Purge => SessionCommand::WorktreePurge {
                session_id,
                confirm_name: confirm_name.unwrap_or_default(),
            },
            Self::Release => SessionCommand::WorktreeRelease { session_id },
        })
    }
}

/// What the audit row needs from a refusal.
type Refusal = (Option<String>, Option<CommandId>, ApiError);

async fn send(
    board: &Arc<Board>,
    action: Action,
    station: &str,
    body: Result<Json<WorktreeBody>, JsonRejection>,
) -> Result<(DeckCommand, bool, String), Refusal> {
    let Json(body) = body.map_err(|e| {
        (
            None,
            None,
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", e.body_text()),
        )
    })?;
    let id = body.command_id.clone();
    let session = body.session_id.to_string();
    let refuse = |e: ApiError| (Some(format!("{station}/{session}")), Some(id.clone()), e);
    if !valid_command_id(&id) {
        return Err(refuse(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_command_id",
            "a command id has 1 to 64 characters from [0-9A-Za-z_-]",
        )));
    }
    let command = action.command(body).map_err(&refuse)?;
    let rec = board
        .store
        .find_station(station.to_owned())
        .await
        .map_err(|e| refuse(ApiError::internal(e)))?
        .ok_or_else(|| {
            refuse(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
            ))
        })?;
    let target = format!("{station}/{session}");
    let (cmd, new) = board
        .link
        .submit_as(
            rec.station_id,
            CommandOrigin::Board,
            CommandKind::Session { command },
            id.clone(),
        )
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
            (Some(target.clone()), Some(id.clone()), api)
        })?;
    Ok((cmd, new, target))
}

async fn act(
    board: Arc<Board>,
    principal: Principal,
    station: String,
    action: Action,
    body: Result<Json<WorktreeBody>, JsonRejection>,
) -> Result<Json<DeckCommand>, ApiError> {
    let name = action.audit_name();
    let result = match owner_only(&principal, "changes worktrees") {
        Ok(()) => send(&board, action, &station, body).await,
        Err(e) => Err((None, None, e)),
    };
    match result {
        Ok((cmd, new, target)) => {
            let mut row = NewAudit::new(principal.actor(), name, AuditResult::Ok)
                .target(target)
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
        Err((target, id, e)) => {
            let mut row =
                NewAudit::new(principal.actor(), name, AuditResult::Refused).detail(e.code);
            row = row.target(target.unwrap_or(station));
            if let Some(id) = id {
                row = row.command(id);
            }
            board.store.audit_or_log(row).await;
            Err(e)
        }
    }
}

macro_rules! action_route {
    ($(#[$m:meta])* $name:ident, $action:expr) => {
        $(#[$m])*
        ///
        /// # Errors
        /// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
        /// `unknown_station`; 409 `not_synced` / `command_id_reused`.
        pub async fn $name(
            State(board): State<Arc<Board>>,
            Extension(principal): Extension<Principal>,
            Path(station): Path<String>,
            body: Result<Json<WorktreeBody>, JsonRejection>,
        ) -> Result<Json<DeckCommand>, ApiError> {
            act(board, principal, station, $action, body).await
        }
    };
}

action_route!(
    /// `POST /api/stations/{station}/worktrees/trash`.
    trash,
    Action::Trash
);
action_route!(
    /// `POST /api/stations/{station}/worktrees/restore`.
    restore,
    Action::Restore
);
action_route!(
    /// `POST /api/stations/{station}/worktrees/purge`.
    purge,
    Action::Purge
);
action_route!(
    /// `POST /api/stations/{station}/worktrees/release`.
    release,
    Action::Release
);
