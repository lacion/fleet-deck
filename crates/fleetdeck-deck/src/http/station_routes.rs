//! The pairing methods of the board API (M2 P4-2): the board's "Add
//! station" dialog and the remove button use the same [`DeckNode`] calls as
//! `fleetdeck deck invite`, `deck add-station` and `deck remove-station`.
//! All three need the `admin` scope and are audited. An invitation is a
//! secret: no audit row, log line or error holds its text.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_core::Secret;
use fleetdeck_proto::board::{AddStationRequest, PairingInvitation};

use super::Board;
use super::guard::ApiError;
use crate::link::{DeckNode, NodeError};
use crate::local::{StationView, station_view};
use crate::store::{AuditResult, NewAudit, Principal};

/// The longest invitation text that `POST /api/stations` reads.
pub const MAX_INVITATION_CHARS: usize = 4096;

/// The command that a station runs to use a deck invitation (it reads the
/// text from a prompt, G12).
pub const JOIN_COMMAND: &str = "fleetdeck station join";

fn node(board: &Board) -> Result<&Arc<DeckNode>, ApiError> {
    board.pairing.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "pairing_unavailable",
            "this deck process cannot pair stations",
        )
    })
}

/// The HTTP form of a pairing error. The messages never hold the
/// invitation text.
fn refusal(e: &NodeError) -> ApiError {
    let (status, code) = match e {
        NodeError::NoListener => (StatusCode::CONFLICT, "no_listener"),
        NodeError::Invitation(_) => (StatusCode::BAD_REQUEST, "invalid_invitation"),
        NodeError::NotAStation => (StatusCode::BAD_REQUEST, "not_a_station"),
        NodeError::NameTaken(_) => (StatusCode::CONFLICT, "name_taken"),
        NodeError::UnknownStation(_) => (StatusCode::NOT_FOUND, "unknown_station"),
        NodeError::Pairing(_) => (StatusCode::BAD_GATEWAY, "pairing_failed"),
        NodeError::Revoke(_) => (StatusCode::INTERNAL_SERVER_ERROR, "revoke_failed"),
        NodeError::Listen { .. } | NodeError::Store(_) => return ApiError::internal(e),
    };
    ApiError::new(status, code, e.to_string())
}

async fn audit(
    board: &Board,
    principal: &Principal,
    method: &str,
    target: &str,
    err: Option<&ApiError>,
) {
    let row = match err {
        None => NewAudit::new(principal.actor(), method, AuditResult::Ok),
        Some(e) => NewAudit::new(principal.actor(), method, AuditResult::Failed).detail(e.code),
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// `POST /api/stations/invite`: a new invitation for a station that dials
/// the deck.
///
/// # Errors
/// 409 `no_listener` without `link_listen`; 503 `pairing_unavailable`.
pub async fn invite(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<PairingInvitation>, ApiError> {
    let ttl = board.store.knobs().invite;
    let r = owner_only(&principal)
        .and_then(|()| node(&board))
        .and_then(|n| n.invite(ttl).map_err(|e| refusal(&e)));
    audit(&board, &principal, "stations.invite", "", r.as_ref().err()).await;
    let text = r?;
    let expires_at = board
        .store
        .now_ms()
        .saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX));
    Ok(Json(PairingInvitation {
        invitation: text.expose().clone(),
        expires_at,
        join_command: JOIN_COMMAND.to_owned(),
    }))
}

/// Only the owner pairs, configures or removes stations (PLAN §9 "Only the
/// owner configures targets", DL-138).
fn owner_only(principal: &Principal) -> Result<(), ApiError> {
    super::guard::owner_only(principal, "pairs, adds or removes stations")
}

/// `POST /api/stations`: pairs with a listening station from its
/// invitation and dials it.
///
/// # Errors
/// 400 `invalid_body` / `invalid_invitation` / `not_a_station`, 409
/// `name_taken`, 502 `pairing_failed`, 503 `pairing_unavailable`.
pub async fn add(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<AddStationRequest>, JsonRejection>,
) -> Result<Json<StationView>, ApiError> {
    // The owner check comes before the body (and its invitation) is read.
    let r = match owner_only(&principal) {
        Ok(()) => add_station(&board, body).await,
        Err(e) => Err(e),
    };
    let target = r.as_ref().map(|v| v.name.clone()).unwrap_or_default();
    audit(
        &board,
        &principal,
        "stations.add",
        &target,
        r.as_ref().err(),
    )
    .await;
    r.map(Json)
}

async fn add_station(
    board: &Board,
    body: Result<Json<AddStationRequest>, JsonRejection>,
) -> Result<StationView, ApiError> {
    let node = node(board)?;
    // A rejection text could quote the body; it holds the secret.
    let Json(req) = body.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "the body must be {\"invitation\": string, \"name\": string | null}",
        )
    })?;
    let text = req.invitation.trim();
    if text.is_empty() || text.chars().count() > MAX_INVITATION_CHARS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_invitation",
            format!("an invitation has 1 to {MAX_INVITATION_CHARS} characters"),
        ));
    }
    let name = req
        .name
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    let st = node
        .add_station(&Secret::new(text.to_owned()), name)
        .await
        .map_err(|e| refusal(&e))?;
    Ok(station_view(&st, &board.store))
}

/// `POST /api/stations/{station}/remove`: unpairs a station by name,
/// closes its link and drops its cards.
///
/// # Errors
/// 404 `unknown_station`, 500 `revoke_failed` when the revocation was not
/// stored (NV-15), 503 `pairing_unavailable`.
pub async fn remove(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
) -> Result<Json<StationView>, ApiError> {
    let r = match owner_only(&principal).and_then(|()| node(&board)) {
        Ok(n) => n
            .remove_station(&station)
            .await
            .map(|st| station_view(&st, &board.store))
            .map_err(|e| refusal(&e)),
        Err(e) => Err(e),
    };
    audit(
        &board,
        &principal,
        "stations.remove",
        &station,
        r.as_ref().err(),
    )
    .await;
    r.map(Json)
}
