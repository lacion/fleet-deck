//! The views of the board at many sessions (R6, DL-169):
//! `POST /api/sessions/{station}/{session}/read` moves the read cursor of
//! the caller (R6.1), and `GET /api/activity` is the merged activity stream
//! (R6.5). Both need `read`.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::board::{ACTIVITY_DEFAULT_LIMIT, ACTIVITY_MAX_LIMIT, ActivityList, ReadMark};
use serde::Deserialize;

use super::Board;
use super::guard::ApiError;
use crate::store::Principal;
use crate::store::activity::ActivityQuery;
use crate::store::read_cursors::principal_key;

/// `POST /api/sessions/{station}/{session}/read`: the answer is the stored
/// cursor body (`{ "seq": n }`, never lower than an earlier one is kept).
///
/// # Errors
/// 400 `invalid_body`, 404 `unknown_station` or `unknown_session`.
pub async fn mark_read(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<ReadMark>, JsonRejection>,
) -> Result<Json<ReadMark>, ApiError> {
    let Ok(Json(mark)) = body else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "the body must be {\"seq\": a non-negative integer}",
        ));
    };
    if i64::try_from(mark.seq).is_err() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "seq is too large",
        ));
    }
    let record = board
        .store
        .find_station(station)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "unknown_station", "no such station")
        })?;
    let known = board
        .store
        .mark_read(
            principal_key(&principal),
            record.station_id,
            SessionId::from(session),
            mark.seq,
        )
        .await
        .map_err(ApiError::internal)?;
    if known {
        Ok(Json(mark))
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_session",
            "no such session",
        ))
    }
}

/// The query of `GET /api/activity`.
#[derive(Debug, Default, Deserialize)]
pub struct ActivityParams {
    pub since: Option<i64>,
    pub repo: Option<String>,
    pub session: Option<String>,
    pub limit: Option<u32>,
}

/// `GET /api/activity?since=&repo=&session=&limit=`.
///
/// # Errors
/// 400 `invalid_query` for a `limit` of 0 or above [`ACTIVITY_MAX_LIMIT`].
pub async fn activity(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ActivityParams>,
) -> Result<Json<ActivityList>, ApiError> {
    // A display gets its own projection from the P2b payload task (no mail,
    // no tool input, R13 §5D item 8). Until then it gets nothing.
    if matches!(principal, Principal::Display(_)) {
        return Err(super::not_built::not_built());
    }
    let limit = params.limit.unwrap_or(ACTIVITY_DEFAULT_LIMIT);
    if limit == 0 || limit > ACTIVITY_MAX_LIMIT {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            format!("limit must be 1 to {ACTIVITY_MAX_LIMIT}"),
        ));
    }
    Ok(Json(
        board
            .store
            .activity(ActivityQuery {
                since: params.since,
                repo: params.repo.filter(|r| !r.is_empty()),
                session: params.session.filter(|s| !s.is_empty()),
                limit,
            })
            .await
            .map_err(ApiError::internal)?,
    ))
}
