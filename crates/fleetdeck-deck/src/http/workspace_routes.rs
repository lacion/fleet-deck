//! `GET /api/stations/{station}/workspaces?path=` (M2 P4-4, DL-061): the
//! directory picker of the spawn form. The deck forwards the query to the
//! station's `workspace.list` method on a live link and returns the answer.
//! The station decides what is inside its allowed roots; the deck keeps no
//! copy. The method needs the `drive` scope and does not mutate.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::link::{Outcome, Preflight, PreflightQuery, WorkspaceListing, WorkspaceQuery};
use serde::Deserialize;
use serde_json::json;

use super::Board;
use super::guard::ApiError;

/// The longest the board waits for the station's answer.
pub const WORKSPACE_WAIT: Duration = Duration::from_secs(8);
/// The longest the board waits for the station's Git access check (the
/// station's own limit is 20 s).
pub const PREFLIGHT_WAIT: Duration = Duration::from_secs(30);
/// The longest path that the route forwards.
pub const MAX_PATH_CHARS: usize = 4096;

#[derive(Debug, Deserialize)]
pub struct WorkspaceParams {
    pub path: Option<String>,
}

/// # Errors
/// 400 `invalid_path`, `missing_workspace`, `outside_roots`; 404
/// `unknown_station`; 409 `station_unreachable`; 502 `station_error`;
/// 504 `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Path(station): Path<String>,
    Query(params): Query<WorkspaceParams>,
) -> Result<Json<WorkspaceListing>, ApiError> {
    let path = params.path.filter(|p| !p.is_empty());
    if path
        .as_ref()
        .is_some_and(|p| p.chars().count() > MAX_PATH_CHARS || p.contains('\0'))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            format!("a path has at most {MAX_PATH_CHARS} characters and no NUL"),
        ));
    }
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
    let params = json!(WorkspaceQuery { path });
    let outcome = tokio::time::timeout(
        WORKSPACE_WAIT,
        control.request("workspace.list", None, params),
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
            "missing_workspace" => {
                ApiError::new(StatusCode::BAD_REQUEST, "missing_workspace", e.message)
            }
            "outside_roots" => ApiError::new(StatusCode::BAD_REQUEST, "outside_roots", e.message),
            "timeout" => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", e.message),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{}: {}", e.code, e.message),
            ),
        }),
    }
}

/// `POST /api/stations/{station}/workspace/preflight {url}` (DL-144): the
/// Git access check of a clone URL, forwarded to the station's
/// `workspace.preflight` method. The station decides what a URL may be; a
/// failed check is a normal answer with `ok: false`.
///
/// # Errors
/// 400 `invalid_url`; 403 `forbidden`; 404 `unknown_station`; 409
/// `station_unreachable`; 500 `internal`; 502 `station_error` (also a
/// station without `workspace.preflight.v1`); 504 `timeout`.
pub async fn preflight(
    State(board): State<Arc<Board>>,
    Path(station): Path<String>,
    Json(query): Json<PreflightQuery>,
) -> Result<Json<Preflight>, ApiError> {
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
        PREFLIGHT_WAIT,
        control.request("workspace.preflight", None, json!(query)),
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
            "invalid_url" => ApiError::new(StatusCode::BAD_REQUEST, "invalid_url", e.message),
            "forbidden" => ApiError::new(StatusCode::FORBIDDEN, "forbidden", e.message),
            "internal" => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.message),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{}: {}", e.code, e.message),
            ),
        }),
    }
}
