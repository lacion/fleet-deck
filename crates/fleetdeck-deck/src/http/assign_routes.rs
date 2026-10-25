//! `POST /api/assign` (M3 6-3, PLAN §7.3, DL-204): `assign auto: <task>`.
//! The deck picks the best session of the fleet ([`crate::routing`]) and
//! sends the task to it as `[FLEETDECK ASSIGNMENT] <task>`: a prompt to an
//! idle session, a follow-up to a busy one. When no session fits, the
//! answer is `unrouted` with the best station for a new session (the
//! station is connected, under `max_sessions`, has the provider, and has
//! the repository). The method needs the `drive` scope; every call is
//! audited as `session.assign`.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use fleetdeck_proto::board::{DeckCommand, DeckSession};
use fleetdeck_proto::command::{CommandOrigin, UserInput};
use fleetdeck_proto::fleet_git::Prefer;
use fleetdeck_proto::integrations::check_placement;
use fleetdeck_proto::link::{Outcome, WorkspaceQuery};
use fleetdeck_proto::orchestration::{
    ASSIGNMENT_PREFIX, AssignOutcome, AssignRequest, MAX_TASK_CHARS, SpawnHint,
};
use fleetdeck_proto::{CardStatus, SessionCommand, SessionId, StationId};
use serde_json::json;

use super::Board;
use super::command_routes::RECEIPT_WAIT;
use super::guard::ApiError;
use super::workspace_routes::WORKSPACE_WAIT;
use crate::link::CommandKind;
use crate::local::station_view;
use crate::routing::{Needs, pick_session};
use crate::routing_place::{PlaceNeeds, rank};
use crate::store::{AuditResult, DeckError, NewAudit, Principal};

/// The audit action and the API method name.
pub const METHOD: &str = "session.assign";

/// `POST /api/assign`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id` / `invalid_text`, 409
/// `not_synced` / `command_id_reused`.
pub async fn assign(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<AssignRequest>, JsonRejection>,
) -> Result<Json<AssignOutcome>, ApiError> {
    let audit = |result| NewAudit::new(principal.actor(), METHOD.to_owned(), result);
    let Json(req) =
        body.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", e.body_text()))?;
    if !fleetdeck_proto::board::valid_command_id(&req.command_id) {
        let e = ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_command_id",
            "a command id has 1 to 64 characters from [0-9A-Za-z_-]",
        );
        board
            .store
            .audit_or_log(audit(AuditResult::Refused).detail(e.code))
            .await;
        return Err(e);
    }
    let text = req.text.trim();
    if text.is_empty() || text.chars().count() > MAX_TASK_CHARS {
        let e = ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_text",
            format!("a task has 1 to {MAX_TASK_CHARS} characters"),
        );
        board
            .store
            .audit_or_log(
                audit(AuditResult::Refused)
                    .command(req.command_id.clone())
                    .detail(e.code),
            )
            .await;
        return Err(e);
    }
    // The placement fields of a new session (FG-6, decision D2).
    if let Err(why) = check_placement(&req.requires, &req.prefer) {
        let e = ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", why);
        board
            .store
            .audit_or_log(
                audit(AuditResult::Refused)
                    .command(req.command_id.clone())
                    .detail(e.code),
            )
            .await;
        return Err(e);
    }
    let sessions = board.store.sessions().await.map_err(ApiError::internal)?;
    // A resend after a lost response: the stored command, nothing new.
    if let Some(cmd) = board
        .store
        .command(req.command_id.clone())
        .await
        .map_err(ApiError::internal)?
    {
        return Ok(Json(assigned(&cmd, &sessions)));
    }
    let needs = Needs {
        repo: req.repo.as_deref(),
        provider: req.provider,
    };
    let buried = board
        .store
        .undelivered_mail()
        .await
        .map_err(ApiError::internal)?;
    let Some(target) = pick_session(&sessions, &needs, &buried) else {
        let spawn_on = spawn_on(&board, &needs, &req.requires, &req.prefer).await;
        let detail = spawn_on.as_ref().map_or_else(
            || "unrouted".to_owned(),
            |s| format!("unrouted; spawn on {}", s.name),
        );
        board
            .store
            .audit_or_log(
                audit(AuditResult::Ok)
                    .command(req.command_id.clone())
                    .detail(detail),
            )
            .await;
        return Ok(Json(AssignOutcome::Unrouted {
            reason: "no connected session fits the task (idle, queued or busy, in the \
                     repository, with the provider)"
                .to_owned(),
            spawn_on,
        }));
    };
    let session_id = target.summary.session_id.clone();
    let input = UserInput::new(format!("{ASSIGNMENT_PREFIX}{text}"));
    let command = if target.status == CardStatus::Idle {
        SessionCommand::Prompt { session_id, input }
    } else {
        SessionCommand::FollowUp { session_id, input }
    };
    let station = target.station_id.clone();
    let submitted = board
        .link
        .submit_as(
            station.clone(),
            CommandOrigin::Board,
            CommandKind::Session { command },
            req.command_id.clone(),
        )
        .await;
    let (cmd, _) = submitted.map_err(|e| match &e {
        DeckError::NotSynced(_) => ApiError::new(StatusCode::CONFLICT, "not_synced", e.to_string()),
        DeckError::CommandIdReused(_) => {
            ApiError::new(StatusCode::CONFLICT, "command_id_reused", e.to_string())
        }
        _ => ApiError::internal(&e),
    })?;
    board
        .store
        .audit_or_log(
            audit(AuditResult::Ok)
                .target(station.0.clone())
                .command(cmd.command_id.clone())
                .detail(format!("assigned to {}", target.summary.session_id.0)),
        )
        .await;
    let cmd = board
        .link
        .wait_command(cmd.command_id, RECEIPT_WAIT)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(assigned(&cmd, &sessions)))
}

