//! The owner's artifact deletion (R13 P5a task C, A4 deck side,
//! `docs/v2/design/omarchy-p5a.md` §3.2 and §3.4).
//!
//! - `DELETE /api/artifacts/{station}/{id}` needs the owner's board login,
//!   `admin`, the CSRF token and a live elevation in the header
//!   `X-FleetDeck-Elevation`. One transaction makes the permanent tombstone
//!   and removes every deck copy; then one station call (`artifacts.delete`)
//!   asks the station to remove its copy. `200 {"state":"deleted"}` or `202
//!   {"state":"pending","reason":"offline"|"unsupported"|"retry"}`. The retry
//!   task (`crate::artifact_deletions`) finishes a pending deletion.
//! - `GET /api/artifacts/{station}/{id}/deletion` gives the same body, or 404
//!   `no_deletion`.
//!
//! The `DELETE` writes one audit row with the identity and the result code.
//! The `GET` and the retries write none.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use fleetdeck_proto::StationId;

use super::Board;
use super::artifact_routes::common::{db, not_found, station_of, unknown_artifact};
use super::guard::{ApiError, owner_only};
use super::settings_routes::{is_elevated, owner};
use crate::artifact_deletions::send_delete;
use crate::store::artifact_tombstones::{DeleteStart, TombstoneView, TryOutcome};
use crate::store::{AuditResult, NewAudit, Principal};

/// The header that carries the elevation token of the owner.
pub const ELEVATION_HEADER: &str = "x-fleetdeck-elevation";
/// The longest the handler waits for the station after the commit.
pub const HANDLER_DEADLINE: Duration = Duration::from_secs(10);

const ACTION: &str = "artifacts.delete";

/// The body and status of a tombstone state, and its audit code.
fn answer(view: &TombstoneView) -> (StatusCode, serde_json::Value, String) {
    if view.state == "deleted" {
        return (
            StatusCode::OK,
            serde_json::json!({ "state": "deleted" }),
            "deleted".to_owned(),
        );
    }
    let reason = view.reason.as_deref().unwrap_or("retry");
    (
        StatusCode::ACCEPTED,
        serde_json::json!({ "state": "pending", "reason": reason }),
        format!("pending:{reason}"),
    )
}

fn view_of(outcome: TryOutcome) -> TombstoneView {
    let (state, reason) = match outcome {
        TryOutcome::Deleted => ("deleted", None),
        TryOutcome::Offline => ("pending", Some("offline")),
        TryOutcome::Unsupported => ("pending", Some("unsupported")),
        TryOutcome::Retry => ("pending", Some("retry")),
    };
    TombstoneView {
        state: state.to_owned(),
        reason: reason.map(str::to_owned),
    }
}

fn elevation_required() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "elevation_required",
        "a recent sign-in is needed",
    )
}

/// `DELETE /api/artifacts/{station}/{id}` (owner only, elevated).
///
/// # Errors
/// 400 `elevation_required`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_artifact`.
pub async fn delete(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path((station, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let supplied = format!("{station}/{id}");
    let session = owner(&board, &principal, ACTION, &supplied, "deletes an artifact").await?;
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    // The target of the audit row is the stable identity once it is known.
    let mut target = supplied;
    let result = async {
        if !is_elevated(&board, &session, token) {
            return Err(elevation_required());
        }
        let rec = station_of(&board, &station).await?;
        target = format!("{}/{id}", rec.station_id);
        let view = match board
            .store
            .artifact_delete(rec.station_id.clone(), id.clone())
            .await
            .map_err(db)?
        {
            DeleteStart::Unknown => return Err(unknown_artifact()),
            DeleteStart::Existing(view) => view,
            DeleteStart::Created => finish(&board, &rec.station_id, &id).await?,
        };
        Ok(answer(&view))
    }
    .await;
    let row = match &result {
        Ok((_, _, code)) => NewAudit::new(principal.actor(), ACTION, AuditResult::Ok)
            .target(target)
            .detail(code.clone()),
        Err(e) => NewAudit::new(principal.actor(), ACTION, AuditResult::Refused)
            .target(target)
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    let (status, body, _) = result?;
    Ok((status, Json(body)).into_response())
}

/// The station try after the commit, and its record. A deadline or a failed
/// write leaves the deletion pending with the reason `retry`.
async fn finish(
    board: &Board,
    station: &StationId,
    artifact_id: &str,
) -> Result<TombstoneView, ApiError> {
    let outcome = tokio::time::timeout(
        HANDLER_DEADLINE,
        send_delete(&board.link, station, artifact_id),
    )
    .await
    .unwrap_or(TryOutcome::Retry);
    board
        .store
        .tombstone_set(
            station.clone(),
            artifact_id.to_owned(),
            outcome,
            board.store.now_ms(),
        )
        .await
        .map_err(db)?;
    Ok(view_of(outcome))
}

/// `GET /api/artifacts/{station}/{id}/deletion` (owner only).
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`, `no_deletion`.
pub async fn deletion(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    owner_only(&principal, "reads a deletion")?;
    let rec = station_of(&board, &station).await?;
    let view = board
        .store
        .tombstone(rec.station_id, id)
        .await
        .map_err(db)?
        .ok_or_else(|| not_found("no_deletion", "This artifact has no deletion."))?;
    let (status, body, _) = answer(&view);
    Ok((status, Json(body)).into_response())
}
