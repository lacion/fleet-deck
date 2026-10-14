//! The previous run of a session under a shim (DL-182, design
//! `docs/v2/design/shim-resume.md` §4.2): `GET
//! /api/sessions/{station}/{session}/execution` asks the station what it
//! sees of the unresolved execution (`session.execution_check`). Only the
//! owner's board login asks. The owner then marks the run as ended with the
//! command `session.reconcile_execution` (`command_routes.rs`).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::execution::{ExecutionCheck, ExecutionCheckQuery};
use fleetdeck_proto::files::code as files_code;
use fleetdeck_proto::link::Outcome;
use serde_json::Value;

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, NewAudit, Principal};

/// The longest the deck waits for the station.
const WAIT: Duration = Duration::from_secs(10);

const METHOD: &str = "session.execution_check";

fn unreachable_error(station: &StationId) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

/// One request to the station's live link.
async fn request(board: &Board, station: &StationId, params: Value) -> Result<Outcome, ApiError> {
    let control = board
        .link
        .control_for(station)
        .ok_or_else(|| unreachable_error(station))?;
    tokio::time::timeout(WAIT, control.request(METHOD, None, params))
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
    let found = board
        .store
        .find_station(station.clone())
        .await
        .map_err(ApiError::internal)?;
    let Some(rec) = found else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_station",
            format!("no station is named `{station}`"),
        ));
    };
    let target = format!("{}/{session}", rec.station_id.0);
    let result = ask(&board, &rec.station_id, &session).await;
    let row = match &result {
        Ok(_) => NewAudit::new(principal.actor(), METHOD, AuditResult::Ok).target(target),
        Err(e) => NewAudit::new(principal.actor(), METHOD, AuditResult::Failed)
            .target(target)
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
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
    match request(board, station, params).await? {
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
