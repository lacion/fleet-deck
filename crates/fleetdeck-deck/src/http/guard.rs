//! The board-API guard (M2 P3-8; PLAN §9, A13, L15). For every request:
//!
//! 1. The matched route must be in [`super::methods::API`] (fail closed).
//! 2. An `Origin` header must name a board address of this deck (its
//!    listen addresses, `localhost` for loopback ones, and the tailnet
//!    host name). This also covers the login methods.
//! 3. Every method except the login methods needs a principal: a bearer
//!    API token (`Authorization: Bearer fdtok1_…`) or the owner's login
//!    cookie. Login is required also on loopback (A13).
//! 4. The principal must hold the method's scope.
//! 5. A mutating method with a cookie login needs the `X-CSRF-Token`
//!    header of that login.
//!
//! The board WebSocket (`GET /api/ws`) authenticates differently: a
//! `?ticket=` from `POST /api/ws/ticket`, or the login cookie with a
//! mandatory `Origin` header of this deck (a browser sends cookies on a
//! cross-site WebSocket upgrade). A bearer header is refused there.
//!
//! A refused mutating request is audited.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fleetdeck_core::Secret;
use fleetdeck_core::secret::ct_eq;
use serde_json::json;

use super::Board;
use super::methods::{self, ApiMethod};
use crate::store::{AuditResult, NewAudit, Principal};

/// The login cookie.
pub const COOKIE: &str = "fd_session";
/// The CSRF header of mutating requests with a cookie login.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// A refused or failed request: `{ "error": code, "message": text }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %e, "board API");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "the deck failed; see its log",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(json!({ "error": self.code, "message": self.message })),
        )
            .into_response()
    }
}

/// The origins that may call the board API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origins(BTreeSet<String>);

impl Origins {
    /// The origins of the bound board addresses and the tailnet host name.
    /// The board serves HTTPS only (DL-054), so every origin is `https`.
    #[must_use]
    pub fn new(addrs: &[SocketAddr], tailnet_hostname: Option<&str>) -> Self {
        let mut set = BTreeSet::new();
        let mut add = |host: &str, port: u16| {
            // A browser sends the default port of `https` as no port
            // (RFC 6454 §6.1): `https://host`, never `https://host:443`.
            if port == 443 {
                set.insert(format!("https://{host}"));
            } else {
                set.insert(format!("https://{host}:{port}"));
            }
        };
        for addr in addrs {
            let ip = match addr.ip() {
                IpAddr::V4(v4) => v4.to_string(),
                IpAddr::V6(v6) => format!("[{v6}]"),
            };
            add(&ip, addr.port());
            if addr.ip().is_loopback() {
                add("localhost", addr.port());
            }
            if let Some(host) = tailnet_hostname {
                add(host, addr.port());
            }
        }
        Self(set)
    }

    #[must_use]
    pub fn allows(&self, origin: &str) -> bool {
        self.0.contains(&origin.to_ascii_lowercase())
    }
}

/// The value of one cookie in the `Cookie` headers.
#[must_use]
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<Secret<String>> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name && !v.is_empty()).then(|| Secret::new(v.to_owned()))
        })
}

fn bearer(headers: &HeaderMap) -> Option<Secret<String>> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = v.strip_prefix("Bearer ")?.trim();
    Some(Secret::new(token.to_owned()))
}

/// The principal of a request. A bearer header that names no valid token
/// is refused; it does not fall back to the cookie.
///
/// # Errors
/// [`ApiError`] `login_required` (401) or `internal`.
pub async fn authenticate(board: &Board, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let missing = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "login_required",
            "log in first (run `fleetdeck open` for a login link)",
        )
    };
    if headers.contains_key(header::AUTHORIZATION) {
        let value = bearer(headers).ok_or_else(missing)?;
        return match board.store.token(&value).await {
            Ok(Some(t)) => Ok(Principal::Token(t)),
            Ok(None) => Err(missing()),
            Err(e) => Err(ApiError::internal(e)),
        };
    }
    let value = cookie(headers, COOKIE).ok_or_else(missing)?;
    match board.store.login_session(&value).await {
        Ok(Some(s)) => Ok(Principal::Owner(s)),
        Ok(None) => Err(missing()),
        Err(e) => Err(ApiError::internal(e)),
    }
}

