//! The previous run of a session under a shim (DL-182, design
//! `docs/v2/design/shim-resume.md` §4.2): `GET
//! /api/sessions/{station}/{session}/execution` asks the station what it
//! sees of the unresolved execution (`session.execution_check`). Only the
//! owner's board login asks. The owner then marks the run as ended with the
//! command `session.reconcile_execution` (`command_routes.rs`).
//!
//! BG-4 (design `board-gaps.md` §5): `GET .../execution/stop-plan?shim_id=`
//! reads the stop plan (`session.execution_stop_plan`), and the audit rows of
//! `session.execution_stop` live here: the command writes `accepted` or
//! `refused` (`command_routes.rs`); a check that carries a `last_stop` writes
//! one `outcome` row for each command ID. A watcher ([`watch_outcome`]) asks
//! the station for that check after each accepted stop, so that the outcome
//! row does not depend on a browser that polls. A failed write of a row is
//! written again, for at most an hour ([`WRITE_RETRY_LIMIT`]).

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::board::CommandRequest;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::execution::{ExecutionCheck, ExecutionCheckQuery};
use fleetdeck_proto::execution_stop::{
    ExecutionStopOutcome, ExecutionStopPlan, ExecutionStopPlanQuery, StopOutcome, code as stop_code,
};
use fleetdeck_proto::files::code as files_code;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, StationId};
use serde::Deserialize;
use serde_json::Value;

use super::Board;
use super::guard::{ApiError, owner_only};
use super::settings_routes::is_elevated;
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal, StationRecord};

/// The longest the deck waits for the station.
const WAIT: Duration = Duration::from_secs(10);

const METHOD: &str = "session.execution_check";
const PLAN_METHOD: &str = "session.execution_stop_plan";

/// The audit action of a stop (design §5.6).
const STOP_ACTION: &str = "session.execution_stop";

/// The text of a station that cannot stop the processes of a lost run.
pub(super) const STOP_UNSUPPORTED_TEXT: &str =
    "This station's build cannot stop the processes of a lost run from the board.";

/// How many command IDs the deck remembers as audited; the set is cleared
/// when it is full.
const AUDITED_CAP: usize = 1_024;

/// How long the watcher keeps trying to write a row that failed.
const WRITE_RETRY_LIMIT: Duration = Duration::from_secs(3600);

/// The longest pause between two writes of a row that failed.
const WRITE_PAUSE_MAX: Duration = Duration::from_secs(60);

/// How the deck watches for the outcome of an accepted stop (BG-4 §5.6).
#[derive(Debug, Clone, Copy)]
pub struct StopWatch {
    /// The pause between two checks.
    pub poll: Duration,
    /// How long the watcher waits for the outcome.
    pub limit: Duration,
}

impl StopWatch {
    /// One check each second for 30 seconds.
    pub const DEFAULT: Self = Self {
        poll: Duration::from_secs(1),
        limit: Duration::from_secs(30),
    };
}

/// The IDs of the stop commands whose outcome row the deck wrote in this
/// process.
static AUDITED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn unreachable_error(station: &StationId) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

/// One request to the station's live link.
async fn request(
    board: &Board,
    station: &StationId,
    method: &str,
    params: Value,
) -> Result<Outcome, ApiError> {
    let control = board
        .link
        .control_for(station)
        .ok_or_else(|| unreachable_error(station))?;
    tokio::time::timeout(WAIT, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable_error(station))
}

/// The error of a station refusal.
fn refusal(code: &str, message: &str) -> ApiError {
    match code {
        files_code::UNKNOWN_SESSION => ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_session",
            "the station does not know this session",
        ),
        files_code::BUSY => ApiError::new(StatusCode::TOO_MANY_REQUESTS, "busy", message),
        files_code::TIMEOUT => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", message),
        "forbidden" => ApiError::new(StatusCode::FORBIDDEN, "forbidden", message),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            format!("{code}: {message}"),
        ),
    }
}

async fn find_station(board: &Board, station: &str) -> Result<StationRecord, ApiError> {
    board
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
        })
}

/// `GET /api/sessions/{station}/{session}/execution`.
///
/// # Errors
/// 403 `owner_only`, 404 `unknown_station` / `unknown_session`, 409
/// `station_unreachable`, 429 `busy`, 502 `station_error`, 504 `timeout`.
pub async fn check(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<ExecutionCheck>, ApiError> {
    owner_only(&principal, "checks a previous run")?;
    let rec = find_station(&board, &station).await?;
    let target = format!("{}/{session}", rec.station_id.0);
    let result = ask(&board, &rec.station_id, &session).await;
    let row = match &result {
        Ok(_) => NewAudit::new(principal.actor(), METHOD, AuditResult::Ok).target(target.clone()),
        Err(e) => NewAudit::new(principal.actor(), METHOD, AuditResult::Failed)
            .target(target.clone())
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    if let Ok(check) = &result {
        audit_outcome(&board, &principal.actor(), &target, check).await;
    }
    result.map(Json)
}

async fn ask(
    board: &Board,
    station: &StationId,
    session: &str,
) -> Result<ExecutionCheck, ApiError> {
    let params = serde_json::to_value(ExecutionCheckQuery {
        session_id: session.into(),
    })
    .map_err(ApiError::internal)?;
    match request(board, station, METHOD, params).await? {
        Outcome::Ok(value) => serde_json::from_value(value).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station sent an invalid execution check",
            )
        }),
        Outcome::Err(e) => Err(refusal(&e.code, &e.message)),
    }
}

