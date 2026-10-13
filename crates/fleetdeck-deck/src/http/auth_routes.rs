//! The login methods (M2 P3-8; PLAN §9, A13): password login, one-time
//! link login (`fleetdeck open`), logout and the session view. Failed
//! logins are limited: after [`MAX_FAILURES`] failures in
//! [`FAILURE_WINDOW_MS`] the deck refuses every login with 429 until the
//! window has passed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fleetdeck_core::Secret;
use fleetdeck_proto::board::{DeviceLink, DeviceLinkEntry, SessionKey};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::share::{JoinRequest, ShareKind, ShareRole, SharedScope};
use serde::{Deserialize, Serialize};

use super::Board;
use super::guard::{ApiError, COOKIE, origin_label, owner_only};
use crate::local_auth::login_url;
use crate::store::auth::{LOGIN_LINK_TTL_MS, SESSION_MAX_MS};
use crate::store::{
    AuditResult, DeckError, LoginMethod, NewAudit, NewLogin, Principal, ShareGrant, ShareScope,
};

/// Failed logins in one window before the deck refuses logins.
pub const MAX_FAILURES: usize = 5;
/// The failure window.
pub const FAILURE_WINDOW_MS: i64 = 60_000;

/// The failed login attempts of the last window.
#[derive(Debug, Default)]
pub struct LoginLimiter(Mutex<VecDeque<i64>>);

impl LoginLimiter {
    fn recent(&self, now: i64) -> std::sync::MutexGuard<'_, VecDeque<i64>> {
        let mut q = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        while q.front().is_some_and(|t| *t <= now - FAILURE_WINDOW_MS) {
            q.pop_front();
        }
        q
    }

    /// The seconds to wait, or `None` when a login may be tried.
    fn locked(&self, now: i64) -> Option<i64> {
        let q = self.recent(now);
        (q.len() >= MAX_FAILURES).then(|| {
            q.front()
                .map_or(1, |t| (t + FAILURE_WINDOW_MS - now) / 1000 + 1)
        })
    }

    fn failed(&self, now: i64) {
        self.recent(now).push_back(now);
    }
}

/// Successful device links in one window.
pub const MAX_DEVICE_LINKS: usize = 10;

/// The device links of the last window (DL-146).
#[derive(Debug, Default)]
pub struct DeviceLinkLimiter(Mutex<VecDeque<i64>>);

