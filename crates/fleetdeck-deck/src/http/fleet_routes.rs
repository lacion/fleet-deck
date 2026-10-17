//! Fleet mail (M3 6-2, DL-205) and tasks with waitable completions (6-4,
//! DL-206) on the board API. A mail or a task is deck-owned intent: the
//! deck stores it with its outbox item in one transaction, and the outbox
//! delivers the framed text once to the target session as a native
//! follow-up (or steer, for mail). Every mutating call is audited.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::board::valid_command_id;
use fleetdeck_proto::command::{CommandOrigin, UserInput};
use fleetdeck_proto::orchestration::{
    GroupMailRequest, MAX_RESULT_CHARS, MAX_TASK_CHARS, MAX_WAIT_IDS, MAX_WAIT_MS, MailDelivery,
    MailRequest, MailView, TaskCompletion, TaskRequest, TaskState, TaskView, TaskWaitAnswer,
    mail_frame, task_frame,
};
use fleetdeck_proto::{SessionCommand, SessionId, StationId};
use serde::Deserialize;

use super::Board;
use super::guard::ApiError;
use crate::link::CommandKind;
use crate::link::router::origin_name;
use crate::store::fleet::{group_mail_id, group_members};
use crate::store::{
    AuditResult, Completed, MemberAction, NewAudit, NewMail, NewOutboxItem, NewTask, Principal,
};

/// How long the outbox tries to deliver a mail or a task.
pub const DELIVERY_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// The rows of one list answer.
pub const LIST_PAGE: u32 = 200;
/// The longest `mail_id` of a group send (the mails add `-<n>`).
pub const MAX_GROUP_MAIL_ID: usize = 56;
/// The default wait of `GET /api/tasks/{task_id}/wait`.
pub const DEFAULT_WAIT_MS: u64 = 30_000;
const WAIT_TICK: Duration = Duration::from_millis(100);

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn check_id(id: &str) -> Result<(), ApiError> {
    if valid_command_id(&id.to_owned().into()) {
        Ok(())
    } else {
        Err(bad(
            "invalid_id",
            "an id has 1 to 64 characters from [0-9A-Za-z_-]",
        ))
    }
}

fn check_text(text: &str, max: usize) -> Result<(), ApiError> {
    if text.trim().is_empty() || text.chars().count() > max {
        return Err(bad(
            "invalid_text",
            format!("a text has 1 to {max} characters"),
        ));
    }
    Ok(())
}

/// The station of a known session of the read model.
async fn target(board: &Board, station: &str, session: &SessionId) -> Result<StationId, ApiError> {
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
    let known = board
        .store
        .sessions()
        .await
        .map_err(ApiError::internal)?
        .iter()
        .any(|s| s.station_id == rec.station_id && &s.summary.session_id == session);
    if !known {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_session",
            format!("station `{}` has no session `{}`", rec.name, session.0),
        ));
    }
    Ok(rec.station_id)
}

fn intent(
    board: &Board,
    station: StationId,
    command: SessionCommand,
) -> Result<NewOutboxItem, ApiError> {
    Ok(NewOutboxItem {
        station_id: station,
        origin: origin_name(CommandOrigin::Mail).to_owned(),
        payload: serde_json::to_value(CommandKind::Session { command })
            .map_err(ApiError::internal)?,
        expires_at: board.store.now_ms().saturating_add(DELIVERY_TTL_MS),
    })
}

pub(crate) async fn audited<T>(
    board: &Board,
    principal: &Principal,
    action: &str,
    target: Option<&str>,
    result: Result<T, ApiError>,
) -> Result<T, ApiError> {
    let row = NewAudit::new(
        principal.actor(),
        action.to_owned(),
        if result.is_ok() {
            AuditResult::Ok
        } else {
            AuditResult::Refused
        },
    );
    let row = match (&result, target) {
        (Err(e), Some(t)) => row.target(t.to_owned()).detail(e.code),
        (Err(e), None) => row.detail(e.code),
        (Ok(_), Some(t)) => row.target(t.to_owned()),
        (Ok(_), None) => row,
    };
    board.store.audit_or_log(row).await;
    result
}

