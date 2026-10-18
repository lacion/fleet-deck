//! The wall display routes (R13 §5, §5A, §5B, §5C, DL-201): pairing by an
//! on-screen code, the one-time display link, the owner's Displays API and
//! `GET /api/displays/self`.
//!
//! - The pairing start, the pairing poll and the link redemption need no
//!   login. The poll uses the `fd_display_pending` cookie only.
//! - The owner routes need the owner's board login; the ones that give
//!   access also need a live elevation (DL-172).
//! - No code, token, cookie or secret goes to an audit row or a log line.
//!   The code is in the owner's list only.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fleetdeck_core::Secret;
use fleetdeck_proto::StationId;
use fleetdeck_proto::display::{
    DisplayBinding, DisplayLinkMade, DisplayList, DisplaySelf, DisplayView, PairingPoll,
    PairingStarted, PairingState,
};
use serde::{Deserialize, Deserializer};
use serde_json::json;

use super::Board;
use super::account_routes::body_error;
use super::admission::{PeerAddr, request_source, too_many_requests};
use super::auth_routes::{Audit, limit_refusal, named_cookie, start_attempt};
use super::display_bodies::{BindingBody, parse_pair_body};
use super::guard::{ApiError, DISPLAY_COOKIE, PENDING_COOKIE, cookie, owner_only};
use super::settings_routes::{is_elevated, owner};
use crate::board_fence;
use crate::store::auth::LoginSession;
use crate::store::displays::{
    DISPLAY_MAX_MS, DisplayChange, NewDisplay, check_quiet_hours, clean_name,
};
use crate::store::displays_binding::{BindError, check_monitor};
use crate::store::displays_pairing::{Approval, Claim};
use crate::store::{AuditResult, DeckError, NewAudit, Principal};

/// The `Max-Age` of the pending cookie: the pending time and a margin.
const PENDING_COOKIE_AGE_S: i64 = 420;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route("/api/displays", get(list))
        .route("/api/displays/pair", post(pair_start))
        .route("/api/displays/pair/{id}", get(pair_poll))
        .route("/api/displays/pair/{id}/approve", post(approve))
        .route("/api/displays/link", post(link_create))
        .route("/api/displays/self", get(own))
        .route("/api/displays/{id}/settings", post(settings))
        .route("/api/displays/{id}/revoke", post(revoke))
        .route("/api/auth/display-link", post(redeem_link))
}

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn unknown_display() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "display_unknown", "no such display")
}

fn superseded() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "superseded",
        "This wall launch was replaced by a newer one.",
    )
}

fn display_mismatch() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "display_mismatch",
        "This pairing belongs to another display.",
    )
}

fn unknown_pairing() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "pairing_unknown", "no such pairing")
}

/// 400 `invalid_display` for a bad value; any other store error is 500.
fn deck_error(e: DeckError) -> ApiError {
    match e {
        DeckError::Invalid(message) => bad("invalid_display", message),
        e => ApiError::internal(e),
    }
}

