//! The directive routes of a wall display (R13 §5D items 3 to 5, R2):
//! - `POST /api/displays/{id}/directive` (owner, scope Drive, body
//!   [`DirectiveBody`], at most 8 KiB) validates and stores a directive;
//! - `GET /api/displays/self/directive` (a display login only) answers the
//!   [`DirectiveView`] that the wall polls. The deck checks the sender and
//!   every reference again on each answer.
//!
//! No URL, token or text of a person goes to an audit row: the detail holds
//! the mode and identifiers only.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveView, MAX_DIRECTIVE_BODY};

use super::Board;
use super::account_routes::body_error;
use super::guard::ApiError;
use super::settings_routes::owner;
use crate::coordinator_desktop::effective_directive;
use crate::store::display_directives::{DirectiveError, DirectiveSender};
use crate::store::{AuditResult, NewAudit, Principal};

const ACTION: &str = "display.directive";

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route(
            "/api/displays/{id}/directive",
            post(set).layer(DefaultBodyLimit::max(MAX_DIRECTIVE_BODY)),
        )
        .route("/api/displays/self/directive", get(read))
}

fn unknown_display() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "display_unknown", "no such display")
}

/// 413 for a body above the limit, else 400 `invalid_directive` with a fixed
/// text (the parser text can repeat the input).
pub(super) fn rejection(e: &JsonRejection) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return body_error(e);
    }
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_directive",
        "the directive is not valid",
    )
}

fn directive_error(e: DirectiveError) -> ApiError {
    match e {
        DirectiveError::Invalid(why) => {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_directive", why)
        }
        DirectiveError::OutsideCeiling => ApiError::new(
            StatusCode::FORBIDDEN,
            "outside_ceiling",
            "The directive points outside the ceiling of this display.",
        ),
        DirectiveError::NotGranted => ApiError::new(
            StatusCode::FORBIDDEN,
            "not_granted",
            "This sender may not send a directive.",
        ),
        DirectiveError::DisplayUnknown => unknown_display(),
        DirectiveError::Store(e) => ApiError::internal(e),
    }
}

/// The audit detail of an accepted directive: the mode and identifiers.
fn detail(body: &DirectiveBody, members: usize) -> String {
    let mut text = match body {
        DirectiveBody::Mission { .. } => "mode=mission".to_owned(),
        DirectiveBody::Ambient { .. } => "mode=ambient".to_owned(),
        DirectiveBody::Focus { session, .. } => format!(
            "mode=focus session={}/{}",
            session.station_id.as_str(),
            session.session_id.as_str()
        ),
        DirectiveBody::Review { session, .. } => format!(
            "mode=review session={}/{}",
            session.station_id.as_str(),
            session.session_id.as_str()
        ),
        DirectiveBody::Race { batch_id, .. } => {
            format!("mode=race batch={batch_id} members={members}")
        }
        DirectiveBody::Artifact { artifact, .. } => format!(
            "mode=artifact artifact={}/{}/v{}",
            artifact.station_id.as_str(),
            artifact.artifact_id,
            artifact.version
        ),
    };
    if let Some(filter) = body.filter() {
        text.push_str(&format!(" filter_stations={}", filter.stations.len()));
        if filter.repo.is_some() {
            text.push_str(" repo_filter=yes");
        }
    }
    text
}

/// One audit row of the owner: the target is the display id.
async fn record<T>(
    board: &Board,
    target: &str,
    detail: Option<String>,
    result: &Result<T, ApiError>,
) {
    let row = match (result, detail) {
        (Ok(_), Some(d)) => NewAudit::new("owner", ACTION, AuditResult::Ok).detail(d),
        (Ok(_), None) => NewAudit::new("owner", ACTION, AuditResult::Ok),
        (Err(e), _) => NewAudit::new("owner", ACTION, AuditResult::Failed).detail(e.code),
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// The owner sets a directive: the owner check, the checks of
/// `display_directive_set_counted`, and the `display.directive` audit row.
/// A body that did not parse is refused before the store and writes no row.
/// `POST /api/displays/{id}/directive` and the remote navigation share it.
///
/// # Errors
/// 400 `invalid_directive`, 403 `outside_ceiling`, 403 `not_granted`, 404
/// `display_unknown`, 413 `too_large`.
pub(super) async fn owner_set(
    board: &Board,
    principal: &Principal,
    id: &str,
    body: Result<DirectiveBody, ApiError>,
) -> Result<DirectiveView, ApiError> {
    owner(board, principal, ACTION, id, "sends a directive").await?;
    let body = body?;
    let outcome = board
        .store
        .display_directive_set_counted(id.to_owned(), DirectiveSender::Owner, body.clone())
        .await;
    let (result, text) = match outcome {
        Ok((view, members)) => (Ok(view), Some(detail(&body, members))),
        Err(e) => (Err(directive_error(e)), None),
    };
    record(board, id, text, &result).await;
    result
}

/// `POST /api/displays/{id}/directive`.
///
/// # Errors
/// See [`owner_set`].
async fn set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<DirectiveBody>, JsonRejection>,
) -> Result<Json<DirectiveView>, ApiError> {
    let body = body.map(|Json(b)| b).map_err(|e| rejection(&e));
    owner_set(&board, &principal, &id, body).await.map(Json)
}

/// `GET /api/displays/self/directive`.
///
/// # Errors
/// 403 `display_only`; 401 `login_required` for a revoked display.
async fn read(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<DirectiveView>, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    effective_directive(&board.link, &login.display_id)
        .await
        .map_err(ApiError::internal)?
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "login_required", "log in first"))
}