/// `POST /api/mail`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_id` / `invalid_text`, 404
/// `unknown_station` / `unknown_session`.
pub async fn send_mail(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<MailRequest>, JsonRejection>,
) -> Result<Json<MailView>, ApiError> {
    let result = async {
        let Json(req) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        check_id(&req.mail_id)?;
        check_text(&req.text, MAX_TASK_CHARS)?;
        // A member mails the sessions of its share (collaborator role); in
        // mob mode only the driver does, as a mail steers the agent.
        let station = if matches!(principal, Principal::Member(..)) {
            let id = super::access::session_of(
                &board,
                &principal,
                &req.station,
                req.session_id.as_str(),
                MemberAction::Mail,
                "send mail",
            )
            .await?;
            super::access::ensure_driver(
                &board,
                &principal,
                &id,
                req.session_id.as_str(),
                MemberAction::Mail,
            )
            .await?;
            id
        } else {
            target(&board, &req.station, &req.session_id).await?
        };
        let sender = principal.actor();
        let delivery = req.delivery.unwrap_or(MailDelivery::FollowUp);
        let input = UserInput::new(mail_frame(&sender, req.text.trim()));
        let session_id = req.session_id.clone();
        let command = match delivery {
            MailDelivery::FollowUp => SessionCommand::FollowUp { session_id, input },
            MailDelivery::Steer => SessionCommand::Steer { session_id, input },
        };
        let item = intent(&board, station, command)?;
        let mail = NewMail {
            mail_id: req.mail_id,
            sender,
            session_id: req.session_id,
            delivery,
            text: req.text.trim().to_owned(),
            item,
        };
        let (view, _) = board
            .store
            .send_mail(mail)
            .await
            .map_err(ApiError::internal)?;
        Ok(view)
    }
    .await;
    let target = result.as_ref().ok().map(|m| m.station_id.0.clone());
    audited(&board, &principal, "mail.send", target.as_deref(), result)
        .await
        .map(Json)
}

/// `GET /api/mail`: the newest mails.
pub async fn mails(State(board): State<Arc<Board>>) -> Result<Json<Vec<MailView>>, ApiError> {
    Ok(Json(
        board
            .store
            .mails(LIST_PAGE)
            .await
            .map_err(ApiError::internal)?,
    ))
}

/// `POST /api/mail/group` (R2.5): one mail per live session of the group,
/// resolved now. A group without a live session sends nothing and answers
/// an empty list.
///
/// # Errors
/// 400 `invalid_body` / `invalid_id` / `invalid_text`.
pub async fn send_group_mail(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<GroupMailRequest>, JsonRejection>,
) -> Result<Json<Vec<MailView>>, ApiError> {
    let result = async {
        let Json(req) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        check_id(&req.mail_id)?;
        if req.mail_id.len() > MAX_GROUP_MAIL_ID {
            return Err(bad(
                "invalid_id",
                format!("a group mail id has at most {MAX_GROUP_MAIL_ID} characters"),
            ));
        }
        check_text(&req.text, MAX_TASK_CHARS)?;
        let sender = principal.actor();
        let delivery = req.delivery.unwrap_or(MailDelivery::FollowUp);
        let text = req.text.trim().to_owned();
        let sessions = board.store.sessions().await.map_err(ApiError::internal)?;
        let mut mails = Vec::new();
        for (i, member) in group_members(&sessions, req.group).into_iter().enumerate() {
            let session_id = member.summary.session_id.clone();
            let input = UserInput::new(mail_frame(&sender, &text));
            let command = match delivery {
                MailDelivery::FollowUp => SessionCommand::FollowUp {
                    session_id: session_id.clone(),
                    input,
                },
                MailDelivery::Steer => SessionCommand::Steer {
                    session_id: session_id.clone(),
                    input,
                },
            };
            mails.push(NewMail {
                mail_id: group_mail_id(&req.mail_id, i + 1),
                sender: sender.clone(),
                session_id,
                delivery,
                text: text.clone(),
                item: intent(&board, member.station_id.clone(), command)?,
            });
        }
        let (views, _) = board
            .store
            .send_group_mail(req.mail_id, mails)
            .await
            .map_err(ApiError::internal)?;
        Ok(views)
    }
    .await;
    let target = result.as_ref().ok().map(|m| m.len().to_string());
    audited(
        &board,
        &principal,
        "mail.send_group",
        target.as_deref(),
        result,
    )
    .await
    .map(Json)
}

/// `POST /api/tasks`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_id` / `invalid_text`, 404
/// `unknown_station` / `unknown_session`.
pub async fn create_task(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<TaskRequest>, JsonRejection>,
) -> Result<Json<TaskView>, ApiError> {
    let result = async {
        let Json(req) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        check_id(&req.task_id)?;
        check_text(&req.title, MAX_TASK_CHARS)?;
        let station = target(&board, &req.station, &req.session_id).await?;
        let command = SessionCommand::FollowUp {
            session_id: req.session_id.clone(),
            input: UserInput::new(task_frame(
                &principal.actor(),
                &req.task_id,
                req.title.trim(),
            )),
        };
        let item = intent(&board, station, command)?;
        let task = NewTask {
            task_id: req.task_id,
            title: req.title.trim().to_owned(),
            creator: principal.actor(),
            session_id: req.session_id,
            item,
        };
        let (view, _) = board
            .store
            .create_task(task)
            .await
            .map_err(ApiError::internal)?;
        Ok(view)
    }
    .await;
    let target = result.as_ref().ok().map(|t| t.task_id.clone());
    audited(
        &board,
        &principal,
        "tasks.create",
        target.as_deref(),
        result,
    )
    .await
    .map(Json)
}