/// Checks steps 2-5 for `api` and returns the principal (`None` for a
/// login method).
///
/// # Errors
/// The refusal.
pub async fn check(
    board: &Board,
    api: &ApiMethod,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<Option<Principal>, (Option<String>, ApiError)> {
    if let Some(origin) = headers.get(header::ORIGIN) {
        let ok = origin.to_str().is_ok_and(|o| board.origins.allows(o));
        if !ok {
            return Err((
                None,
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "origin",
                    "the request comes from a page that is not this deck's board",
                ),
            ));
        }
    }
    let Some(scope) = api.scope else {
        return Ok(None);
    };
    let principal = if api.name == methods::WS_METHOD {
        ws_principal(board, headers, query).await
    } else {
        authenticate(board, headers).await
    }
    .map_err(|e| (None, e))?;
    if !principal.has(scope) {
        return Err((
            Some(principal.actor()),
            ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                format!("`{}` needs the {scope:?} scope", api.name).to_lowercase(),
            ),
        ));
    }
    if api.mutating
        && let Principal::Owner(session) = &principal
    {
        let sent = headers
            .get(CSRF_HEADER)
            .map(|v| v.as_bytes())
            .unwrap_or_default();
        if !ct_eq(sent, session.csrf.as_bytes()) {
            return Err((
                Some(principal.actor()),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "csrf",
                    "a mutating request needs the X-CSRF-Token of the login",
                ),
            ));
        }
    }
    Ok(Some(principal))
}

/// The principal of a WebSocket upgrade: a ticket, or a cookie login
/// with an `Origin` header (checked by the caller).
async fn ws_principal(
    board: &Board,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<Principal, ApiError> {
    let ticket = query
        .unwrap_or_default()
        .split('&')
        .find_map(|kv| kv.strip_prefix("ticket="));
    if let Some(ticket) = ticket {
        return board
            .tickets
            .redeem(ticket, board.store.now_ms())
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "login_required",
                    "the WebSocket ticket is unknown, used or expired",
                )
            });
    }
    if headers.contains_key(header::AUTHORIZATION) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "login_required",
            "a bearer client opens the WebSocket with a ticket (POST /api/ws/ticket)",
        ));
    }
    // A missing or ended login is 401 first; a valid cookie without an
    // Origin is refused (a foreign Origin was refused before).
    let principal = authenticate(board, headers).await?;
    if !headers.contains_key(header::ORIGIN) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin",
            "a WebSocket with a login cookie needs the Origin of this deck's board",
        ));
    }
    Ok(principal)
}

/// The middleware of every board-API route.
pub async fn guard(
    State(board): State<Arc<Board>>,
    matched: Option<MatchedPath>,
    mut req: Request,
    next: Next,
) -> Response {
    let api = matched
        .as_ref()
        .and_then(|p| methods::find(req.method(), p.as_str()));
    let Some(api) = api else {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such method").into_response();
    };
    let query = req.uri().query().map(str::to_owned);
    match check(&board, api, req.headers(), query.as_deref()).await {
        Ok(Some(principal)) => {
            req.extensions_mut().insert(principal);
            next.run(req).await
        }
        Ok(None) => next.run(req).await,
        Err((actor, e)) => {
            if api.mutating {
                board
                    .store
                    .audit_or_log(
                        NewAudit::new(
                            actor.unwrap_or_else(|| "anonymous".to_owned()),
                            api.name,
                            AuditResult::Refused,
                        )
                        .detail(e.code),
                    )
                    .await;
            }
            e.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn origins(addrs: &[&str], tailnet: Option<&str>) -> Origins {
        let addrs: Vec<SocketAddr> = addrs.iter().map(|a| a.parse().unwrap()).collect();
        Origins::new(&addrs, tailnet)
    }

    #[test]
    fn the_default_https_port_is_the_canonical_origin_without_a_port() {
        let o = origins(
            &["127.0.0.1:443", "[::1]:443", "192.168.1.5:443"],
            Some("deck.tail1234.ts.net"),
        );
        for ok in [
            "https://127.0.0.1",
            "https://localhost",
            "https://[::1]",
            "https://192.168.1.5",
            "https://deck.tail1234.ts.net",
            "HTTPS://LOCALHOST",
        ] {
            assert!(o.allows(ok), "{ok}");
        }
        for refused in [
            "https://127.0.0.1:443",
            "https://localhost:443",
            "http://127.0.0.1",
            "https://192.168.1.6",
            "https://evil.example",
            "null",
        ] {
            assert!(!o.allows(refused), "{refused}");
        }
    }

    #[test]
    fn another_port_is_part_of_the_origin() {
        let o = origins(
            &["127.0.0.1:7420", "[fd7a:115c:a1e0::1]:7420"],
            Some("deck.tail1234.ts.net"),
        );
        for ok in [
            "https://127.0.0.1:7420",
            "https://localhost:7420",
            "https://[fd7a:115c:a1e0::1]:7420",
            "https://deck.tail1234.ts.net:7420",
        ] {
            assert!(o.allows(ok), "{ok}");
        }
        for refused in [
            "https://127.0.0.1",
            "https://localhost",
            "https://deck.tail1234.ts.net",
            "https://[fd7a:115c:a1e0::1]",
            "https://127.0.0.1:7421",
        ] {
            assert!(!o.allows(refused), "{refused}");
        }
    }
}