fn stop_unsupported() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "unsupported",
        STOP_UNSUPPORTED_TEXT,
    )
}

/// The query of the plan route.
#[derive(Deserialize)]
pub struct PlanQuery {
    shim_id: String,
}

/// `GET /api/sessions/{station}/{session}/execution/stop-plan?shim_id=`:
/// link method `session.execution_stop_plan` (BG-4, design §5.2). The read
/// writes no audit row.
///
/// # Errors
/// 403 `owner_only`, 404 `unknown_station` / `unknown_session`, 409
/// `station_unreachable`, 429 `busy`, 501 `unsupported`, 502
/// `station_error`, 504 `timeout`.
pub async fn stop_plan(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    Query(query): Query<PlanQuery>,
) -> Result<Json<ExecutionStopPlan>, ApiError> {
    owner_only(&principal, "reads the stop plan of a previous run")?;
    let rec = find_station(&board, &station).await?;
    if board
        .store
        .link_feature(&rec.station_id, feature::EXECUTION_STOP)
        == LinkFeature::Without
    {
        return Err(stop_unsupported());
    }
    let params = serde_json::to_value(ExecutionStopPlanQuery {
        session_id: session.into(),
        shim_id: query.shim_id,
    })
    .map_err(ApiError::internal)?;
    match request(&board, &rec.station_id, PLAN_METHOD, params).await? {
        Outcome::Ok(value) => serde_json::from_value(value).map(Json).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station sent an invalid stop plan",
            )
        }),
        Outcome::Err(e) => Err(match e.code.as_str() {
            "unsupported" | "feature_not_negotiated" | "unknown_method" => stop_unsupported(),
            _ => refusal(&e.code, &e.message),
        }),
    }
}

/// The gates of the stop command, in this order: the owner's board login, a
/// station that did not refuse the feature (a down link passes: the link
/// layer fails as for any command), and a live elevation of the login.
///
/// # Errors
/// 403 `owner_only`, 501 `unsupported`, 400 `elevation_required`.
pub(super) fn stop_gates(
    board: &Board,
    principal: &Principal,
    station: &StationId,
    elevation: Option<&str>,
) -> Result<(), ApiError> {
    let Principal::Owner(session) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "only the owner's board login stops the processes of a previous run",
        ));
    };
    if board.store.link_feature(station, feature::EXECUTION_STOP) == LinkFeature::Without {
        return Err(stop_unsupported());
    }
    if !is_elevated(board, session, elevation) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            "This action needs the owner's elevation.",
        ));
    }
    Ok(())
}

/// What the audit of a stop command names: the session, the shim and the
/// command ID. Never a command name, an argument or `/proc` text.
#[derive(Clone)]
pub(super) struct StopAudit {
    session: String,
    shim: String,
    command: CommandId,
}

impl StopAudit {
    /// The audit subject of a `stop_execution` request; `None` for any other
    /// command.
    pub(super) fn of(req: &CommandRequest) -> Option<Self> {
        let SessionCommand::StopExecution {
            session_id,
            shim_id,
            ..
        } = &req.command
        else {
            return None;
        };
        Some(Self {
            session: session_id.as_str().to_owned(),
            shim: shim_id.clone(),
            command: req.command_id.clone(),
        })
    }

    fn row(&self, actor: &str, station: &str, result: AuditResult, detail: String) -> NewAudit {
        NewAudit::new(actor, STOP_ACTION, result)
            .target(format!("{station}/{}", self.session))
            .command(self.command.clone())
            .detail(detail)
    }

    /// `stage=accepted`: the deck accepted a new stop command.
    pub(super) async fn accepted(&self, board: &Board, actor: &str, station: &str) {
        let detail = format!("stage=accepted shim={}", self.shim);
        board
            .store
            .audit_or_log(self.row(actor, station, AuditResult::Ok, detail))
            .await;
    }

    /// `stage=refused code=<code>`: the deck refused the stop command.
    pub(super) async fn refused(&self, board: &Board, actor: &str, station: &str, code: &str) {
        let detail = format!("stage=refused code={code} shim={}", self.shim);
        board
            .store
            .audit_or_log(self.row(actor, station, AuditResult::Refused, detail))
            .await;
    }
}

