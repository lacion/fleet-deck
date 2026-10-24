//! Who may do what to which session (R11, DL-166): the one place that turns a
//! station name and a session id of a request into ids that the principal
//! may use. A member sees the sessions of its share only: a session outside
//! the share is as unknown as a session that does not exist.

use axum::http::StatusCode;
use fleetdeck_proto::StationId;

use super::Board;
use super::guard::ApiError;
use crate::store::{MemberAction, Principal, member_allows};

/// The 404 of a session that does not exist or that a member cannot see.
#[must_use]
pub fn unknown_session(station: &str, session: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_session",
        format!("no session `{session}` on `{station}`"),
    )
}

/// The 403 of a member whose role does not hold the action.
#[must_use]
pub fn role_refused(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        format!("the role of this share cannot {what}"),
    )
}

/// The ids of a session of a request. For the owner and a token the session
/// must exist. For a member the share must cover it and its role must hold
/// `action`.
///
/// # Errors
/// 404 `unknown_station` (owner, token) / `unknown_session`, 403
/// `forbidden` (a member's role), 500 `internal`.
pub async fn session_of(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    action: MemberAction,
    what: &str,
) -> Result<StationId, ApiError> {
    let found = board
        .store
        .find_station(station.to_owned())
        .await
        .map_err(ApiError::internal)?;
    let Some(rec) = found else {
        return Err(if matches!(principal, Principal::Member(..)) {
            unknown_session(station, session)
        } else {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
            )
        });
    };
    if let Principal::Member(_, grant) = principal {
        if !grant.scope.covers(rec.station_id.as_str(), session) {
            return Err(unknown_session(station, session));
        }
        if !member_allows(grant, action, rec.station_id.as_str(), session) {
            return Err(role_refused(what));
        }
    }
    let known = board
        .store
        .session_known(rec.station_id.0.clone(), session.to_owned())
        .await
        .map_err(ApiError::internal)?;
    if !known {
        return Err(unknown_session(station, session));
    }
    Ok(rec.station_id)
}

/// In mob mode only the current driver steers (the owner always may). A
/// member who is not the driver is refused with 403 `not_driver`.
///
/// # Errors
/// 403 `not_driver`, 500 `internal`.
pub async fn ensure_driver(
    board: &Board,
    principal: &Principal,
    station: &StationId,
    session: &str,
    action: MemberAction,
) -> Result<(), ApiError> {
    let Principal::Member(_, grant) = principal else {
        return Ok(());
    };
    if !action.steers() {
        return Ok(());
    }
    let mob = grant.steering == fleetdeck_proto::share::Steering::Mob
        || board
            .store
            .session_is_mob(station.0.clone(), session.to_owned())
            .await
            .map_err(ApiError::internal)?;
    if !mob {
        return Ok(());
    }
    let state = board
        .store
        .driver_state(station.0.clone(), session.to_owned())
        .await
        .map_err(ApiError::internal)?;
    if state.driver.as_deref() == Some(grant.actor().as_str()) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_driver",
            "this session is in mob mode: ask to drive first",
        ))
    }
}
