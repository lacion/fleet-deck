//! The earlier history of a session (DL-189, `docs/v2/design/session-history.md`
//! 3.6): `GET /api/sessions/{station}/{session}/history?before=&limit=`.
//!
//! The owner's board login reads it. A member reads it only through its own
//! share, and only when the owner turned on the `history` option of that
//! share. A bearer token never reads it. The deck forwards the query to the
//! station's `session.history` method and returns the answer. It stores and
//! caches nothing. The station empties the tool summaries of a member read.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::history::{MAX_LIMIT, SessionHistory, SessionHistoryQuery, code};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde::Deserialize;

use super::Board;
use super::access::{self, session_of};
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, LinkFeature, MemberAction, NewAudit, Principal};

/// The longest the deck waits for the station's answer. The station's own
/// budget is 10 s.
const HISTORY_WAIT: Duration = Duration::from_secs(15);
/// The longest session id of a request, in characters.
const MAX_SESSION_CHARS: usize = 128;
/// The longest cursor of a request, in characters.
const MAX_CURSOR_CHARS: usize = 64;
/// The audit action of this route.
const ACTION: &str = "sessions.history";

/// The query of the route (DL-189).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryParams {
    before: Option<String>,
    limit: Option<u32>,
}

fn bad(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", message)
}

fn unreachable_station(station: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

/// True when `session` is short and holds no control character.
fn plain_session(session: &str) -> bool {
    session.chars().count() <= MAX_SESSION_CHARS && !session.chars().any(char::is_control)
}

/// Checks the query. Returns the cursor and the page size.
fn checked_query(
    session: &str,
    query: Result<Query<HistoryParams>, QueryRejection>,
) -> Result<(Option<String>, Option<u32>), ApiError> {
    let Query(p) = query.map_err(|_| bad("the query is not valid"))?;
    if !plain_session(session) {
        return Err(bad("the session id is not valid"));
    }
    if let Some(before) = &p.before
        && (before.chars().count() > MAX_CURSOR_CHARS
            || !before.chars().all(|c| c.is_ascii_graphic()))
    {
        return Err(bad("the cursor is not valid"));
    }
    if let Some(limit) = p.limit
        && !(1..=MAX_LIMIT).contains(&limit)
    {
        return Err(bad("the limit is 1 to 100"));
    }
    Ok((p.before, p.limit))
}

/// Maps a station error code to a fixed HTTP error. The text of the station
/// never goes to the answer.
fn station_error(code_text: &str, station: &str, session: &str) -> ApiError {
    match code_text {
        code::UNKNOWN_SESSION => access::unknown_session(station, session),
        code::INVALID_PARAMS => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_params",
            "the station refused the query",
        ),
        code::STALE_CURSOR => ApiError::new(
            StatusCode::CONFLICT,
            "stale_cursor",
            "The history changed. Load it again from the start.",
        ),
        code::BUSY => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "The station is busy. Try again in a moment.",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "the station could not read the history",
        ),
    }
}

/// The checked read: the guards of the route, then the forward.
async fn read(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    query: Result<Query<HistoryParams>, QueryRejection>,
) -> Result<SessionHistory, ApiError> {
    if matches!(principal, Principal::Token(..)) {
        owner_only(principal, "reads the session history")?;
    }
    let (before, limit) = checked_query(session, query)?;
    let station_id = session_of(
        board,
        principal,
        station,
        session,
        MemberAction::Read,
        "read the history",
    )
    .await?;
    if let Principal::Member(_, grant) = principal
        && !grant.history
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "history_not_shared",
            "The owner did not share the earlier history of this session.",
        ));
    }
    match board.store.link_feature(&station_id, feature::HISTORY) {
        LinkFeature::Down => return Err(unreachable_station(station)),
        LinkFeature::Without => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "history_unsupported",
                "This station runs an older Fleet Deck. Update it to see the earlier history.",
            ));
        }
        LinkFeature::Negotiated => {}
    }
    let control = board
        .link
        .control_for(&station_id)
        .ok_or_else(|| unreachable_station(station))?;
    let params = serde_json::to_value(SessionHistoryQuery {
        session_id: SessionId::from(session),
        before,
        limit,
        member: matches!(principal, Principal::Member(..)),
    })
    .map_err(ApiError::internal)?;
    let outcome = tokio::time::timeout(
        HISTORY_WAIT,
        control.request("session.history", None, params),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the station did not answer in time",
        )
    })?
    .map_err(|_| unreachable_station(station))?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|e| {
            tracing::warn!(error = %e, "the station sent a history answer that does not parse");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station sent an answer that the deck cannot read",
            )
        }),
        Outcome::Err(e) => {
            if !matches!(
                e.code.as_str(),
                code::UNKNOWN_SESSION | code::INVALID_PARAMS | code::STALE_CURSOR | code::BUSY
            ) {
                tracing::warn!(code = %e.code, "the station failed a history read");
            }
            Err(station_error(&e.code, station, session))
        }
    }
}

/// Writes the audit row of one call: a row for every failure and for every
/// member read. The detail is the error code or the share id, never a text
/// of the station.
async fn audit(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    error: Option<&ApiError>,
) {
    let actor = principal.actor();
    let row = match (error, principal) {
        (Some(e), _) => {
            let kind = if matches!(
                e.status,
                StatusCode::BAD_REQUEST
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
                    | StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::PAYLOAD_TOO_LARGE
            ) {
                AuditResult::Refused
            } else {
                AuditResult::Failed
            };
            NewAudit::new(actor, ACTION, kind).detail(e.code)
        }
        (None, Principal::Member(_, grant)) => NewAudit::new(actor, ACTION, AuditResult::Ok)
            .detail(format!("share:{}", grant.share_id)),
        (None, _) => return,
    };
    // The names come from the path: keep them out of the log when they are
    // not plain.
    let target = if plain_session(station) && plain_session(session) {
        format!("{station}/{session}")
    } else {
        String::new()
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// `GET /api/sessions/{station}/{session}/history?before=&limit=`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`, `forbidden`,
/// `history_not_shared`; 404 `unknown_station`, `unknown_session`; 409
/// `station_unreachable`, `history_unsupported`, `stale_cursor`; 502
/// `station_error`; 503 `busy`; 504 `timeout`.
pub async fn history(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    query: Result<Query<HistoryParams>, QueryRejection>,
) -> Result<Json<SessionHistory>, ApiError> {
    let result = read(&board, &principal, &station, &session, query).await;
    audit(
        &board,
        &principal,
        &station,
        &session,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}
