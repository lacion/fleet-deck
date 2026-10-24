//! The rows of the MCP App routes (B15, design `mcp-apps.md` §4.7). The
//! guard refuses a member and a display with 403 before the handler, and
//! every handler also requires the owner's board login. The two POST rows
//! are mutating on the board API, so a cookie login needs `X-CSRF-Token` and
//! a refused request is audited; their link methods are not mutating.

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, m};

/// The MCP App rows; [`super::api`] joins them with the others.
pub(super) const API_APPS: &[ApiMethod] = &[
    m(
        "sessions.app_view",
        Method::GET,
        "/api/sessions/{station}/{session}/apps/{app_id}",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.app_call",
        Method::POST,
        "/api/sessions/{station}/{session}/apps/{app_id}/call",
        Some(Scope::Drive),
        true,
    ),
    m(
        "sessions.app_calls",
        Method::GET,
        "/api/sessions/{station}/{session}/app-calls",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.app_release",
        Method::POST,
        "/api/sessions/{station}/{session}/app-calls/{call_id}/release",
        Some(Scope::Drive),
        true,
    ),
];