/// `GET /api/tasks`: the newest tasks.
pub async fn tasks(State(board): State<Arc<Board>>) -> Result<Json<Vec<TaskView>>, ApiError> {
    Ok(Json(
        board
            .store
            .tasks(LIST_PAGE)
            .await
            .map_err(ApiError::internal)?,
    ))
}

/// `POST /api/tasks/{task_id}/complete`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_state` (`open`, or a reason or question
/// that does not fit the state) / `invalid_text`, 404 `unknown_task`, 409
/// `task_ended` (another completion came first).
pub async fn complete_task(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(task_id): Path<String>,
    body: Result<Json<TaskCompletion>, JsonRejection>,
) -> Result<Json<TaskView>, ApiError> {
    let result = async {
        let Json(req) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        let texts = [&req.result, &req.reason, &req.question];
        if texts
            .into_iter()
            .flatten()
            .any(|t| t.chars().count() > MAX_RESULT_CHARS)
        {
            return Err(bad(
                "invalid_text",
                format!("a text has at most {MAX_RESULT_CHARS} characters"),
            ));
        }
        req.check()
            .map_err(|message| bad("invalid_state", message))?;
        match board
            .store
            .complete_task(task_id.clone(), req)
            .await
            .map_err(ApiError::internal)?
        {
            None => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_task",
                format!("no task `{task_id}`"),
            )),
            Some(Completed::Ended(t)) => Ok(t),
            Some(Completed::Conflict(t)) => Err(ApiError::new(
                StatusCode::CONFLICT,
                "task_ended",
                format!("task `{}` already ended", t.task_id),
            )),
        }
    }
    .await;
    audited(&board, &principal, "tasks.complete", Some(&task_id), result)
        .await
        .map(Json)
}

#[derive(Debug, Deserialize)]
pub struct WaitParams {
    pub timeout_ms: Option<u64>,
}

/// `GET /api/tasks/{task_id}/wait?timeout_ms=`: the task once it is not
/// `open` any more (done, failed, blocked or in question), or at the
/// deadline (at most [`MAX_WAIT_MS`]) as it is.
///
/// # Errors
/// 404 `unknown_task`.
pub async fn wait_task(
    State(board): State<Arc<Board>>,
    Path(task_id): Path<String>,
    Query(params): Query<WaitParams>,
) -> Result<Json<TaskView>, ApiError> {
    let wait = params
        .timeout_ms
        .unwrap_or(DEFAULT_WAIT_MS)
        .min(MAX_WAIT_MS);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);
    loop {
        let task = board
            .store
            .task(task_id.clone())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_task",
                    format!("no task `{task_id}`"),
                )
            })?;
        if task.state != TaskState::Open || tokio::time::Instant::now() >= deadline {
            return Ok(Json(task));
        }
        tokio::time::sleep(WAIT_TICK.min(deadline - tokio::time::Instant::now())).await;
    }
}

#[derive(Debug, Deserialize)]
pub struct WaitAnyParams {
    /// The task ids, separated by commas.
    pub ids: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// `GET /api/tasks/wait?ids=a,b,c&timeout_ms=` (R2.4): the first of the
/// tasks (in the order of `ids`) that is not `open`, as soon as there is
/// one, or `{"task": null}` at the deadline (at most [`MAX_WAIT_MS`]).
///
/// # Errors
/// 400 `invalid_ids` (none, more than [`MAX_WAIT_IDS`], or a malformed
/// id), 404 `unknown_task`.
pub async fn wait_any_task(
    State(board): State<Arc<Board>>,
    Query(params): Query<WaitAnyParams>,
) -> Result<Json<TaskWaitAnswer>, ApiError> {
    let ids: Vec<String> = params
        .ids
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect();
    if ids.len() > MAX_WAIT_IDS || ids.iter().any(|id| check_id(id).is_err()) {
        return Err(bad(
            "invalid_ids",
            format!("`ids` has 1 to {MAX_WAIT_IDS} task ids, separated by commas"),
        ));
    }
    let wait = params
        .timeout_ms
        .unwrap_or(DEFAULT_WAIT_MS)
        .min(MAX_WAIT_MS);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);
    loop {
        let tasks = board
            .store
            .tasks_by_ids(ids.clone())
            .await
            .map_err(ApiError::internal)?
            .map_err(|id| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_task",
                    format!("no task `{id}`"),
                )
            })?;
        let ready = tasks.into_iter().find(|t| t.state != TaskState::Open);
        if ready.is_some() || tokio::time::Instant::now() >= deadline {
            return Ok(Json(TaskWaitAnswer { task: ready }));
        }
        tokio::time::sleep(WAIT_TICK.min(deadline - tokio::time::Instant::now())).await;
    }
}
