//! The owner's routes of the shared fleet (R11, DL-154, DL-166): shares with
//! their invites, and spaces. Every route needs the owner's board login: no
//! token and no member makes a share. The invite token goes to the answer
//! once; neither the token nor the URL goes to a log or to the audit row.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use fleetdeck_proto::share::{
    NewShare, NewSpace, ShareCreated, ShareHistory, ShareInfo, ShareKind, SpaceSession, SpaceView,
    Steering,
};

use super::Board;
use super::fleet_routes::audited;
use super::guard::{ApiError, owner_only};
use super::settings_routes::is_elevated;
use crate::store::shares::NewShareRow;
use crate::store::{DeckError, Principal};

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn not_found(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, message)
}

fn deck_error(e: DeckError) -> ApiError {
    match e {
        DeckError::Invalid(message) => bad("invalid_share", message),
        e => ApiError::internal(e),
    }
}

/// Sharing the history is a danger change (DL-189): the owner's login must
/// hold a live elevation.
fn require_elevation(
    board: &Board,
    principal: &Principal,
    elevation: Option<&str>,
) -> Result<(), ApiError> {
    let Principal::Owner(session) = principal else {
        return owner_only(principal, "shares the history");
    };
    if is_elevated(board, session, elevation) {
        return Ok(());
    }
    let mut keys = serde_json::Map::new();
    keys.insert("keys".to_owned(), serde_json::json!(["history"]));
    Err(bad(
        "elevation_required",
        "a recent sign-in is needed to share the history",
    )
    .with_details(keys))
}

/// The target of a share, checked and written with ids: a session as
/// `<station_id>/<session_id>`, a space as its id, the board as empty.
async fn checked_target(board: &Board, new: &NewShare) -> Result<String, ApiError> {
    match new.kind {
        ShareKind::Board => {
            if new.target.is_empty() {
                Ok(String::new())
            } else {
                Err(bad("invalid_target", "a board share has no target"))
            }
        }
        ShareKind::Space => {
            let known = board
                .store
                .space_exists(new.target.clone())
                .await
                .map_err(ApiError::internal)?;
            if known {
                Ok(new.target.clone())
            } else {
                Err(not_found("unknown_space", "no such space"))
            }
        }
        ShareKind::Session => {
            let Some((station, session)) = new.target.split_once('/') else {
                return Err(bad(
                    "invalid_target",
                    "a session share names `<station>/<session id>`",
                ));
            };
            let rec = board
                .store
                .find_station(station.to_owned())
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| not_found("unknown_station", "no such station"))?;
            let known = board
                .store
                .session_known(rec.station_id.0.clone(), session.to_owned())
                .await
                .map_err(ApiError::internal)?;
            if !known {
                return Err(not_found("unknown_session", "no such session"));
            }
            Ok(format!("{}/{session}", rec.station_id))
        }
    }
}

/// The board origin that the invite URL names: the `Origin` of the owner's
/// own request (the guard checked it), else the first origin of the deck.
fn board_origin(board: &Board, headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .filter(|o| board.origins.allows(o))
        .map(str::to_owned)
        .or_else(|| board.origins.list().into_iter().next())
}

/// `POST /api/shares`: a share with a one-time invite URL.
///
/// # Errors
/// 403 `owner_only`; 400 `invalid_body` / `invalid_target` /
/// `invalid_share` / `elevation_required`; 404 `unknown_station` / `unknown_session` /
/// `unknown_space`.
pub async fn create_share(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Result<Json<NewShare>, JsonRejection>,
) -> Result<Json<ShareCreated>, ApiError> {
    let result = async {
        owner_only(&principal, "makes a share")?;
        let Json(new) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        let history = new.history.unwrap_or(false);
        if history {
            require_elevation(&board, &principal, new.elevation.as_deref())?;
        }
        let target = checked_target(&board, &new).await?;
        let origin = board_origin(&board, &headers)
            .ok_or_else(|| ApiError::internal("the deck has no board origin"))?;
        let created = board
            .store
            .create_share(NewShareRow {
                kind: new.kind,
                target,
                role: new.role,
                steering: new.steering.unwrap_or(Steering::Open),
                expires_in_days: new.expires_in_days,
                uses: new.uses,
                history,
            })
            .await
            .map_err(deck_error)?;
        Ok(ShareCreated {
            url: format!("{origin}/#join={}", created.token.expose()),
            share_id: created.share_id,
            expires_at: created.expires_at,
        })
    }
    .await;
    let target = result.as_ref().ok().map(|c| c.share_id.clone());
    audited(
        &board,
        &principal,
        "shares.create",
        target.as_deref(),
        result,
    )
    .await
    .map(Json)
}