impl DeviceLinkLimiter {
    /// Records a link at `now`; false when the window holds
    /// [`MAX_DEVICE_LINKS`] already.
    fn reserve(&self, now: i64) -> bool {
        let mut q = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        while q.front().is_some_and(|t| *t <= now - FAILURE_WINDOW_MS) {
            q.pop_front();
        }
        if q.len() >= MAX_DEVICE_LINKS {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Takes back the record of `reserve(now)` (the link was not made).
    fn release(&self, now: i64) {
        let mut q = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = q.iter().rposition(|t| *t == now) {
            q.remove(i);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordLogin {
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkLogin {
    token: String,
}

/// The answer to a login and to `GET /api/auth/session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    /// `owner` or `token:<name>`.
    pub actor: String,
    pub scopes: Vec<Scope>,
    /// The CSRF token of a cookie login (send it as `X-CSRF-Token`).
    pub csrf: Option<String>,
    pub expires_at: i64,
    /// The version of this deck (DL-141).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck_version: Option<String>,
    /// The display name of a member (R11, DL-166).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The role of a member's share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ShareRole>,
    /// What a member's share covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<SharedScope>,
}

/// The version of this deck.
const DECK_VERSION: &str = env!("CARGO_PKG_VERSION");

fn set_cookie(value: &str, max_age_s: i64) -> HeaderValue {
    let text =
        format!("{COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={max_age_s}");
    HeaderValue::from_str(&text)
        .unwrap_or_else(|_| HeaderValue::from_static("fd_session=; Max-Age=0"))
}

fn logged_in(login: &NewLogin) -> Response {
    let view = SessionView {
        actor: "owner".to_owned(),
        scopes: crate::store::auth::OWNER_SCOPES.to_vec(),
        csrf: Some(login.csrf.clone()),
        expires_at: login.expires_at,
        deck_version: Some(DECK_VERSION.to_owned()),
        display_name: None,
        role: None,
        scope: None,
    };
    let mut res = Json(view).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(login.cookie.expose(), SESSION_MAX_MS / 1000),
    );
    res
}

async fn login(
    board: &Board,
    action: &str,
    method: LoginMethod,
    valid: Result<bool, ApiError>,
) -> Response {
    let now = board.store.now_ms();
    let refused =
        |detail: &str| NewAudit::new("anonymous", action, AuditResult::Refused).detail(detail);
    match valid {
        Ok(true) => match board.store.create_login(method).await {
            Ok(login) => {
                board
                    .store
                    .audit_or_log(NewAudit::new("owner", action, AuditResult::Ok))
                    .await;
                logged_in(&login)
            }
            Err(e) => ApiError::internal(e).into_response(),
        },
        Ok(false) => {
            board.limiter.failed(now);
            board
                .store
                .audit_or_log(refused("invalid credentials"))
                .await;
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "the password or login link is not valid",
            )
            .into_response()
        }
        Err(e) => e.into_response(),
    }
}

fn throttled(board: &Board) -> Option<ApiError> {
    let wait = board.limiter.locked(board.store.now_ms())?;
    Some(ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_attempts",
        format!("too many failed logins; try again in {wait} s"),
    ))
}

/// `POST /api/auth/login { password }`.
pub async fn password(
    State(board): State<Arc<Board>>,
    Json(body): Json<PasswordLogin>,
) -> Response {
    if let Some(e) = throttled(&board) {
        board
            .store
            .audit_or_log(
                NewAudit::new("anonymous", "auth.login", AuditResult::Refused).detail(e.code),
            )
            .await;
        return e.into_response();
    }
    let valid = board
        .store
        .check_password(Secret::new(body.password))
        .await
        .map_err(ApiError::internal);
    login(&board, "auth.login", LoginMethod::Password, valid).await
}

/// `POST /api/auth/link { token }`: the token of a `fleetdeck open` link.
pub async fn link(State(board): State<Arc<Board>>, Json(body): Json<LinkLogin>) -> Response {
    if let Some(e) = throttled(&board) {
        board
            .store
            .audit_or_log(
                NewAudit::new("anonymous", "auth.link", AuditResult::Refused).detail(e.code),
            )
            .await;
        return e.into_response();
    }
    let valid = board
        .store
        .redeem_login_link(&Secret::new(body.token))
        .await
        .map_err(ApiError::internal);
    login(&board, "auth.link", LoginMethod::Link, valid).await
}

/// What a member's share covers, for the session view and the join answer.
async fn shared_scope(board: &Board, grant: &ShareGrant) -> Result<SharedScope, ApiError> {
    let sessions = match &grant.scope {
        ShareScope::Board => Vec::new(),
        ShareScope::Sessions(set) => set
            .iter()
            .map(|(station, session)| SessionKey {
                station_id: station.clone().into(),
                session_id: session.clone().into(),
            })
            .collect(),
    };
    let space_name = if grant.kind == ShareKind::Space {
        board
            .store
            .space_name(grant.target.clone())
            .await
            .map_err(ApiError::internal)?
    } else {
        None
    };
    Ok(SharedScope {
        kind: grant.kind,
        target: grant.target.clone(),
        space_name,
        steering: grant.steering,
        sessions,
    })
}

async fn member_view(board: &Board, grant: &ShareGrant) -> Result<SessionView, ApiError> {
    Ok(SessionView {
        actor: grant.actor(),
        scopes: grant.scopes().into_iter().collect(),
        csrf: Some(grant.csrf.clone()),
        expires_at: grant.expires_at,
        deck_version: Some(DECK_VERSION.to_owned()),
        display_name: Some(grant.display_name.clone()),
        role: Some(grant.role),
        scope: Some(shared_scope(board, grant).await?),
    })
}

/// `POST /api/auth/join { token, display_name }` (R11.4, DL-166): the invite
/// token of `#join=<token>` and the display name that the person picks. A
/// valid invite makes a member login (a cookie tied to the member and the
/// share). The deck limits failed attempts like failed logins. Neither the
/// token nor the cookie goes to a log or to the audit row.
///
/// # Errors
/// 400 `invalid_display_name`; 401 `invalid_invite`; 409
/// `display_name_taken`; 429
/// `too_many_attempts`; 500 `internal`.
pub async fn join(State(board): State<Arc<Board>>, Json(body): Json<JoinRequest>) -> Response {
    const ACTION: &str = "auth.join";
    if let Some(e) = throttled(&board) {
        board
            .store
            .audit_or_log(NewAudit::new("anonymous", ACTION, AuditResult::Refused).detail(e.code))
            .await;
        return e.into_response();
    }
    let joined = board
        .store
        .join(&Secret::new(body.token), &body.display_name)
        .await;
    match joined {
        Ok(Some(login)) => {
            let grant = login.grant;
            board
                .store
                .audit_or_log(
                    NewAudit::new(grant.actor(), ACTION, AuditResult::Ok)
                        .target(grant.share_id.clone()),
                )
                .await;
            let view = match member_view(&board, &grant).await {
                Ok(v) => v,
                Err(e) => return e.into_response(),
            };
            let max_age = (grant.expires_at - board.store.now_ms()).max(0) / 1000;
            let mut res = Json(view).into_response();
            res.headers_mut().insert(
                header::SET_COOKIE,
                set_cookie(login.cookie.expose(), max_age),
            );
            res
        }
        Ok(None) => {
            board.limiter.failed(board.store.now_ms());
            board
                .store
                .audit_or_log(
                    NewAudit::new("anonymous", ACTION, AuditResult::Refused)
                        .detail("invalid invite"),
                )
                .await;
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_invite",
                "the invitation is not valid, is used up, has expired or was revoked",
            )
            .into_response()
        }
        Err(DeckError::Invalid(message)) => {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_display_name", message).into_response()
        }
        Err(DeckError::NameTaken(_)) => ApiError::new(
            StatusCode::CONFLICT,
            "display_name_taken",
            "a member of another share uses this name; pick another",
        )
        .into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

/// `POST /api/auth/device-link` (DL-146): a one-time login link for a new
/// device, for the owner's board login only. One single-use token, one link
/// for each board origin. Neither the token nor a URL goes to a log or to
/// the audit row.
///
/// # Errors
/// 403 `owner_only`; 429 `too_many_requests`; 500 `internal`.
pub async fn device_link(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<DeviceLink>, ApiError> {
    const ACTION: &str = "auth.device_link";
    if let Err(e) = owner_only(&principal, "adds a device") {
        board
            .store
            .audit_or_log(
                NewAudit::new(principal.actor(), ACTION, AuditResult::Refused).detail(e.code),
            )
            .await;
        return Err(e);
    }
    let now = board.store.now_ms();
    if !board.device_links.reserve(now) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "Wait a minute before you add another device.",
        ));
    }
    let token = match board.store.create_login_link().await {
        Ok(token) => token,
        Err(e) => {
            board.device_links.release(now);
            return Err(ApiError::internal(e));
        }
    };
    board
        .store
        .audit_or_log(NewAudit::new(principal.actor(), ACTION, AuditResult::Ok))
        .await;
    let links = board
        .origins
        .list()
        .into_iter()
        .map(|origin| DeviceLinkEntry {
            label: origin_label(&origin),
            url: login_url(&origin, &token).expose().clone(),
        })
        .collect();
    Ok(Json(DeviceLink {
        links,
        expires_at: now + LOGIN_LINK_TTL_MS,
        certificate_sha256: board.certificate_sha256.clone(),
        self_signed: board.self_signed,
    }))
}

/// The body of `POST /api/auth/elevate`: the owner password or a one-time
/// code of `fleetdeck open --elevate`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElevateBody {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// The answer to `POST /api/auth/elevate` (DL-172, design 1.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Elevated {
    pub elevation: String,
    pub expires_at: i64,
}