/// One audit row of the owner: the target is a display or a pairing id.
async fn record<T>(
    board: &Board,
    action: &'static str,
    target: &str,
    detail: Option<String>,
    result: &Result<T, ApiError>,
) {
    let row = match result {
        Ok(_) => NewAudit::new("owner", action, AuditResult::Ok),
        Err(e) => NewAudit::new("owner", action, AuditResult::Failed).detail(e.code),
    };
    let row = match (result, detail) {
        (Ok(_), Some(d)) => row.detail(d),
        _ => row,
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// A live elevation, or 400 `elevation_required` (`keys: ["displays"]`).
fn require_elevation(
    board: &Board,
    session: &LoginSession,
    elevation: Option<&str>,
) -> Result<(), ApiError> {
    if is_elevated(board, session, elevation) {
        return Ok(());
    }
    let mut keys = serde_json::Map::new();
    keys.insert("keys".to_owned(), json!(["displays"]));
    Err(bad(
        "elevation_required",
        "a recent sign-in is needed to give a display access",
    )
    .with_details(keys))
}

// ---------------------------------------------------------------- pairing

/// `POST /api/displays/pair`: no login. Answers the code and sets the
/// pending cookie (R13 §5B item 2).
///
/// # Errors
/// 429 `too_many_requests`; 503 `pairing_busy`.
async fn pair_start(
    State(board): State<Arc<Board>>,
    peer: Option<Extension<PeerAddr>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    const ACTION: &str = "display.pair_start";
    if let Err(e) = board_fence::sign_in_check(&board.store, board_fence::peer_ip(&peer)).await {
        return e.into_response();
    }
    let ip = request_source(peer);
    let source = ip.to_string();
    let refused = |detail: &'static str| {
        NewAudit::new("anonymous", ACTION, AuditResult::Refused)
            .target(source.clone())
            .detail(detail)
    };
    // A body that is not valid makes no pairing, so it takes no budget.
    let expected = match parse_pair_body(&body) {
        Ok(e) => e,
        Err(message) => return bad("invalid_body", message).into_response(),
    };
    if let Err(wait) = board.limiter.pair_start.reserve(ip, board.store.now_ms()) {
        board.store.audit_or_log(refused("too_many_requests")).await;
        return too_many_requests(format!("Too many display pairings; try again in {wait} s"))
            .into_response();
    }
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    match board
        .store
        .pairing_start(source.clone(), agent, expected)
        .await
    {
        Ok((pairing_id, code, secret)) => {
            let expires_at = match board.store.pairing_get(pairing_id.clone()).await {
                Ok(Some(p)) => p.expires_at,
                Ok(None) => return unknown_pairing().into_response(),
                Err(e) => return ApiError::internal(e).into_response(),
            };
            board
                .store
                .audit_or_log(
                    NewAudit::new("anonymous", ACTION, AuditResult::Ok).target(source.clone()),
                )
                .await;
            let mut res = Json(PairingStarted {
                pairing_id,
                code,
                expires_at,
            })
            .into_response();
            res.headers_mut().insert(
                header::SET_COOKIE,
                named_cookie(PENDING_COOKIE, secret.expose(), PENDING_COOKIE_AGE_S),
            );
            res
        }
        Err(DeckError::Busy(_)) => {
            board.store.audit_or_log(refused("pairing_busy")).await;
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pairing_busy",
                "Too many display pairings; try again later",
            )
            .into_response()
        }
        Err(e) => ApiError::internal(e).into_response(),
    }
}

/// `GET /api/displays/pair/{id}`: the kiosk's poll. With the pending
/// cookie of an approved pairing, the pairing is claimed once and the
/// answer sets `fd_display` and clears the pending cookie.
///
/// # Errors
/// 404 `pairing_unknown` (a wrong cookie, an unknown or expired id); 429
/// `too_many_requests` above one poll a second.
async fn pair_poll(
    State(board): State<Arc<Board>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    const ACTION: &str = "display.pair_claim";
    let Some(secret) = cookie(&headers, PENDING_COOKIE) else {
        return unknown_pairing().into_response();
    };
    if !board.limiter.pair_poll.allow(&id, board.store.now_ms()) {
        return too_many_requests("Poll at most once a second").into_response();
    }
    match board.store.pairing_claim(id, &secret).await {
        Ok(Claim::Pending) => Json(PairingPoll {
            state: PairingState::Pending,
        })
        .into_response(),
        Ok(Claim::Unknown) => unknown_pairing().into_response(),
        Ok(Claim::Stale) => {
            board
                .store
                .audit_or_log(
                    NewAudit::new("anonymous", ACTION, AuditResult::Refused).detail("stale"),
                )
                .await;
            ApiError::new(StatusCode::UNAUTHORIZED, "login_required", "log in first")
                .into_response()
        }
        Ok(Claim::Superseded) => {
            board
                .store
                .audit_or_log(
                    NewAudit::new("anonymous", ACTION, AuditResult::Refused).detail("superseded"),
                )
                .await;
            superseded().into_response()
        }
        Ok(Claim::Claimed(issued)) => {
            let cookie_value = issued.cookie;
            board
                .store
                .audit_or_log(
                    NewAudit::new("anonymous", ACTION, AuditResult::Ok).target(issued.display_id),
                )
                .await;
            let mut res = Json(PairingPoll {
                state: PairingState::ApprovedClaimed,
            })
            .into_response();
            let h = res.headers_mut();
            h.append(
                header::SET_COOKIE,
                named_cookie(DISPLAY_COOKIE, cookie_value.expose(), DISPLAY_MAX_MS / 1000),
            );
            h.append(header::SET_COOKIE, named_cookie(PENDING_COOKIE, "", 0));
            res
        }
        Err(e) => ApiError::internal(e).into_response(),
    }
}

