//! The automations of the board API (R5, DL-168). Every route needs the
//! `admin` scope and the owner's board login (DL-138): a schedule starts
//! sessions and a precheck runs shell code on a station.
//!
//! - `GET /api/schedules`: every schedule with its next run and last run.
//! - `POST /api/schedules`: creates one.
//! - `PUT /api/schedules/{id}`: replaces every field (`POST
//!   /api/schedules/{id}/update` is the same, for the board client that
//!   sends only GET and POST).
//! - `DELETE /api/schedules/{id}`: deletes it with its runs (`POST
//!   /api/schedules/{id}/delete` is the same).
//! - `POST /api/schedules/{id}/run`: "run now"; the answer is the claimed
//!   run (`started`); the work goes on in a task.
//! - `GET /api/schedules/{id}/runs`: the newest runs.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::automation::{MAX_RUNS, Schedule, ScheduleInput, ScheduleRun, ScheduleView};
use serde::Deserialize;

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::scheduler::Runner;
use crate::scheduler::cron::{self, Cron};
use crate::store::{AuditResult, DeckError, NewAudit, Principal};

const WHO: &str = "manages automations";

/// The author of a saved schedule (MCP P4a §3): the owner's login or a bearer
/// token; every other principal is `unknown` and gets no worker grant.
fn author_of(principal: &Principal) -> &'static str {
    match principal {
        Principal::Owner(_) => "owner",
        Principal::Token(..) => "token",
        _ => "unknown",
    }
}

fn unknown_schedule(id: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_schedule",
        format!("no schedule has the id `{id}`"),
    )
}

fn invalid(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

/// The next due time after `now` of an enabled schedule.
fn next_run(schedule: &Schedule, now: i64) -> Option<i64> {
    if !schedule.enabled {
        return None;
    }
    let cron = Cron::parse(&schedule.cron).ok()?;
    let offset = cron::parse_offset(&schedule.timezone).ok()?;
    cron.next_after(now, offset)
}

/// `GET /api/schedules`.
///
/// # Errors
/// 403 `owner_only`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<ScheduleView>>, ApiError> {
    owner_only(&principal, WHO)?;
    let now = board.store.now_ms();
    let schedules = board.store.schedules().await.map_err(ApiError::internal)?;
    let mut last = board
        .store
        .last_schedule_runs()
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(
        schedules
            .into_iter()
            .map(|s| ScheduleView {
                next_run_at: next_run(&s.schedule, now),
                last_run: last.remove(&s.schedule.schedule_id),
                schedule: s.schedule,
            })
            .collect(),
    ))
}

