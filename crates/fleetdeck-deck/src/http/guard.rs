//! The board-API guard (M2 P3-8; PLAN §9, A13, L15). For every request:
//!
//! 1. The matched route must be in [`super::methods::api`] (fail closed).
//! 2. An `Origin` header must name a board address of this deck (its
//!    listen addresses, `localhost` for loopback ones, and the tailnet
//!    host name). This also covers the login methods.
//! 3. Every method except the login methods needs a principal: a bearer
//!    API token (`Authorization: Bearer fdtok1_…`) or the owner's login
//!    cookie. Login is required also on loopback (A13).
//! 4. The principal must hold the method's scope. A member of a shared
//!    session (R11, DL-166) has no scope check: the method must be open to
//!    members (`ApiMethod::member`; every other method is 403 `owner_only`),
//!    and its handler then checks the share (the session and the role).
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
use crate::store::auth::secret_hash;
use crate::store::{AuditResult, NewAudit, Principal};

/// The login cookie.
pub const COOKIE: &str = "fd_session";
/// The CSRF header of mutating requests with a cookie login.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// The login cookie of a wall display (R13 §5, DL-201).
pub const DISPLAY_COOKIE: &str = "fd_display";
/// The cookie of a pending display pairing (R13 §5C item 1).
pub const PENDING_COOKIE: &str = "fd_display_pending";
/// The header of a display's own logout: a custom header blocks a
/// cross-site form post, and it selects the display login when the browser
/// also holds the owner's cookie.
pub const DISPLAY_HEADER: &str = "x-fleetdeck-display";

/// A refused or failed request: `{ "error": code, "message": text }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// Fields that join the top level of the body, next to `error` and
    /// `message` (for example `keys` or `would_stop`).
    pub details: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ApiError {
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Adds fields to the top level of the error body. `error` and
    /// `message` cannot be replaced.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Map<String, serde_json::Value>) -> Self {
        self.details = Some(details);
        self
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
        let mut body = self.details.unwrap_or_default();
        body.insert("error".to_owned(), json!(self.code));
        body.insert("message".to_owned(), json!(self.message));
        (self.status, axum::Json(serde_json::Value::Object(body))).into_response()
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

    /// The origins that a link may name, in the order of a device link: the
    /// tailnet host names, then the non-loopback addresses, then loopback and
    /// `localhost`. A tailnet name resolves to a tailnet address, so it is
    /// listed only when the board also listens beyond loopback; it stays an
    /// allowed origin either way ([`Self::allows`]).
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        let beyond_loopback = self.0.iter().any(|o| origin_kind(o) == OriginKind::Network);
        let mut origins: Vec<String> = self
            .0
            .iter()
            .filter(|o| beyond_loopback || origin_kind(o) != OriginKind::Tailnet)
            .cloned()
            .collect();
        origins.sort_by_key(|o| origin_kind(o));
        origins
    }

    #[must_use]
    pub fn allows(&self, origin: &str) -> bool {
        self.0.contains(&origin.to_ascii_lowercase())
    }
}

/// What an origin names; the variants are in the order of
/// [`Origins::list`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OriginKind {
    Tailnet,
    Network,
    ThisComputer,
}

/// The host of `https://host[:port]` without brackets and port.
fn origin_host(origin: &str) -> &str {
    let rest = origin.strip_prefix("https://").unwrap_or(origin);
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split(']').next().unwrap_or(v6);
    }
    rest.split(':').next().unwrap_or(rest)
}

fn origin_kind(origin: &str) -> OriginKind {
    let host = origin_host(origin);
    if host == "localhost" {
        return OriginKind::ThisComputer;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => OriginKind::ThisComputer,
        Ok(_) => OriginKind::Network,
        Err(_) => OriginKind::Tailnet,
    }
}

/// The label of an origin in a device link: `Tailnet <host>`,
/// `Network <ip>` or `This computer`.
#[must_use]
pub fn origin_label(origin: &str) -> String {
    match origin_kind(origin) {
        OriginKind::Tailnet => format!("Tailnet {}", origin_host(origin)),
        OriginKind::Network => format!("Network {}", origin_host(origin)),
        OriginKind::ThisComputer => "This computer".to_owned(),
    }
}