/// The answer for a stored assignment command.
fn assigned(cmd: &DeckCommand, sessions: &[DeckSession]) -> AssignOutcome {
    let session_id = cmd.envelope["command"]["session_id"]
        .as_str()
        .map(|s| SessionId::from(s.to_owned()))
        .unwrap_or_else(|| SessionId::from(String::new()));
    let callsign = sessions
        .iter()
        .find(|s| s.station_id == cmd.station_id && s.summary.session_id == session_id)
        .and_then(|s| s.summary.callsign.clone());
    AssignOutcome::Assigned {
        station_id: cmd.station_id.clone(),
        session_id,
        callsign,
        command_id: cmd.command_id.clone(),
        state: cmd.state,
    }
}

/// The best station for a new session, in the placement order of FG-6
/// (`requires`, `prefer`, resources, live sessions, name); with a
/// repository, the first candidate whose `workspace.list` finds it.
async fn spawn_on(
    board: &Arc<Board>,
    needs: &Needs<'_>,
    requires: &[String],
    prefer: &[Prefer],
) -> Option<SpawnHint> {
    let records = board.store.stations().await.ok()?;
    let views: Vec<_> = records
        .iter()
        .map(|st| station_view(st, &board.store))
        .collect();
    let sessions = board.store.sessions().await.ok()?;
    let place = PlaceNeeds {
        requires,
        prefer,
        provider: needs.provider,
        homes: None,
    };
    for id in rank(&views, &sessions, &place) {
        if let Some(repo) = needs.repo
            && !repo_exists(board, &id, repo).await
        {
            continue;
        }
        let st = views.iter().find(|st| st.station_id == id)?;
        return Some(SpawnHint {
            station_id: id,
            name: st.name.clone(),
        });
    }
    None
}

async fn repo_exists(board: &Arc<Board>, station: &StationId, repo: &str) -> bool {
    let Some(control) = board.link.control_for(station) else {
        return false;
    };
    let params = json!(WorkspaceQuery {
        path: Some(repo.to_owned())
    });
    matches!(
        tokio::time::timeout(
            WORKSPACE_WAIT,
            control.request("workspace.list", None, params)
        )
        .await,
        Ok(Ok(Outcome::Ok(_)))
    )
}