/// Checks a body and resolves its station: the canonical station id.
async fn checked(board: &Board, input: &ScheduleInput) -> Result<StationId, ApiError> {
    input.check().map_err(|m| invalid("invalid_schedule", m))?;
    Cron::parse(&input.cron).map_err(|m| invalid("invalid_cron", m))?;
    cron::parse_offset(&input.timezone).map_err(|m| invalid("invalid_timezone", m))?;
    let rec = board
        .store
        .find_station(input.station.trim().to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{}`", input.station.trim()),
            )
        })?;
    // A station that reports that it runs no setup commands cannot run a
    // precheck. A station whose report is not known (not linked since the
    // deck started) is accepted: the run then fails with the station's own
    // refusal.
    if input.precheck().is_some()
        && board
            .store
            .station_info(&rec.station_id)
            .is_some_and(|(info, _)| info.allow_setup_cmd != Some(true))
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "setup_not_supported",
            "this station does not run shell commands (allow_setup_cmd = false)",
        ));
    }
    Ok(rec.station_id)
}

async fn audited<T>(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    target: Option<&str>,
    result: &Result<T, ApiError>,
) {
    let mut row = match result {
        Ok(_) => NewAudit::new(principal.actor(), action, AuditResult::Ok),
        Err(e) => NewAudit::new(principal.actor(), action, AuditResult::Failed).detail(e.code),
    };
    if let Some(t) = target {
        row = row.target(t);
    }
    board.store.audit_or_log(row).await;
}

fn body_error(e: &JsonRejection) -> ApiError {
    invalid("invalid_body", e.body_text())
}

/// `POST /api/schedules`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_schedule` / `invalid_cron` /
/// `invalid_timezone`; 403 `owner_only`; 404 `unknown_station`; 409
/// `setup_not_supported`.
pub async fn create(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<ScheduleInput>, JsonRejection>,
) -> Result<Json<Schedule>, ApiError> {
    owner_only(&principal, WHO)?;
    let r = match body {
        Err(e) => Err(body_error(&e)),
        Ok(Json(input)) => match checked(&board, &input).await {
            Err(e) => Err(e),
            Ok(station) => board
                .store
                .create_schedule(input, station, author_of(&principal))
                .await
                .map_err(ApiError::internal),
        },
    };
    let target = r.as_ref().ok().map(|s| s.schedule_id.clone());
    audited(
        &board,
        &principal,
        "schedules.create",
        target.as_deref(),
        &r,
    )
    .await;
    r.map(Json)
}

/// `PUT /api/schedules/{id}` and `POST /api/schedules/{id}/update`.
///
/// # Errors
/// The errors of [`create`], and 404 `unknown_schedule`.
pub async fn update(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<ScheduleInput>, JsonRejection>,
) -> Result<Json<Schedule>, ApiError> {
    owner_only(&principal, WHO)?;
    let r = match body {
        Err(e) => Err(body_error(&e)),
        Ok(Json(input)) => match checked(&board, &input).await {
            Err(e) => Err(e),
            Ok(station) => board
                .store
                .update_schedule(id.clone(), input, station, author_of(&principal))
                .await
                .map_err(|e| match e {
                    DeckError::UnknownSchedule(id) => unknown_schedule(&id),
                    e => ApiError::internal(e),
                }),
        },
    };
    audited(&board, &principal, "schedules.update", Some(&id), &r).await;
    r.map(Json)
}

/// `DELETE /api/schedules/{id}` and `POST /api/schedules/{id}/delete`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_schedule`.
pub async fn delete(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owner_only(&principal, WHO)?;
    let r = match board.store.delete_schedule(id.clone()).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(unknown_schedule(&id)),
        Err(e) => Err(ApiError::internal(e)),
    };
    audited(&board, &principal, "schedules.delete", Some(&id), &r).await;
    r
}

/// `POST /api/schedules/{id}/run`: "run now".
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_schedule`; 409 `station_unreachable` (no
/// live link) or `run_exists` (the same millisecond).
pub async fn run(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<ScheduleRun>, ApiError> {
    owner_only(&principal, WHO)?;
    let r = run_now(&board, &principal, &id).await;
    audited(&board, &principal, "schedules.run", Some(&id), &r).await;
    r.map(Json)
}

async fn run_now(
    board: &Arc<Board>,
    principal: &Principal,
    id: &str,
) -> Result<ScheduleRun, ApiError> {
    let stored = board
        .store
        .schedule(id.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| unknown_schedule(id))?;
    let runner = Runner::new(board.store.clone(), Arc::clone(&board.link));
    match runner.run_now(stored.schedule, principal.actor()).await {
        Ok(Some(run)) => Ok(run),
        Ok(None) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "run_exists",
            "a run was claimed at the same moment; try again",
        )),
        Err(DeckError::Invalid(m)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            m,
        )),
        Err(e) => Err(ApiError::internal(e)),
    }
}

/// The query of `GET /api/schedules/{id}/runs`.
#[derive(Debug, Default, Deserialize)]
pub struct RunsQuery {
    pub limit: Option<u32>,
}

/// `GET /api/schedules/{id}/runs`: the newest runs, newest first (at most
/// [`MAX_RUNS`], default 30).
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_schedule`.
pub async fn runs(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Query(q): Query<RunsQuery>,
) -> Result<Json<Vec<ScheduleRun>>, ApiError> {
    owner_only(&principal, WHO)?;
    if board
        .store
        .schedule(id.clone())
        .await
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(unknown_schedule(&id));
    }
    let limit = q.limit.unwrap_or(30).clamp(1, MAX_RUNS);
    Ok(Json(
        board
            .store
            .schedule_runs(id, limit)
            .await
            .map_err(ApiError::internal)?,
    ))
}
