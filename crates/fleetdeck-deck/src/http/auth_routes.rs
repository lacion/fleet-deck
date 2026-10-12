//! The login methods (M2 P3-8; PLAN §9, A13): password login, one-time
//! link login (`fleetdeck open`), logout and the session view. Failed
//! logins are limited: after [`MAX_FAILURES`] failures in
//! [`FAILURE_WINDOW_MS`] the deck refuses every login with 429 until the
//! window has passed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fleetdeck_core::Secret;
use fleetdeck_proto::link::Scope;
use serde::{Deserialize, Serialize};

use super::Board;
use super::guard::{ApiError, COOKIE};
use crate::store::auth::SESSION_MAX_MS;
use crate::store::{AuditResult, LoginMethod, NewAudit, NewLogin, Principal};

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

/// `POST /api/auth/logout`: ends the cookie login.
pub async fn logout(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Response {
    let actor = principal.actor();
    if let Principal::Owner(session) = principal
        && let Err(e) = board.store.end_login(session.session_hash).await
    {
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
pub async fn session(Extension(principal): Extension<Principal>) -> Json<SessionView> {
    let (csrf, expires_at) = match &principal {
        Principal::Owner(s) => (Some(s.csrf.clone()), s.expires_at),
        Principal::Token(t, _) => (None, t.expires_at),
    };
    Json(SessionView {
        actor: principal.actor(),
        scopes: principal.scopes().into_iter().collect(),
        csrf,
        expires_at,
        deck_version: Some(DECK_VERSION.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
