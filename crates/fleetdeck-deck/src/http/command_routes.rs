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
use fleetdeck_proto::StationId;
use fleetdeck_proto::board::{CommandRequest, valid_command_id};
use fleetdeck_proto::command::{
    CommandOrigin, SessionCommand, UserInput, WorkspaceRequest, check_native_session_id,
};
use fleetdeck_proto::event::RequestKind;
use fleetdeck_proto::fleet_git::StartKind;
use fleetdeck_proto::version::feature;

use super::execution_routes::{StopAudit, stop_gates, watch_outcome};
use super::guard::ApiError;
use super::methods::COMMAND_PREFIX;
use super::{Board, access};
use crate::link::CommandKind;
use crate::store::worker_grant::{Provenance, WorkerStart};
use crate::store::{
    AuditResult, DeckCommand, DeckError, LinkFeature, MemberAction, NewAudit, Principal,
    ShareGrant, member_allows,
};

/// How long a command call waits for the station's receipt before it
/// answers with the `pending` command (the board then polls).
pub const RECEIPT_WAIT: Duration = Duration::from_secs(5);

/// The method of the command that stops the processes of a lost run.
const STOP_METHOD: &str = "session.stop_execution";

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
    // BG-4: a stop command has its own audit rows next to the generic one.
    let stop = match &body {
        Ok(Json(req)) if method == STOP_METHOD => StopAudit::of(req),
        _ => None,
    };
    match route(&board, &method, &principal, body).await {
        Ok((cmd, new)) => {
            let mut row = NewAudit::new(principal.actor(), method, AuditResult::Ok)
                .target(cmd.station_id.0.clone())
                .command(cmd.command_id.clone());
            if !new {
                row = row.detail("duplicate");
            }
            board.store.audit_or_log(row).await;
            if let Some(stop) = stop.as_ref().filter(|_| new) {
                stop.accepted(&board, &principal.actor(), &cmd.station_id.0)
                    .await;
                // The outcome row does not wait for a browser (BG-4 §5.6).
                watch_outcome(
                    Arc::clone(&board),
                    principal.actor(),
                    cmd.station_id.clone(),
                    stop.clone(),
                );
            }
            let cmd = board
                .link
                .wait_command(cmd.command_id, RECEIPT_WAIT)
                .await
                .map_err(ApiError::internal)?;
            Ok(Json(cmd))
        }
        Err((station, id, e)) => {
            if let Some(stop) = &stop {
                stop.refused(
                    &board,
                    &principal.actor(),
                    station.as_deref().unwrap_or("-"),
                    e.code,
                )
                .await;
            }
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
    principal: &Principal,
    body: Result<Json<CommandRequest>, JsonRejection>,
) -> Result<(DeckCommand, bool), Refusal> {
    let owner = matches!(principal, Principal::Owner(_));
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
    if let SessionCommand::Import {
        native_session_id, ..
    } = &req.command
        && let Err(message) = check_native_session_id(native_session_id)
    {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "invalid_session_id",
            message.to_owned(),
        ));
    }
    // MCP P0 §2.6: an import never carries a coordinator grant.
    if let SessionCommand::Import { spec, .. } = &req.command
        && spec.coordinator.is_some()
    {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "invalid_command",
            "an import never carries a coordinator grant".to_owned(),
        ));
    }
    let found = board
        .store
        .find_station(req.station.clone())
        .await
        .map_err(|e| (station.clone(), id.clone(), ApiError::internal(e)))?;
    let Some(rec) = found else {
        // A member learns nothing about the stations of the deck.
        if matches!(principal, Principal::Member(..)) {
            return Err(refuse(
                StatusCode::NOT_FOUND,
                "unknown_session",
                "no such session".to_owned(),
            ));
        }
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "unknown_station",
            format!("no station is named `{}`", req.station),
        ));
    };
    let station = Some(rec.station_id.0.clone());
    // A pinned import needs a station that verifies the pin: an older one
    // would ignore the field and launch an unverified import (B7 PRE-PR r1).
    // A station that is down is checked again at every send.
    if matches!(
        &req.command,
        SessionCommand::Import {
            transcript: Some(_),
            ..
        }
    ) && board
        .store
        .link_feature(&rec.station_id, feature::IMPORT_PIN)
        == LinkFeature::Without
    {
        return Err(refuse(
            StatusCode::CONFLICT,
            "import_pin_unsupported",
            "This station cannot verify an import. Update it.".to_owned(),
        ));
    }
    // Stopping the processes of a lost run (BG-4): the owner, the feature and
    // the elevation are checked before anything else is stored or sent.
    if matches!(&req.command, SessionCommand::StopExecution { .. }) {
        stop_gates(board, principal, &rec.station_id, req.elevation.as_deref())
            .map_err(|e| (station.clone(), id.clone(), e))?;
    }
    let mut command = req.command;
    if let Principal::Member(_, grant) = principal {
        member_command(board, grant, &rec.station_id, &mut command)
            .await
            .map_err(|e| (station.clone(), id.clone(), e))?;
    } else {
        // Only the deck says who typed an input: a caller cannot forge it.
        clear_author(&mut command);
    }
    // A setup command runs shell code on the station: only the owner sends
    // one, and only to a station that reports it runs setup commands
    // (an older station would ignore the field, DL-148).
    if let SessionCommand::Start {
        setup_cmd: Some(_), ..
    } = &command
    {
        if !owner {
            return Err((
                station.clone(),
                id.clone(),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "owner_only",
                    "only the owner's board login starts a session with a setup command".to_owned(),
                ),
            ));
        }
        let runs_setup = board
            .store
            .station_info(&rec.station_id)
            .is_some_and(|(info, _)| info.allow_setup_cmd == Some(true));
        if !runs_setup {
            return Err((
                station.clone(),
                id.clone(),
                ApiError::new(
                    StatusCode::CONFLICT,
                    "setup_not_supported",
                    "this station does not run setup commands".to_owned(),
                ),
            ));
        }
    }
    // An owner start with a coordinator request: the deck creates the grant
    // and fills in its id and the control skill (ADR-006 §4, DL-217). Only
    // the owner creates a grant (ADR-006 §4): a bearer token is refused
    // before anything is stored or sent (DL-136).
    if let SessionCommand::Start { spec, .. } = &mut command
        && let Some(coordinator) = spec.coordinator.as_deref_mut()
    {
        if !owner {
            return Err((
                station.clone(),
                id.clone(),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "owner_only",
                    "only the owner's board login starts a coordinator session".to_owned(),
                ),
            ));
        }
        let Principal::Owner(session) = principal else {
            return Err((
                station.clone(),
                id.clone(),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "owner_only",
                    "only the owner's board login starts a coordinator session".to_owned(),
                ),
            ));
        };
        super::grant_routes::owner_start(
            board,
            &rec.station_id,
            &req.command_id,
            coordinator,
            (session, req.elevation.as_deref()),
        )
        .await
        .map_err(|e| (station.clone(), id.clone(), e))?;
    }
    if matches!(command, SessionCommand::ReconcileExecution { .. }) && !owner {
        return Err((
            station.clone(),
            id.clone(),
            ApiError::new(
                StatusCode::FORBIDDEN,
                "owner_only",
                "only the owner's board login marks a previous run as ended".to_owned(),
            ),
        ));
    }
    // A shell command runs code in the session folder (B8): only the owner's
    // board login sends one or releases a folder, and a station that has
    // shells off refuses the command itself.
    if matches!(
        command,
        SessionCommand::ShellCommand { .. } | SessionCommand::ReleaseShell { .. }
    ) && !owner
    {
        return Err((
            station.clone(),
            id.clone(),
            ApiError::new(
                StatusCode::FORBIDDEN,
                "owner_only",
                "only the owner's board login runs shell commands".to_owned(),
            ),
        ));
    }
    if matches!(command, SessionCommand::ShellCommand { .. }) {
        let shells_on = board
            .store
            .station_info(&rec.station_id)
            .is_some_and(|(info, _)| info.allow_shell == Some(true));
        if !shells_on {
            return Err((
                station.clone(),
                id.clone(),
                ApiError::new(
                    StatusCode::CONFLICT,
                    "shell_not_allowed",
                    "Shells are off on this station.".to_owned(),
                ),
            ));
        }
    }
    // Fleet Git (FG-1, design §2.1): only the deck says what a start does for
    // a sync project. A value from the caller is dropped; the deck attaches
    // its own for a start in the verified home of a sync project.
    if let SessionCommand::Start {
        spec,
        workspace,
        fleet_git,
        ..
    } = &mut command
    {
        *fleet_git = None;
        let git_kind = match workspace.as_deref() {
            None => Some(StartKind::InPlace),
            Some(WorkspaceRequest::Worktree { .. }) => Some(StartKind::Worktree),
            Some(_) => None,
        };
        if let Some(git_kind) = git_kind {
            *fleet_git =
                super::project_git_routes::attach_start(board, &rec, &spec.cwd, git_kind, None)
                    .await
                    .map_err(|e| (station.clone(), id.clone(), e))?;
        }
    }
    // An owner start without a coordinator request may get the default worker
    // grant (MCP P4a §3). Only the owner's login authorizes it; a bearer
    // token start keeps the plain path.
    let worker_eligible = owner
        && matches!(&command, SessionCommand::Start { spec, .. } if spec.coordinator.is_none());
    let submitted = if worker_eligible {
        board
            .link
            .submit_start_by(WorkerStart {
                station: rec.station_id,
                command,
                command_id: req.command_id,
                provenance: Provenance::Owner,
                author: Some(principal.actor()),
                setting: board.live.get().worker_grant.clone(),
            })
            .await
    } else {
        board
            .link
            .submit_by(
                rec.station_id,
                CommandOrigin::Board,
                CommandKind::Session { command },
                req.command_id,
                Some(principal.actor()),
            )
            .await
    };
    submitted.map_err(|e| {
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

/// Removes the author of every input and the `answered_by` of an answer of
/// a command that does not come from a member.
fn clear_author(command: &mut SessionCommand) {
    if let Some(input) = command.user_input_mut() {
        input.author = None;
    }
    if let SessionCommand::Answer { decision, .. } = command {
        decision.answered_by = None;
    }
}

/// The member policy of a command (R11.2, DL-166): a member sends a prompt, a
/// steer, a follow-up, an interrupt or an answer to a session that the share
/// covers, with the role that the action needs. Everything else is for the
/// owner. The deck sets the author of the input and of the answer.
async fn member_command(
    board: &Board,
    grant: &ShareGrant,
    station: &StationId,
    command: &mut SessionCommand,
) -> Result<(), ApiError> {
    let owner_only = || {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "only the owner's board login sends this command",
        )
    };
    let Some(session) = command.session_id().map(|s| s.as_str().to_owned()) else {
        return Err(owner_only());
    };
    let action = match command {
        SessionCommand::Prompt { .. } => MemberAction::Prompt,
        SessionCommand::Steer { .. } => MemberAction::Steer,
        SessionCommand::FollowUp { .. } => MemberAction::FollowUp,
        SessionCommand::Interrupt { .. } => MemberAction::Interrupt,
        SessionCommand::Answer { request_id, .. } => {
            // A question needs the collaborator role, a tool approval the
            // approver role. A request that the deck cannot find is a tool
            // approval (fail closed).
            let kind = board
                .store
                .request_kind(station.0.clone(), session.clone(), request_id.to_string())
                .await
                .map_err(ApiError::internal)?;
            if kind == Some(RequestKind::Question) {
                MemberAction::AnswerQuestion
            } else {
                MemberAction::AnswerApproval
            }
        }
        _ => return Err(owner_only()),
    };
    if !grant.scope.covers(station.as_str(), &session) {
        return Err(access::unknown_session(station.as_str(), &session));
    }
    if !member_allows(grant, action, station.as_str(), &session) {
        return Err(access::role_refused(
            if action == MemberAction::AnswerApproval {
                "answer a tool approval"
            } else {
                "send this command"
            },
        ));
    }
    access::ensure_driver(
        board,
        &Principal::Member(grant.member_id.clone(), Box::new(grant.clone())),
        station,
        &session,
        action,
    )
    .await?;
    if let Some(input) = command.user_input_mut() {
        input.author = Some(grant.actor());
    }
    if let SessionCommand::Answer { decision, .. } = command {
        decision.answered_by = Some(grant.actor());
    }
    Ok(())
}

/// `GET /api/commands/{command_id}`. A member reads its own commands only.
///
/// # Errors
/// 404 `unknown_command`.
pub async fn get_command(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(command_id): Path<String>,
) -> Result<Json<DeckCommand>, ApiError> {
    let found = board
        .store
        .command(CommandId::from(command_id))
        .await
        .map_err(ApiError::internal)?
        .filter(|c| {
            !matches!(principal, Principal::Member(..))
                || c.author.as_deref() == Some(principal.actor().as_str())
        });
    found
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_command", "no such command"))
}
