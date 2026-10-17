//! The owner's desktop relay routes (R13 §6D items 6 to 10):
//! - `GET /api/stations/{station}/desktop` (owner, scope Read): the info;
//! - `POST /api/stations/{station}/desktop/act` (owner, scope Drive,
//!   mutating, CSRF): the body is a
//!   [`fleetdeck_proto::desktop::DesktopActBody`] and the answer a
//!   [`fleetdeck_proto::desktop::DesktopActResult`].
//!
//! Both are owner only (a token, a member and a display are refused) and
//! need the deck setting `desktop_control`. The work and the audit rows are in
//! [`crate::desktop_relay`]. The answers use the fixed, redacted texts of
//! [`RelayError::message`]. A wall open or reload needs a board address that is
//! not loopback ([`pick_origin`]).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use fleetdeck_proto::StationId;
use fleetdeck_proto::desktop::{DesktopActBody, DesktopActResult, DesktopInfo};

use super::Board;
use super::guard::{ApiError, owner_only};
use super::settings_routes::owner;
use crate::desktop_relay::{self, RelayCtx, RelayError, is_loopback_origin};
use crate::store::{AuditResult, NewAudit, Principal};

/// The largest body of `POST .../desktop/act`.
pub const MAX_ACT_BODY: usize = 16 * 1024;

/// The code of the audit row of a request that stops before the relay.
const ACT_ACTION: &str = "desktop.act";

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route("/api/stations/{station}/desktop", get(show))
        .route(
            "/api/stations/{station}/desktop/act",
            post(act).layer(DefaultBodyLimit::max(MAX_ACT_BODY)),
        )
}

fn relay_error(e: RelayError) -> ApiError {
    let (status, code) = e.http();
    ApiError::new(status, code, e.message())
}

fn ctx(board: &Board) -> RelayCtx {
    RelayCtx {
        store: board.store.clone(),
        link: Arc::clone(&board.link),
        live: board.live.clone(),
    }
}

async fn station_id(board: &Board, name: &str) -> Result<StationId, ApiError> {
    board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .map(|rec| rec.station_id)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })
}

/// The board address for a wall pair page: the `Origin` of the request when
/// the board allows it and it is not loopback, else the first board address
/// that is not loopback (R13 §6D item 6).
fn pick_origin(
    request: Option<&str>,
    allows: impl Fn(&str) -> bool,
    endpoints: impl IntoIterator<Item = String>,
) -> Option<String> {
    request
        .filter(|o| allows(o) && !is_loopback_origin(o))
        .map(str::to_owned)
        .or_else(|| endpoints.into_iter().find(|o| !is_loopback_origin(o)))
}

fn board_origin(board: &Board, headers: &HeaderMap) -> Option<String> {
    let request = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    pick_origin(
        request,
        |o| board.origins.allows(o),
        board
            .endpoints
            .entries(board.store.now_ms())
            .into_iter()
            .map(|e| e.origin),
    )
}

/// `GET /api/stations/{station}/desktop`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; the codes of [`RelayError`].
async fn show(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
) -> Result<Json<DesktopInfo>, ApiError> {
    owner_only(&principal, "reads the desktop of a station")?;
    let id = station_id(&board, &station).await?;
    desktop_relay::info(&ctx(&board), id, false)
        .await
        .map(Json)
        .map_err(relay_error)
}

/// A request that stops before the relay: one audit row.
async fn refused(board: &Board, principal: &Principal, id: &str, e: ApiError) -> ApiError {
    board
        .store
        .audit_or_log(
            NewAudit::new(principal.actor(), ACT_ACTION, AuditResult::Refused)
                .target(id)
                .detail(e.code),
        )
        .await;
    e
}

/// `POST /api/stations/{station}/desktop/act`.
///
/// # Errors
/// 400 `invalid_body`; 403 `owner_only`; 404 `unknown_station`; 409
/// `desktop_no_endpoint`; the codes of [`RelayError`].
async fn act(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DesktopActBody>, JsonRejection>,
) -> Result<Json<DesktopActResult>, ApiError> {
    // The owner check comes before the body is read.
    owner(
        &board,
        &principal,
        ACT_ACTION,
        "",
        "controls the desktop of a station",
    )
    .await?;
    let id = match station_id(&board, &station).await {
        Ok(id) => id,
        Err(e) => return Err(refused(&board, &principal, "", e).await),
    };
    // A rejection text could quote the body.
    let Json(body) = match body {
        Ok(b) => b,
        Err(_) => {
            let e = ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the body is not a valid desktop action",
            );
            return Err(refused(&board, &principal, id.as_str(), e).await);
        }
    };
    let needs_origin = matches!(
        body,
        DesktopActBody::WallOpen { .. } | DesktopActBody::WallReload { .. }
    );
    let origin = if needs_origin {
        match board_origin(&board, &headers) {
            Some(o) => Some(o),
            None => {
                let e = ApiError::new(
                    StatusCode::CONFLICT,
                    "desktop_no_endpoint",
                    "The deck has no board address that the desktop can reach.",
                );
                return Err(refused(&board, &principal, id.as_str(), e).await);
            }
        }
    } else {
        None
    };
    desktop_relay::act_body(&ctx(&board), id, body, origin.as_deref())
        .await
        .map(Json)
        .map_err(relay_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<String> {
        vec![
            "https://localhost:7420".to_owned(),
            "https://deck.tail1.ts.net:7420".to_owned(),
            "https://100.64.0.1:7420".to_owned(),
        ]
    }

    #[test]
    fn the_request_origin_wins_when_allowed_and_not_loopback() {
        let allows = |o: &str| list().iter().any(|e| e == o);
        assert_eq!(
            pick_origin(Some("https://100.64.0.1:7420"), allows, list()).as_deref(),
            Some("https://100.64.0.1:7420")
        );
    }

    #[test]
    fn a_loopback_or_unknown_request_origin_falls_back_to_the_first_reachable_address() {
        let allows = |o: &str| list().iter().any(|e| e == o);
        for request in [
            Some("https://localhost:7420"),
            Some("https://evil.example"),
            None,
        ] {
            assert_eq!(
                pick_origin(request, allows, list()).as_deref(),
                Some("https://deck.tail1.ts.net:7420"),
                "{request:?}"
            );
        }
    }

    #[test]
    fn a_deck_with_loopback_addresses_only_has_no_endpoint() {
        let only = vec![
            "https://127.0.0.1:7420".to_owned(),
            "https://[::1]:7420".to_owned(),
        ];
        assert_eq!(
            pick_origin(Some("https://127.0.0.1:7420"), |_| true, only),
            None
        );
    }
}
