//! The people features of a shared session (R11.6, DL-154, DL-166): the side
//! chat (people only; the agent never reads it) and mob steering with
//! "request to drive". The owner and the members of a share use them; the
//! share decides which session and which role. Every change is audited with
//! the caller and sent to the open board sockets.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::board::{BoardServerMessage, SessionKey};
use fleetdeck_proto::share::{ChatMessage, ChatPost, DriveGrant, DriverState};
use serde::Deserialize;

use super::Board;
use super::access::session_of;
use super::fleet_routes::audited;
use super::guard::ApiError;
use crate::store::{DeckError, MemberAction, Principal};

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

/// Only the owner and the members: a bearer token is not a person.
fn person(principal: &Principal) -> Result<(), ApiError> {
    match principal {
        Principal::Owner(_) | Principal::Member(..) => Ok(()),
        // A display is read-only and the guard refuses it first (R13 §5).
        Principal::Token(..) | Principal::Display(_) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "only a person (the owner or a member) does this",
        )),
    }
}

fn key(station: &StationId, session: &str) -> SessionKey {
    SessionKey {
        station_id: station.clone(),
        session_id: session.into(),
    }
}

/// The query of `GET .../chat`.
#[derive(Debug, Default, Deserialize)]
pub struct ChatQuery {
    /// The last `seq` that the client holds.
    pub after: Option<u64>,
}

/// `GET /api/sessions/{station}/{session}/chat`: the newest messages of the
/// side chat after `after`, oldest first.
///
/// # Errors
/// 403 `owner_only` for a bearer token (NV-07); 404 `unknown_station` /
/// `unknown_session`.
pub async fn chat(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    Query(q): Query<ChatQuery>,
) -> Result<Json<Vec<ChatMessage>>, ApiError> {
    person(&principal)?;
    let station_id = session_of(
        &board,
        &principal,
        &station,
        &session,
        MemberAction::Chat,
        "read the chat",
    )
    .await?;
    let messages = board
        .store
        .chat(station_id.0, session, q.after.unwrap_or(0))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(messages))
}

/// `POST /api/sessions/{station}/{session}/chat { text }`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_text`; 403 `owner_only` for a bearer token
/// (NV-07); 404 `unknown_station` / `unknown_session`.
pub async fn post_chat(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<ChatPost>, JsonRejection>,
) -> Result<Json<ChatMessage>, ApiError> {
    let result = async {
        person(&principal)?;
        let Json(post) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        let station_id = session_of(
            &board,
            &principal,
            &station,
            &session,
            MemberAction::Chat,
            "write in the chat",
        )
        .await?;
        let message = board
            .store
            .post_chat(
                station_id.0.clone(),
                session.clone(),
                principal.actor(),
                &post.text,
            )
            .await
            .map_err(|e| match e {
                DeckError::Invalid(m) => bad("invalid_text", m),
                e => ApiError::internal(e),
            })?;
        board.store.publish_social(
            key(&station_id, &session),
            BoardServerMessage::Chat {
                station_id: station_id.clone(),
                session_id: session.as_str().into(),
                message: message.clone(),
            },
        );
        Ok(message)
    }
    .await;
    let target = format!("{station}/{session}");
    audited(&board, &principal, "chat.post", Some(&target), result)
        .await
        .map(Json)
}

/// Sends the driver and the requests of a session to the open sockets.
fn publish_drive(board: &Board, station: &StationId, session: &str, state: &DriverState) {
    let k = key(station, session);
    board.store.publish_social(
        k.clone(),
        BoardServerMessage::Driver {
            station_id: station.clone(),
            session_id: session.into(),
            driver: state.driver.clone(),
        },
    );
    board.store.publish_social(
        k,
        BoardServerMessage::DriveRequest {
            station_id: station.clone(),
            session_id: session.into(),
            requests: state.requests.clone(),
        },
    );
}

/// The ids of a session in mob mode, for a person who may drive.
async fn mob_session(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    what: &str,
) -> Result<StationId, ApiError> {
    person(principal)?;
    let station_id = session_of(
        board,
        principal,
        station,
        session,
        MemberAction::Drive,
        what,
    )
    .await?;
    let mob = board
        .store
        .session_is_mob(station_id.0.clone(), session.to_owned())
        .await
        .map_err(ApiError::internal)?;
    if !mob {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_mob",
            "this session is not in mob mode",
        ));
    }
    Ok(station_id)
}