/// True once for each command ID that the deck has not claimed yet.
fn claim(command_id: &str) -> bool {
    let mut seen = AUDITED.lock().unwrap_or_else(PoisonError::into_inner);
    if seen.contains(command_id) {
        return false;
    }
    if seen.len() >= AUDITED_CAP {
        seen.clear();
    }
    seen.insert(command_id.to_owned())
}

/// Gives a claimed command ID back, after a failed write.
fn release(command_id: &str) {
    AUDITED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(command_id);
}

/// A code of the station for the audit: one of the six, else a fixed word, so
/// that a station cannot put text into the audit.
fn known_code(code: &str) -> &str {
    if stop_code::text(code).is_some() {
        code
    } else {
        "unknown_code"
    }
}

/// The audit detail of a stop outcome.
fn outcome_detail(last: &ExecutionStopOutcome, shim: &str) -> String {
    format!(
        "stage=outcome outcome={} code={} term={} kill={} gone={} skipped={} shim={shim}",
        last.outcome.as_str(),
        last.code.as_deref().map_or("-", known_code),
        last.term_sent,
        last.kill_sent,
        last.gone,
        last.skipped,
    )
}

/// One `stage=outcome` row for the `last_stop` of a check, when the deck did
/// not write the row of its command ID yet in this process. A failed write
/// gives the ID back: a later check or the watcher can write the row again.
/// Returns `false` when the write failed; `true` when the check has no
/// `last_stop`, when another writer has the ID, or when the write succeeded.
async fn audit_outcome(board: &Board, actor: &str, target: &str, check: &ExecutionCheck) -> bool {
    let Some(last) = &check.last_stop else {
        return true;
    };
    if !claim(&last.command_id) {
        return true;
    }
    let result = match last.outcome {
        StopOutcome::Done => AuditResult::Ok,
        StopOutcome::Failed | StopOutcome::Unknown => AuditResult::Failed,
    };
    let shim = check.shim_id.as_deref().unwrap_or("-");
    let row = NewAudit::new(actor, STOP_ACTION, result)
        .target(target)
        .command(CommandId::from(last.command_id.as_str()))
        .detail(outcome_detail(last, shim));
    if let Err(e) = board.store.audit(row).await {
        tracing::warn!(error = %e, command = %last.command_id, "the stop outcome row was not written");
        release(&last.command_id);
        return false;
    }
    true
}

/// Runs `write` until it succeeds: after each failure it waits `poll`, then
/// twice as long each time, at most [`WRITE_PAUSE_MAX`], for at most
/// [`WRITE_RETRY_LIMIT`]. Logs an error when it gives up.
async fn until_written<F, Fut>(poll: Duration, what: &str, mut write: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let give_up = tokio::time::Instant::now() + WRITE_RETRY_LIMIT;
    let mut pause = poll;
    while !write().await {
        if tokio::time::Instant::now() >= give_up {
            tracing::error!(what, "the stop audit row was not written");
            return;
        }
        tokio::time::sleep(pause).await;
        pause = pause.saturating_mul(2).min(WRITE_PAUSE_MAX);
    }
}

/// Watches for the outcome of an accepted stop without a browser (BG-4
/// §5.6): asks the station for the execution check until its `last_stop` is
/// the outcome of this command, then writes the outcome row. When
/// `board.stop_watch.limit` passes first, it writes one `outcome_unobserved`
/// row; a later check can still write the real outcome. A failed write of
/// either row is written again, for at most an hour.
pub(super) fn watch_outcome(board: Arc<Board>, actor: String, station: StationId, stop: StopAudit) {
    tokio::spawn(async move {
        let watch = board.stop_watch;
        let until = tokio::time::Instant::now() + watch.limit;
        let target = format!("{}/{}", station.0, stop.session);
        while tokio::time::Instant::now() < until {
            if let Ok(check) = ask(&board, &station, &stop.session).await
                && check
                    .last_stop
                    .as_ref()
                    .is_some_and(|last| last.command_id == stop.command.as_str())
            {
                until_written(watch.poll, "outcome", || {
                    audit_outcome(&board, &actor, &target, &check)
                })
                .await;
                return;
            }
            tokio::time::sleep(watch.poll).await;
        }
        let detail = format!("stage=outcome_unobserved shim={}", stop.shim);
        let row = stop.row(&actor, &station.0, AuditResult::Failed, detail);
        until_written(watch.poll, "outcome_unobserved", || async {
            match board.store.audit(row.clone()).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "the stop audit row was not written");
                    false
                }
            }
        })
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::{claim, release};

    #[test]
    fn a_command_id_is_claimed_once_until_it_is_released() {
        assert!(claim("claim-test-1"));
        assert!(!claim("claim-test-1"));
        release("claim-test-1");
        assert!(claim("claim-test-1"));
        release("claim-test-1");
    }
}