async fn refused_elevation(board: &Board, code: &'static str) {
    board
        .store
        .audit_or_log(NewAudit::new("owner", "auth.elevate", AuditResult::Refused).detail(code))
        .await;
}

/// `POST /api/auth/elevate { password }` or `{ code }` (DL-172): an
/// elevation for the danger changes and the restart of the settings. Only
/// the owner's board login may call it. A failure counts in the same limiter
/// as a failed login. Neither the password, the code nor the elevation goes
/// to a log or to the audit row.
///
/// # Errors
/// 400 `invalid_request`; 401 `invalid_credentials`; 403 `owner_only`; 429
/// `too_many_attempts`; 500 `internal`.
pub async fn elevate(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<ElevateBody>, JsonRejection>,
) -> Result<Json<Elevated>, ApiError> {
    const ACTION: &str = "auth.elevate";
    let session =
        super::settings_routes::owner(&board, &principal, ACTION, "", "can ask for an elevation")
            .await?;
    let refuse = |code: &'static str| refused_elevation(&board, code);
    if let Some(e) = throttled(&board) {
        refuse(e.code).await;
        return Err(e);
    }
    let invalid = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "send either `password` or `code`",
        )
    };
    let Ok(Json(body)) = body else {
        refuse("invalid_request").await;
        return Err(invalid());
    };
    let now = board.store.now_ms();
    let valid = match (body.password, body.code) {
        (Some(password), None) => board
            .store
            .check_password(Secret::new(password))
            .await
            .map_err(ApiError::internal)?,
        (None, Some(code)) => board.elevations.redeem_code(&code, now),
        _ => {
            refuse("invalid_request").await;
            return Err(invalid());
        }
    };
    if !valid {
        board.limiter.failed(now);
        refuse("invalid_credentials").await;
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "the password or the code is not valid",
        ));
    }
    let (token, expires_at) = board
        .elevations
        .issue(&session.session_hash, now)
        .map_err(ApiError::internal)?;
    board
        .store
        .audit_or_log(NewAudit::new("owner", ACTION, AuditResult::Ok))
        .await;
    Ok(Json(Elevated {
        elevation: token.expose().clone(),
        expires_at,
    }))
}