// ------------------------------------------------------------- owner API

/// `GET /api/displays`: the displays and the pending pairings.
///
/// # Errors
/// 403 `owner_only`.
async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<DisplayList>, ApiError> {
    owner_only(&principal, "lists displays")?;
    let displays = board
        .store
        .display_list()
        .await
        .map_err(ApiError::internal)?
        .iter()
        .map(|d| d.view())
        .collect();
    let pending = board
        .store
        .pairing_pending()
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(DisplayList { displays, pending }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    code: String,
    /// An existing display (a managed `wall.open` launch), or the name and
    /// ceiling of a new one: exactly one of the two forms.
    #[serde(default)]
    display_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    ceiling: Option<Vec<StationId>>,
    #[serde(default)]
    elevation: Option<String>,
}

/// `POST /api/displays/pair/{id}/approve { code, name, ceiling, elevation }`.
///
/// # Errors
/// 400 `invalid_body`, `invalid_display`, `elevation_required`; 403
/// `owner_only`; 404 `pairing_unknown`; 409 `pairing_mismatch`.
async fn approve(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<ApproveBody>, JsonRejection>,
) -> Result<Json<DisplayView>, ApiError> {
    const ACTION: &str = "display.pair_approve";
    let session = owner(&board, &principal, ACTION, &id, "approves a display").await?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    let result = approve_pairing(&board, &session, &id, body).await;
    record(&board, ACTION, &id, None, &result).await;
    result.map(Json)
}

async fn approve_pairing(
    board: &Board,
    session: &LoginSession,
    id: &str,
    body: ApproveBody,
) -> Result<DisplayView, ApiError> {
    if body.code.len() != 6 || !body.code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("invalid_display", "the code is 6 digits"));
    }
    let approval = match (body.display_id, body.name, body.ceiling) {
        (Some(display_id), None, None) => {
            // An existing display gives it a new login: elevation is needed.
            require_elevation(board, session, body.elevation.as_deref())?;
            board
                .store
                .pairing_approve_existing(id.to_owned(), body.code, display_id)
                .await
                .map_err(deck_error)?
        }
        (None, Some(name), Some(ceiling)) => {
            let new = board
                .store
                .display_validate(NewDisplay { name, ceiling })
                .await
                .map_err(deck_error)?;
            require_elevation(board, session, body.elevation.as_deref())?;
            board
                .store
                .pairing_approve_new(id.to_owned(), body.code, new)
                .await
                .map_err(deck_error)?
        }
        _ => {
            return Err(bad(
                "invalid_display",
                "give display_id, or name and ceiling",
            ));
        }
    };
    match approval {
        Approval::Approved(d) => Ok(d.view()),
        Approval::Unknown => Err(unknown_pairing()),
        Approval::Mismatch => Err(ApiError::new(
            StatusCode::CONFLICT,
            "pairing_mismatch",
            "The code does not match.",
        )),
        Approval::DisplayUnknown => Err(unknown_display()),
        Approval::DisplayMismatch => Err(display_mismatch()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkBody {
    name: String,
    ceiling: Vec<StationId>,
    #[serde(default)]
    elevation: Option<String>,
}

/// The board origin for a link: the `Origin` of the request when this deck's
/// board allows it, else the first board address (R13 D23 Q10).
fn board_origin(board: &Board, headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| board.origins.allows(o))
        .map(str::to_owned)
        .or_else(|| {
            board
                .endpoints
                .entries(board.store.now_ms())
                .into_iter()
                .next()
                .map(|e| e.origin)
        })
}

/// `POST /api/displays/link { name, ceiling, elevation }`: makes the
/// display and a one-time link (5 minutes).
///
/// # Errors
/// 400 `invalid_body`, `invalid_display`, `elevation_required`; 403
/// `owner_only`.
async fn link_create(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Result<Json<LinkBody>, JsonRejection>,
) -> Result<Json<DisplayLinkMade>, ApiError> {
    const ACTION: &str = "display.link_create";
    let session = owner(&board, &principal, ACTION, "", "adds a display").await?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    let result = make_link(&board, &session, &headers, body).await;
    match &result {
        Ok((id, _)) => record(&board, ACTION, id, None, &result).await,
        Err(_) => record(&board, ACTION, "", None, &result).await,
    }
    result.map(|(_, made)| Json(made))
}

async fn make_link(
    board: &Board,
    session: &LoginSession,
    headers: &HeaderMap,
    body: LinkBody,
) -> Result<(String, DisplayLinkMade), ApiError> {
    let new = board
        .store
        .display_validate(NewDisplay {
            name: body.name,
            ceiling: body.ceiling,
        })
        .await
        .map_err(deck_error)?;
    require_elevation(board, session, body.elevation.as_deref())?;
    let origin = board_origin(board, headers)
        .ok_or_else(|| ApiError::internal("the deck has no board address for a display link"))?;
    let (display, token, expires_at) = board
        .store
        .create_display_with_link(new)
        .await
        .map_err(deck_error)?;
    // The display document is `/display`; the link is `/display#display=...`
    // (R13 §5F item 3).
    let url = format!(
        "{}/display#display={}",
        origin.trim_end_matches('/'),
        token.expose()
    );
    Ok((display.display_id, DisplayLinkMade { url, expires_at }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkRedeem {
    token: String,
}

/// `POST /api/auth/display-link { token }`: no login. Redeems a link and
/// sets `fd_display`. The login link limiter applies.
///
/// # Errors
/// 401 `invalid_credentials`; 429 `too_many_attempts`.
async fn redeem_link(
    State(board): State<Arc<Board>>,
    peer: Option<Extension<PeerAddr>>,
    body: Result<Json<LinkRedeem>, JsonRejection>,
) -> Response {
    const AUDIT: Audit = ("display.link_redeem", "display_link");
    if let Err(e) = board_fence::sign_in_check(&board.store, board_fence::peer_ip(&peer)).await {
        return e.into_response();
    }
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return body_error(&e).into_response(),
    };
    let ip = request_source(peer);
    let attempt =
        match start_attempt(&board, &board.limiter.login_link, ip, AUDIT, "anonymous").await {
            Ok(a) => a,
            Err(e) => return e.into_response(),
        };
    let found = board
        .store
        .redeem_display_link(&Secret::new(body.token))
        .await;
    let issued = match found {
        Ok(Some(issued)) => issued,
        Ok(None) => {
            attempt.failed(board.store.now_ms());
            return limit_refusal(
                &board,
                AUDIT,
                "anonymous",
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "invalid_credentials",
                    "the display link is not valid",
                ),
            )
            .await
            .into_response();
        }
        Err(e) => {
            attempt.release();
            return ApiError::internal(e).into_response();
        }
    };
    attempt.succeeded();
    board
        .store
        .audit_or_log(
            NewAudit::new("anonymous", AUDIT.0, AuditResult::Ok).target(issued.display_id),
        )
        .await;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        named_cookie(
            DISPLAY_COOKIE,
            issued.cookie.expose(),
            DISPLAY_MAX_MS / 1000,
        ),
    );
    res
}

/// `Some(None)` for a JSON `null`, `None` for an absent field.
fn some_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    ceiling: Option<Vec<StationId>>,
    #[serde(default)]
    show_diffs: Option<bool>,
    #[serde(default)]
    wake_on_needs_you: Option<bool>,
    #[serde(default, deserialize_with = "some_option")]
    quiet_hours: Option<Option<String>>,
    /// `null` clears the binding (R13 §6D item 2).
    #[serde(default, deserialize_with = "some_option")]
    binding: Option<Option<BindingBody>>,
    #[serde(default)]
    elevation: Option<String>,
}

