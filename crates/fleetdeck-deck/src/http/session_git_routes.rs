//! "Push now" of a session (design §5.8; FG-2). Scope `drive`, owner login or
//! a `drive` token, CSRF, audited `session.push_now`, closed to members (the
//! method table has no member column for it).
//!
//! `POST /api/sessions/{station}/{session}/push`: the deck forwards
//! `fleet_git.push_now { session_id }` to the station of the session and
//! returns its `PushNowAnswer`.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::{PushNowAnswer, PushNowParams, code};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;

use super::Board;
use super::guard::ApiError;
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal};

/// The deadline of the station's answer.
pub const PUSH_NOW_WAIT: Duration = Duration::from_secs(10);

fn unreachable() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        "the link to the station ended",
    )
}

/// The HTTP error for an error code of `fleet_git.push_now`.
fn station_error(error_code: &str, message: String) -> ApiError {
    match error_code {
        code::NOT_SYNCED => ApiError::new(
            StatusCode::CONFLICT,
            code::NOT_SYNCED,
            "This session is not in a synced project.",
        ),
        code::HELD => ApiError::new(
            StatusCode::CONFLICT,
            code::HELD,
            "A land or a move holds this session.",
        ),
        code::UNKNOWN_SESSION => ApiError::new(
            StatusCode::NOT_FOUND,
            code::UNKNOWN_SESSION,
            "The station does not know this session.",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            format!("{error_code}: {message}"),
        ),
    }
}

async fn push(board: &Board, station: &str, session: &str) -> Result<PushNowAnswer, ApiError> {
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
        .session_known(rec.station_id.0.clone(), session.to_owned())
        .await
        .map_err(ApiError::internal)?;
    if !known {
        return Err(super::access::unknown_session(station, session));
    }
    if board
        .store
        .link_feature(&rec.station_id, feature::FLEET_GIT)
        == LinkFeature::Without
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            code::FLEET_GIT_UNSUPPORTED,
            "This station does not support Fleet Git. Update it first.",
        ));
    }
    let control = board.link.control_for(&rec.station_id).ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            format!("station `{}` has no live link to this deck", rec.name),
        )
    })?;
    let params = serde_json::to_value(PushNowParams {
        session_id: SessionId::from(session.to_owned()),
    })
    .map_err(ApiError::internal)?;
    let outcome = tokio::time::timeout(
        PUSH_NOW_WAIT,
        control.request("fleet_git.push_now", None, params),
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
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(station_error(&e.code, e.message)),
    }
}

/// `POST /api/sessions/{station}/{session}/push`.
///
/// # Errors
/// 404 `unknown_station` / `unknown_session`; 409 `fleet_git_unsupported` /
/// `station_unreachable` / `not_synced` / `held`; 502 `station_error`; 504
/// `timeout`.
pub async fn push_now(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<PushNowAnswer>, ApiError> {
    let result = push(&board, &station, &session).await;
    let row = match &result {
        Ok(_) => NewAudit::new(principal.actor(), "session.push_now", AuditResult::Ok)
            .target(session.clone())
            .detail(station),
        Err(e) => NewAudit::new(principal.actor(), "session.push_now", AuditResult::Failed)
            .target(session.clone())
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(Json)
}
