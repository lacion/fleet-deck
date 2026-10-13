//! The archived sessions of the board API (DL-149): `GET /api/archive`
//! reads the set (`read`), `POST /api/archive` applies one change with a
//! `mutation_id` (`drive`, audited as `archive.change`).

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use fleetdeck_proto::board::{ArchiveChange, ArchiveList};

use super::Board;
use super::guard::ApiError;
use crate::store::{AuditResult, NewAudit, Principal};

/// `GET /api/archive`.
pub async fn list(State(board): State<Arc<Board>>) -> Result<Json<ArchiveList>, ApiError> {
    Ok(Json(
        board
            .store
            .archive_list()
            .await
            .map_err(ApiError::internal)?,
    ))
}

/// `POST /api/archive`.
///
/// # Errors
/// 400 `invalid_body`.
pub async fn change(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<ArchiveChange>, JsonRejection>,
) -> Result<Json<ArchiveList>, ApiError> {
    let invalid = |text: &str| ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", text);
    let r = match body {
        Err(_) => Err(invalid(
            "the body must be {\"mutation_id\": string, \"changes\": [{\"station_id\": string, \"session_id\": string, \"archived\": boolean}]}",
        )),
        Ok(Json(req)) => match req.validate() {
            Err(text) => Err(invalid(text)),
            Ok(()) => {
                let n = req.changes.len();
                board
                    .store
                    .apply_archive(req)
                    .await
                    .map(|(list, applied)| (list, n, applied))
                    .map_err(ApiError::internal)
            }
        },
    };
    let row = match &r {
        Ok((_, n, applied)) => {
            let detail = if *applied {
                format!("{n} changes")
            } else {
                format!("{n} changes, replayed")
            };
            NewAudit::new(principal.actor(), "archive.change", AuditResult::Ok).detail(detail)
        }
        Err(e) => {
            NewAudit::new(principal.actor(), "archive.change", AuditResult::Failed).detail(e.code)
        }
    };
    board.store.audit_or_log(row).await;
    r.map(|(list, _, _)| Json(list))
}