/// `POST /api/displays/{id}/settings`: changes a display. Elevation is
/// needed when `show_diffs` turns on, the ceiling gains a station, or the
/// binding is set or changed (it gives station authority and the wake).
///
/// # Errors
/// 400 `invalid_body`, `invalid_display`, `elevation_required`; 403
/// `owner_only`; 404 `display_unknown`.
async fn settings(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<SettingsBody>, JsonRejection>,
) -> Result<Json<DisplayView>, ApiError> {
    const ACTION: &str = "display.settings";
    let session = owner(&board, &principal, ACTION, &id, "changes a display").await?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    let keys = [
        ("name", body.name.is_some()),
        ("ceiling", body.ceiling.is_some()),
        ("show_diffs", body.show_diffs.is_some()),
        ("wake_on_needs_you", body.wake_on_needs_you.is_some()),
        ("quiet_hours", body.quiet_hours.is_some()),
        ("binding", body.binding.is_some()),
    ]
    .iter()
    .filter(|(_, set)| *set)
    .map(|(k, _)| *k)
    .collect::<Vec<_>>()
    .join(",");
    let result = change_display(&board, &session, &id, body).await;
    record(&board, ACTION, &id, Some(keys), &result).await;
    result.map(Json)
}

async fn change_display(
    board: &Board,
    session: &LoginSession,
    id: &str,
    body: SettingsBody,
) -> Result<DisplayView, ApiError> {
    let name = body
        .name
        .as_deref()
        .map(clean_name)
        .transpose()
        .map_err(deck_error)?;
    if let Some(Some(q)) = &body.quiet_hours {
        check_quiet_hours(q).map_err(deck_error)?;
    }
    if let Some(ceiling) = &body.ceiling {
        board
            .store
            .display_validate_ceiling(ceiling.clone())
            .await
            .map_err(deck_error)?;
    }
    let old = board
        .store
        .display_get(id.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(unknown_display)?;
    let more_stations = body
        .ceiling
        .as_ref()
        .is_some_and(|c| c.iter().any(|s| !old.ceiling.contains(s)));
    let diffs_on = body.show_diffs == Some(true) && !old.show_diffs;
    let new_binding = match &body.binding {
        Some(Some(b)) => {
            check_monitor(&b.monitor).map_err(bind_error)?;
            board
                .store
                .display_validate_ceiling(vec![b.station_id.clone()])
                .await
                .map_err(deck_error)?;
            Some(DisplayBinding {
                station_id: b.station_id.clone(),
                monitor: b.monitor.clone(),
            })
        }
        _ => None,
    };
    // Setting or changing the binding needs elevation; clearing it does not.
    let binding_set = new_binding.is_some() && new_binding != old.binding;
    if more_stations || diffs_on || binding_set {
        require_elevation(board, session, body.elevation.as_deref())?;
    }
    let change = DisplayChange {
        name,
        ceiling: body.ceiling,
        show_diffs: body.show_diffs,
        wake_on_needs_you: body.wake_on_needs_you,
        quiet_hours: body.quiet_hours,
    };
    let updated = board
        .store
        .display_update(id.to_owned(), change)
        .await
        .map_err(deck_error)?
        .ok_or_else(unknown_display)?;
    match body.binding {
        None => Ok(updated.view()),
        Some(_) => board
            .store
            .display_set_binding(id.to_owned(), new_binding)
            .await
            .map(|d| d.view())
            .map_err(bind_error),
    }
}

/// The HTTP error of a refused binding change.
fn bind_error(e: BindError) -> ApiError {
    match e {
        BindError::Unknown => unknown_display(),
        BindError::Bound => ApiError::new(
            StatusCode::CONFLICT,
            "display_bound",
            "The display is bound to another station.",
        ),
        BindError::Superseded => superseded(),
        BindError::Invalid(m) => bad("invalid_display", m),
        BindError::Store(e) => ApiError::internal(e),
    }
}

/// `POST /api/displays/{id}/revoke`: deletes the sessions, links and
/// approved pairings of the display. Its WebSocket closes within 5 s.
///
/// # Errors
/// 403 `owner_only`; 404 `display_unknown`.
async fn revoke(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    const ACTION: &str = "display.revoke";
    owner(&board, &principal, ACTION, &id, "revokes a display").await?;
    let result = match board.store.display_revoke(id.clone()).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(unknown_display()),
        Err(e) => Err(ApiError::internal(e)),
    };
    record(&board, ACTION, &id, None, &result).await;
    result
}

/// `GET /api/displays/self`: the display itself.
///
/// # Errors
/// 403 `display_only`.
async fn own(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<DisplaySelf>, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    let record = board
        .store
        .display_get(login.display_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "login_required", "log in first"))?;
    Ok(Json(DisplaySelf {
        display_id: record.display_id,
        name: record.name,
        ceiling: record.ceiling,
        binding: record.binding,
    }))
}