/// Refuses everything but the owner's board login (PLAN §9, ADR-006 §3,
/// DL-137, DL-138): a bearer token gets 403 `owner_only`, whatever its
/// scopes, and a WebSocket ticket keeps the identity it was made for.
/// `action` completes "only the owner's board login …".
///
/// # Errors
/// 403 `owner_only` for a token principal.
pub fn owner_only(principal: &Principal, action: &str) -> Result<(), ApiError> {
    if matches!(principal, Principal::Owner(_)) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            format!("only the owner's board login {action}"),
        ))
    }
}

/// The login cookie of a member starts with this (the owner's cookie is
/// plain hex), so the guard asks one table.
const MEMBER_COOKIE_PREFIX: &str = "fdmem1_";

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

fn login_required() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "login_required",
        "log in first (run `fleetdeck open` for a login link)",
    )
}

/// The display login of the `fd_display` cookie, if it is valid.
async fn display_principal(
    board: &Board,
    headers: &HeaderMap,
) -> Result<Option<Principal>, ApiError> {
    let Some(value) = cookie(headers, DISPLAY_COOKIE) else {
        return Ok(None);
    };
    match board.store.display_session(&value).await {
        Ok(found) => Ok(found.map(Principal::Display)),
        Err(e) => Err(ApiError::internal(e)),
    }
}

/// True when the request names the display login (R13 §5C item 6).
#[must_use]
pub fn display_header(headers: &HeaderMap) -> bool {
    headers
        .get(DISPLAY_HEADER)
        .is_some_and(|v| v.as_bytes() == b"1")
}

/// The principal of a request. A bearer header that names no valid token
/// is refused; it does not fall back to the cookie. A valid `fd_session`
/// (owner or member) wins over `fd_display` (R13 §5C item 6).
///
/// # Errors
/// [`ApiError`] `login_required` (401) or `internal`.
pub async fn authenticate(board: &Board, headers: &HeaderMap) -> Result<Principal, ApiError> {
    if headers.contains_key(header::AUTHORIZATION) {
        let value = bearer(headers).ok_or_else(login_required)?;
        return match board.store.token(&value).await {
            Ok(Some(t)) => Ok(Principal::Token(t, secret_hash(value.expose()))),
            Ok(None) => Err(login_required()),
            Err(e) => Err(ApiError::internal(e)),
        };
    }
    if let Some(value) = cookie(headers, COOKIE) {
        let found = if value.expose().starts_with(MEMBER_COOKIE_PREFIX) {
            match board.store.member_login(&value).await {
                Ok(g) => g.map(|g| Principal::Member(g.member_id.clone(), Box::new(g))),
                Err(e) => return Err(ApiError::internal(e)),
            }
        } else {
            match board.store.login_session(&value).await {
                Ok(s) => s.map(Principal::Owner),
                Err(e) => return Err(ApiError::internal(e)),
            }
        };
        if let Some(principal) = found {
            return Ok(principal);
        }
    }
    display_principal(board, headers)
        .await?
        .ok_or_else(login_required)
}

/// The principal of a route that uses the display cookie only: the routes
/// of the display itself, and the session and logout routes with the
/// display header (R13 §5C item 6).
async fn authenticate_display(board: &Board, headers: &HeaderMap) -> Result<Principal, ApiError> {
    display_principal(board, headers)
        .await?
        .ok_or_else(login_required)
}