/// `POST /api/auth/logout`: ends the cookie login.
pub async fn logout(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Response {
    let actor = principal.actor();
    let ended = match principal {
        Principal::Owner(session) => {
            board.elevations.end_session(&session.session_hash);
            board.store.end_login(session.session_hash).await
        }
        Principal::Member(_, grant) => board.store.end_member_login(grant.session_hash).await,
        Principal::Token(..) => Ok(true),
    };
    if let Err(e) = ended {
        return ApiError::internal(e).into_response();
    }
    board
        .store
        .audit_or_log(NewAudit::new(actor, "auth.logout", AuditResult::Ok))
        .await;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, set_cookie("", 0));
    res
}

/// `GET /api/auth/session`.
pub async fn session(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<SessionView>, ApiError> {
    let (csrf, expires_at) = match &principal {
        Principal::Owner(s) => (Some(s.csrf.clone()), s.expires_at),
        Principal::Token(t, _) => (None, t.expires_at),
        Principal::Member(_, grant) => return member_view(&board, grant).await.map(Json),
    };
    Ok(Json(SessionView {
        actor: principal.actor(),
        scopes: principal.scopes().into_iter().collect(),
        csrf,
        expires_at,
        deck_version: Some(DECK_VERSION.to_owned()),
        display_name: None,
        role: None,
        scope: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eleventh_device_link_in_a_minute_waits() {
        let l = DeviceLinkLimiter::default();
        for t in 0..10 {
            assert!(l.reserve(t * 1000));
        }
        assert!(!l.reserve(10_000));
        // A link that was not made frees its place.
        l.release(9000);
        assert!(l.reserve(10_001));
        // The first link leaves the window.
        assert!(l.reserve(60_000));
        assert!(!l.reserve(60_001));
    }

    #[test]
    fn five_failures_lock_logins_until_the_window_passes() {
        let l = LoginLimiter::default();
        for t in 0..5 {
            assert_eq!(l.locked(t * 1000), None);
            l.failed(t * 1000);
        }
        assert_eq!(l.locked(5000), Some(56));
        assert_eq!(l.locked(59_999), Some(1));
        // The first failure leaves the window.
        assert_eq!(l.locked(60_000), None);
    }
}
