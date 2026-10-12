//! The plan library of the board API (M3 6-6, DL-301): `GET /api/plans`
//! reads the library (newest first; `?archived=true` adds the archived
//! plans), `POST /api/plans/mark` sets the owner's status of one plan
//! (`drive`, audited).

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::plans::{DeckPlan, PlanMarkRequest};
use serde::Deserialize;

use super::Board;
use super::guard::ApiError;
use crate::store::{AuditResult, DeckError, NewAudit, Principal};

/// The query of `GET /api/plans`.
#[derive(Debug, Default, Deserialize)]
pub struct PlanQuery {
    #[serde(default)]
    pub archived: bool,
}

/// `GET /api/plans`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Query(q): Query<PlanQuery>,
) -> Result<Json<Vec<DeckPlan>>, ApiError> {
    Ok(Json(
        board
            .store
            .plans(q.archived)
            .await
            .map_err(ApiError::internal)?,
    ))
}

/// `POST /api/plans/mark`.
///
/// # Errors
/// 400 `invalid_body`, 404 `unknown_plan`.
pub async fn mark(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<PlanMarkRequest>, JsonRejection>,
) -> Result<Json<DeckPlan>, ApiError> {
    let r = match body {
        Err(_) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "the body must be {\"id\": string, \"status\": \"proposed\" | \"executed\" | \"archived\"}",
        )),
        Ok(Json(req)) => board
            .store
            .mark_plan(req.id, req.status)
            .await
            .map_err(|e| match e {
                DeckError::UnknownPlan(id) => ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_plan",
                    format!("unknown plan {id}"),
                ),
                e => ApiError::internal(e),
            }),
    };
    let row = match &r {
        Ok(plan) => NewAudit::new(principal.actor(), "plans.mark", AuditResult::Ok)
            .target(&plan.id)
            .detail(plan.status.as_str()),
        Err(e) => {
            NewAudit::new(principal.actor(), "plans.mark", AuditResult::Failed).detail(e.code)
        }
    };
    board.store.audit_or_log(row).await;
    r.map(Json)
}