/// `GET /api/shares`.
///
/// # Errors
/// 403 `owner_only`.
pub async fn list_shares(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<ShareInfo>>, ApiError> {
    owner_only(&principal, "lists the shares")?;
    Ok(Json(
        board.store.shares().await.map_err(ApiError::internal)?,
    ))
}

/// `POST /api/shares/{share_id}/revoke`: ends the share, its invite and
/// every member login of it at once.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_share`.
pub async fn revoke_share(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(share_id): Path<String>,
) -> Result<Json<ShareInfo>, ApiError> {
    let result = async {
        owner_only(&principal, "revokes a share")?;
        // Wait for the member mail that is being sent now (NV-17).
        let _fence = crate::link::router::member_revocation_fence().await;
        let view = board
            .store
            .revoke_share(share_id.clone())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| not_found("unknown_share", "no such share"))?;
        // Every open board socket of a member checks its login now.
        board.store.board_changed();
        Ok(view)
    }
    .await;
    audited(&board, &principal, "shares.revoke", Some(&share_id), result)
        .await
        .map(Json)
}

/// `POST /api/shares/{share_id}/history`: turns the history option of a
/// share on or off (DL-189). Turning it on needs the owner's elevation. It
/// takes effect at the next read: the member grant is loaded on every
/// request.
///
/// # Errors
/// 403 `owner_only`; 400 `invalid_body` / `elevation_required`; 404
/// `unknown_share`.
pub async fn set_history(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(share_id): Path<String>,
    body: Result<Json<ShareHistory>, JsonRejection>,
) -> Result<Json<ShareInfo>, ApiError> {
    let action = match &body {
        Ok(Json(b)) if b.enabled => "shares.history_on",
        Ok(_) => "shares.history_off",
        Err(_) => "shares.history",
    };
    let result = async {
        owner_only(&principal, "changes the history option of a share")?;
        let Json(req) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        if req.enabled {
            require_elevation(&board, &principal, req.elevation.as_deref())?;
        }
        board
            .store
            .set_share_history(share_id.clone(), req.enabled)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| not_found("unknown_share", "no such share"))
    }
    .await;
    audited(&board, &principal, action, Some(&share_id), result)
        .await
        .map(Json)
}

/// `POST /api/spaces`.
///
/// # Errors
/// 403 `owner_only`; 400 `invalid_body` / `invalid_space`.
pub async fn create_space(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<NewSpace>, JsonRejection>,
) -> Result<Json<SpaceView>, ApiError> {
    let result = async {
        owner_only(&principal, "makes a space")?;
        let Json(new) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        board
            .store
            .create_space(&new.name)
            .await
            .map_err(|e| match e {
                DeckError::Invalid(message) => bad("invalid_space", message),
                e => ApiError::internal(e),
            })
    }
    .await;
    let target = result.as_ref().ok().map(|s| s.space_id.clone());
    audited(
        &board,
        &principal,
        "spaces.create",
        target.as_deref(),
        result,
    )
    .await
    .map(Json)
}

/// `GET /api/spaces`.
///
/// # Errors
/// 403 `owner_only`.
pub async fn list_spaces(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<SpaceView>>, ApiError> {
    owner_only(&principal, "lists the spaces")?;
    Ok(Json(
        board.store.spaces().await.map_err(ApiError::internal)?,
    ))
}

/// `POST /api/spaces/{space_id}/sessions`: adds a session to a space. The
/// members of a share of the space see it at once.
///
/// # Errors
/// 403 `owner_only`; 400 `invalid_body`; 404 `unknown_station` /
/// `unknown_session` / `unknown_space`.
pub async fn add_space_session(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(space_id): Path<String>,
    body: Result<Json<SpaceSession>, JsonRejection>,
) -> Result<Json<SpaceView>, ApiError> {
    let result = async {
        owner_only(&principal, "changes a space")?;
        let Json(req) = body.map_err(|e| bad("invalid_body", e.body_text()))?;
        let rec = board
            .store
            .find_station(req.station.clone())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| not_found("unknown_station", "no such station"))?;
        let known = board
            .store
            .session_known(rec.station_id.0.clone(), req.session_id.0.clone())
            .await
            .map_err(ApiError::internal)?;
        if !known {
            return Err(not_found("unknown_session", "no such session"));
        }
        let view = board
            .store
            .add_space_session(space_id.clone(), rec.station_id, req.session_id)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| not_found("unknown_space", "no such space"))?;
        board.store.board_changed();
        Ok(view)
    }
    .await;
    audited(
        &board,
        &principal,
        "spaces.add_session",
        Some(&space_id),
        result,
    )
    .await
    .map(Json)
}