/// True when the principal may hand the composer on: the owner, or the
/// current driver.
fn may_hand_on(principal: &Principal, state: &DriverState) -> bool {
    matches!(principal, Principal::Owner(_))
        || state
            .driver
            .as_deref()
            .is_some_and(|d| d == principal.actor())
}

/// `POST /api/sessions/{station}/{session}/drive/request`: ask to drive. When
/// nobody drives, the caller drives now.
///
/// # Errors
/// 403 `forbidden` (a viewer); 404; 409 `not_mob`.
pub async fn drive_request(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<DriverState>, ApiError> {
    let result = async {
        let station_id =
            mob_session(&board, &principal, &station, &session, "ask to drive").await?;
        let state = board
            .store
            .drive_request(station_id.0.clone(), session.clone(), principal.actor())
            .await
            .map_err(ApiError::internal)?;
        publish_drive(&board, &station_id, &session, &state);
        Ok(state)
    }
    .await;
    let target = format!("{station}/{session}");
    audited(&board, &principal, "drive.request", Some(&target), result)
        .await
        .map(Json)
}

/// `POST /api/sessions/{station}/{session}/drive/grant { to }`: the driver or
/// the owner hands the composer to `to` (`owner` or `member:<name>` of a
/// share that covers the session with the collaborator role or more).
///
/// # Errors
/// 400 `invalid_body` / `invalid_driver`; 403 `forbidden`; 404; 409 `not_mob`.
pub async fn drive_grant(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<DriveGrant>, JsonRejection>,
) -> Result<Json<DriverState>, ApiError> {
    let result = async {
        let Json(grant) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        let station_id = mob_session(
            &board,
            &principal,
            &station,
            &session,
            "hand on the composer",
        )
        .await?;
        let current = board
            .store
            .driver_state(station_id.0.clone(), session.clone())
            .await
            .map_err(ApiError::internal)?;
        if !may_hand_on(&principal, &current) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "only the driver or the owner hands on the composer",
            ));
        }
        let allowed = match grant.to.strip_prefix("member:") {
            _ if grant.to == "owner" => true,
            Some(name) => board
                .store
                .member_can_drive(station_id.0.clone(), session.clone(), name.to_owned())
                .await
                .map_err(ApiError::internal)?,
            None => false,
        };
        if !allowed {
            return Err(bad(
                "invalid_driver",
                "name `owner` or a member of this session with the collaborator role",
            ));
        }
        let state = board
            .store
            .drive_grant(station_id.0.clone(), session.clone(), grant.to)
            .await
            .map_err(ApiError::internal)?;
        publish_drive(&board, &station_id, &session, &state);
        Ok(state)
    }
    .await;
    let target = format!("{station}/{session}");
    audited(&board, &principal, "drive.grant", Some(&target), result)
        .await
        .map(Json)
}

/// `POST /api/sessions/{station}/{session}/drive/release`: the driver (or the
/// owner) lets go; nobody drives until the next request or grant.
///
/// # Errors
/// 403 `forbidden`; 404; 409 `not_mob`.
pub async fn drive_release(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<DriverState>, ApiError> {
    let result = async {
        let station_id = mob_session(
            &board,
            &principal,
            &station,
            &session,
            "let go of the composer",
        )
        .await?;
        let current = board
            .store
            .driver_state(station_id.0.clone(), session.clone())
            .await
            .map_err(ApiError::internal)?;
        if !may_hand_on(&principal, &current) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "only the driver or the owner lets go of the composer",
            ));
        }
        let state = board
            .store
            .drive_release(station_id.0.clone(), session.clone())
            .await
            .map_err(ApiError::internal)?;
        publish_drive(&board, &station_id, &session, &state);
        Ok(state)
    }
    .await;
    let target = format!("{station}/{session}");
    audited(&board, &principal, "drive.release", Some(&target), result)
        .await
        .map(Json)
}