/// True for a route that answers the display login even when the browser
/// also holds the owner's cookie.
fn display_route(api: &ApiMethod, headers: &HeaderMap) -> bool {
    // Every `/api/displays/self...` row answers the display login only.
    api.path.starts_with("/api/displays/self")
        || (matches!(api.name, "auth.logout" | "auth.session") && display_header(headers))
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
    let principal = if methods::is_socket(api.name) {
        ws_principal(board, headers, query).await
    } else if display_route(api, headers) {
        authenticate_display(board, headers).await
    } else {
        authenticate(board, headers).await
    }
    .map_err(|e| (None, e))?;
    if matches!(principal, Principal::Member(..)) {
        if !api.member {
            return Err((
                Some(principal.actor()),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "owner_only",
                    "only the owner's board login does this",
                ),
            ));
        }
    } else if matches!(principal, Principal::Display(_)) {
        // Deny by default (R13 §5, DL-201): a row opts in with `.display()`.
        if !api.display {
            return Err((
                Some(principal.actor()),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "display_read_only",
                    "A wall display can only read.",
                ),
            ));
        }
        if api.name == "auth.logout" && !display_header(headers) {
            return Err((
                Some(principal.actor()),
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "csrf",
                    "a display logout needs the X-Fleetdeck-Display header",
                ),
            ));
        }
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
    } else if !principal.has(scope) {
        return Err((
            Some(principal.actor()),
            ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                format!("`{}` needs the {scope:?} scope", api.name).to_lowercase(),
            ),
        ));
    }
    let csrf = match &principal {
        Principal::Owner(session) => Some(session.csrf.as_str()),
        Principal::Member(_, grant) => Some(grant.csrf.as_str()),
        Principal::Token(..) | Principal::Display(_) => None,
    };
    if api.mutating
        && let Some(csrf) = csrf
    {
        let sent = headers
            .get(CSRF_HEADER)
            .map(|v| v.as_bytes())
            .unwrap_or_default();
        if !ct_eq(sent, csrf.as_bytes()) {
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
        let principal = board
            .tickets
            .redeem(ticket, board.store.now_ms())
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "login_required",
                    "the WebSocket ticket is unknown, used or expired",
                )
            })?;
        // The login or token that asked for the ticket may have ended in
        // the ticket's 30 s (logout, password change, token revoke or
        // expiry): the ticket dies with it.
        if !board
            .store
            .principal_alive(&principal)
            .await
            .map_err(ApiError::internal)?
        {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "login_required",
                "the login of this WebSocket ticket has ended",
            ));
        }
        return Ok(principal);
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
                let actor = actor.unwrap_or_else(|| "anonymous".to_owned());
                // A refusal of a display is one action with the row as its
                // target (R13 §5B item 15).
                let row = if e.code == "display_read_only" {
                    NewAudit::new(actor, "display.refused", AuditResult::Refused).target(api.name)
                } else {
                    NewAudit::new(actor, api.name, AuditResult::Refused)
                };
                board.store.audit_or_log(row.detail(e.code)).await;
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
    fn a_link_names_the_tailnet_only_when_the_board_listens_beyond_loopback() {
        let local = origins(&["127.0.0.1:7420"], Some("deck.tail1234.ts.net"));
        assert!(local.allows("https://deck.tail1234.ts.net:7420"));
        assert!(!local.list().iter().any(|o| o.contains("ts.net")));
        let tailnet = origins(
            &["127.0.0.1:7420", "100.89.30.8:7420"],
            Some("deck.tail1234.ts.net"),
        );
        assert_eq!(tailnet.list()[0], "https://deck.tail1234.ts.net:7420");
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
    fn a_device_link_lists_tailnet_then_network_then_this_computer() {
        let o = origins(
            &[
                "127.0.0.1:7420",
                "192.168.1.5:7420",
                "[fd7a:115c:a1e0::1]:7420",
            ],
            Some("deck.tail1234.ts.net"),
        );
        let list = o.list();
        let labels: Vec<String> = list.iter().map(|u| origin_label(u)).collect();
        assert_eq!(
            labels,
            [
                "Tailnet deck.tail1234.ts.net",
                "Network 192.168.1.5",
                "Network fd7a:115c:a1e0::1",
                "This computer",
                "This computer",
            ]
        );
        assert_eq!(list[0], "https://deck.tail1234.ts.net:7420");
        assert_eq!(list[1], "https://192.168.1.5:7420");
        assert!(list[3..].contains(&"https://localhost:7420".to_owned()));
        assert!(list[3..].contains(&"https://127.0.0.1:7420".to_owned()));
        for origin in &list {
            assert!(o.allows(origin), "{origin}");
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
