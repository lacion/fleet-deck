//! The owner routes of the wall remote (B17 §3.1.2, §3.1.3):
//! - `POST /api/displays/{id}/control` (owner, scope Drive, at most 512
//!   bytes) relays a scroll or a selection to the wall of a display;
//! - `POST /api/displays/{id}/remote/navigate` (owner, scope Drive) stores an
//!   owner directive and pushes a `navigate` frame;
//! - `GET /api/displays/{id}/remote?after=<seq>` (owner, scope Read) answers
//!   the view of a remote: the directive, the link, the checkpoint and the
//!   kept frames.
//!
//! No row is open to a display. The audit writes one `display.remote` row
//! for the start of a remote session, never one for each control.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use fleetdeck_proto::display_directive::MAX_DIRECTIVE_BODY;
use fleetdeck_proto::wall_control::{
    RemoteNavigate, WallControl, WallControlAck, WallControlBody, WallControlFrame, WallLink,
    WallRemoteView,
};
use serde::Deserialize;

use super::Board;
use super::account_routes::body_error;
use super::display_directive_routes::{owner_set, rejection};
use super::display_view::in_scope;
use super::guard::ApiError;
use super::settings_routes::owner;
use crate::coordinator_desktop::{effective_directive, effective_scope};
use crate::store::{AuditResult, NewAudit, Principal};
use crate::wall_control::{PushError, PushEvent};

const ACTION: &str = "display.remote";
/// The largest control body, in bytes.
const MAX_CONTROL_BODY: usize = 512;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route(
            "/api/displays/{id}/control",
            post(control).layer(DefaultBodyLimit::max(MAX_CONTROL_BODY)),
        )
        .route(
            "/api/displays/{id}/remote/navigate",
            post(navigate).layer(DefaultBodyLimit::max(MAX_DIRECTIVE_BODY)),
        )
        .route("/api/displays/{id}/remote", get(remote))
}

fn unknown_display() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "display_unknown", "no such display")
}

fn invalid_control() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_control",
        "The control is not valid.",
    )
}

fn too_many() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_controls",
        "Too many controls. Wait a moment.",
    )
}

fn ack(frame: &WallControlFrame) -> WallControlAck {
    WallControlAck {
        incarnation: frame.incarnation.clone(),
        seq: frame.seq,
        generation: frame.generation,
    }
}

/// The audit row of the start of a remote session (the first accepted
/// control or navigation after ten minutes without one).
async fn audit_start(board: &Board, id: &str, now: Instant) {
    if board.wall_controls.audit_due(id, now) {
        board
            .store
            .audit_or_log(
                NewAudit::new("owner", ACTION, AuditResult::Ok)
                    .target(id)
                    .detail("start"),
            )
            .await;
    }
}

/// A `select` names a session that exists and that the effective scope of
/// the display allows now. Any other control passes.
async fn selectable(board: &Board, id: &str, control: &WallControl) -> Result<(), ApiError> {
    let WallControl::Select { item: Some(item) } = control else {
        return Ok(());
    };
    let scope = effective_scope(&board.link, id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(unknown_display)?;
    let summary = board
        .store
        .session_summary(item.station_id.clone(), item.session_id.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(invalid_control)?;
    let root = summary.repo.as_ref().map(|r| r.root.as_str());
    if in_scope(&scope, &item.station_id, root) {
        Ok(())
    } else {
        Err(invalid_control())
    }
}

/// `POST /api/displays/{id}/control`.
///
/// # Errors
/// 400 `invalid_control`, 404 `display_unknown`, 409 `wall_offline`, 409
/// `wall_controls_unsupported`, 413 `too_large`, 429 `too_many_controls`.
async fn control(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<WallControlBody>, JsonRejection>,
) -> Result<Json<WallControlAck>, ApiError> {
    owner(&board, &principal, ACTION, &id, "sends a control").await?;
    let Json(WallControlBody { control }) = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            body_error(&e)
        } else {
            invalid_control()
        }
    })?;
    let view = effective_directive(&board.link, &id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(unknown_display)?;
    selectable(&board, &id, &control).await?;
    match board.wall_controls.link(&id) {
        WallLink::Offline => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "wall_offline",
                "The wall is not connected.",
            ));
        }
        WallLink::Older => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "wall_controls_unsupported",
                "This wall runs an older board. Reload the wall to use the remote.",
            ));
        }
        WallLink::Ready => {}
    }
    let now = board.wall_controls.now();
    let frame = board
        .wall_controls
        .push(&id, view.generation, PushEvent::Control(control), now)
        .map_err(|PushError::TooMany| too_many())?;
    audit_start(&board, &id, now).await;
    Ok(Json(ack(&frame)))
}

/// `POST /api/displays/{id}/remote/navigate`: an owner directive, then a
/// `navigate` frame. There is no link check: the directive is durable.
///
/// # Errors
/// The errors of the directive route, and 429 `too_many_controls`.
async fn navigate(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<RemoteNavigate>, JsonRejection>,
) -> Result<Json<WallControlAck>, ApiError> {
    let now = board.wall_controls.now();
    if !board.wall_controls.rate_ok(&id, now) {
        return Err(too_many());
    }
    let body = body.map(|Json(b)| b.directive).map_err(|e| rejection(&e));
    let stored = owner_set(&board, &principal, &id, body).await?;
    let answer = match board
        .wall_controls
        .push(&id, stored.generation, PushEvent::Navigate, now)
    {
        Ok(frame) => ack(&frame),
        Err(PushError::TooMany) => {
            // A race with another sender: the directive is stored, and the
            // wall reads it with its next poll.
            tracing::debug!(display = %id, "the navigate frame was not pushed: too many");
            let parts = board
                .wall_controls
                .view(&id, stored.generation, u64::MAX, now);
            WallControlAck {
                incarnation: parts.incarnation,
                seq: parts.seq,
                generation: stored.generation,
            }
        }
    };
    audit_start(&board, &id, now).await;
    Ok(Json(answer))
}

/// The query of `GET /api/displays/{id}/remote`.
#[derive(Debug, Default, Deserialize)]
struct RemoteQuery {
    #[serde(default)]
    after: u64,
}

/// `GET /api/displays/{id}/remote?after=<seq>`.
///
/// # Errors
/// 400 `invalid_query`, 404 `display_unknown`.
async fn remote(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    query: Result<Query<RemoteQuery>, QueryRejection>,
) -> Result<Json<WallRemoteView>, ApiError> {
    owner(&board, &principal, ACTION, &id, "reads the remote").await?;
    let Query(RemoteQuery { after }) = query.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "The query is not valid.",
        )
    })?;
    let directive = effective_directive(&board.link, &id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(unknown_display)?;
    let parts =
        board
            .wall_controls
            .view(&id, directive.generation, after, board.wall_controls.now());
    Ok(Json(WallRemoteView {
        directive,
        link: board.wall_controls.link(&id),
        incarnation: parts.incarnation,
        seq: parts.seq,
        state: parts.state,
        hold_left_ms: parts.hold_left_ms,
        complete: parts.complete,
        controls: parts.controls,
    }))
}
