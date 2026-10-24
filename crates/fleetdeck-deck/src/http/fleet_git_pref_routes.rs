//! The Fleet Git preferences of the deck (FG-11, design §14.3, decision
//! "Recorded for T2.3b"):
//!
//! - `GET /api/fleet-git/prefs` (`fleet_git.prefs`, scope `read`):
//!   `{ "drift_reference": "<station id>" | null }`. The answer is `null`
//!   when no station has the stored id (the row stays).
//! - `PUT /api/fleet-git/drift-reference { station }`
//!   (`fleet_git.drift_reference`, owner's login, CSRF, audited
//!   `drift.reference`): `null` clears; an unknown station is 404
//!   `unknown_station`. The answer is the GET shape.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::guard::{ApiError, owner_only};
use super::projects_routes::bad_body;
use crate::store::{AuditResult, NewAudit, Principal};

const ACTION: &str = "drift.reference";

/// The body of the PUT.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceBody {
    pub station: Option<String>,
}

/// The stored reference when a station has that id, else `None`.
async fn current(board: &Board) -> Result<Option<String>, ApiError> {
    let Some(id) = board
        .store
        .fleet_git_drift_reference()
        .await
        .map_err(ApiError::internal)?
    else {
        return Ok(None);
    };
    let known = board
        .store
        .stations()
        .await
        .map_err(ApiError::internal)?
        .iter()
        .any(|st| st.station_id.as_str() == id);
    Ok(known.then_some(id))
}

/// `GET /api/fleet-git/prefs`.
///
/// # Errors
/// 500 on a store error.
pub async fn prefs(State(board): State<Arc<Board>>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "drift_reference": current(&board).await? })))
}

async fn set_inner(
    board: &Board,
    body: Result<Json<ReferenceBody>, JsonRejection>,
) -> (String, Result<(), ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return ("none".to_owned(), Err(bad_body(&e))),
    };
    let target = body.station.clone().unwrap_or_else(|| "none".to_owned());
    let run = async {
        if let Some(id) = &body.station {
            let known = board
                .store
                .stations()
                .await
                .map_err(ApiError::internal)?
                .iter()
                .any(|st| st.station_id.as_str() == id);
            if !known {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_station",
                    "Unknown station.",
                ));
            }
        }
        board
            .store
            .set_fleet_git_drift_reference(body.station.clone())
            .await
            .map_err(ApiError::internal)
    };
    (target, run.await)
}

/// `PUT /api/fleet-git/drift-reference`.
///
/// # Errors
/// 400 `invalid_body`; 403 `owner_only`; 404 `unknown_station`.
pub async fn set_reference(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<ReferenceBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    owner_only(&principal, "changes the config reference")?;
    let (target, result) = set_inner(&board, body).await;
    let row = match &result {
        Ok(()) => NewAudit::new(principal.actor(), ACTION, AuditResult::Ok).target(target),
        Err(e) => NewAudit::new(principal.actor(), ACTION, AuditResult::Refused)
            .target(target)
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result?;
    Ok(Json(json!({ "drift_reference": current(&board).await? })))
}
