//! The board-API method table (L15, PLAN §9): every route declares its
//! required scope and whether it mutates. The guard refuses a route that
//! is not in this table, so a new route without a declared scope fails
//! closed.

use axum::http::Method;
use fleetdeck_proto::link::Scope;

/// One board-API method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiMethod {
    /// The audit name, for example `auth.login`.
    pub name: &'static str,
    pub method: Method,
    /// The axum route pattern; for a command method the method name
    /// (see [`route_of`]).
    pub path: &'static str,
    /// `None`: no login needed (the login methods only).
    pub scope: Option<Scope>,
    /// A mutating method needs the CSRF token of a cookie login and is
    /// audited.
    pub mutating: bool,
    /// A member of a shared session may call the method (R11, DL-166). The
    /// handler then checks the share: the session and the role. `false`: the
    /// guard refuses a member with 403 `owner_only`.
    pub member: bool,
    /// A wall display may call the method (R13 §5, DL-201). `false`: the
    /// guard refuses a display with 403 `display_read_only`.
    pub display: bool,
}

const fn m(
    name: &'static str,
    method: Method,
    path: &'static str,
    scope: Option<Scope>,
    mutating: bool,
) -> ApiMethod {
    ApiMethod {
        name,
        method,
        path,
        scope,
        mutating,
        member: false,
        display: false,
    }
}

impl ApiMethod {
    /// The method that a member of a shared session may call too.
    const fn member(mut self) -> Self {
        self.member = true;
        self
    }

    /// The method that a wall display may call too (R13 §5): a read row
    /// whose handler shows nothing beyond the display projection.
    const fn display(mut self) -> Self {
        self.display = true;
        self
    }
}

/// The board WebSocket: the guard authenticates it by ticket or by a
/// cookie with an `Origin` header (P3-9b).
pub const WS_METHOD: &str = "board.ws";
/// The WebSocket of one human shell (PLAN §6.8).
pub const SHELL_WS_METHOD: &str = "shells.attach";
/// The WebSocket of one maintenance run (BG-1, `board-gaps.md` section 2.9).
pub const MAINTENANCE_WS_METHOD: &str = "station.maintenance_attach";

/// True for a WebSocket method: its login is a ticket or a cookie with an
/// `Origin` of this deck's board.
#[must_use]
pub fn is_socket(name: &str) -> bool {
    name == WS_METHOD || name == SHELL_WS_METHOD || name == MAINTENANCE_WS_METHOD
}

#[path = "methods_table.rs"]
mod table;
#[path = "methods_table_display.rs"]
mod table_display;
#[path = "methods_table_git.rs"]
mod table_git;
#[path = "methods_table_reports.rs"]
mod table_reports;
#[path = "methods_table_rest.rs"]
mod table_rest;
#[path = "methods_table_tools.rs"]
mod table_tools;

/// Every board-API method, in table order.
pub fn api() -> impl Iterator<Item = &'static ApiMethod> {
    table::API
        .iter()
        .chain(table_rest::API_REST.iter())
        .chain(table_git::API_GIT.iter())
        .chain(table_display::API_DISPLAY.iter())
        .chain(table_reports::API_REPORTS.iter())
        .chain(table_tools::API_TOOLS.iter())
}

/// The route prefix of the command methods.
pub const COMMAND_PREFIX: &str = "/api/commands/";

/// A command method: `POST /api/commands/<method>` with the scope of the
/// link method of the same name (checked by a test). Its `path` is the
/// method name; [`route_of`] adds [`COMMAND_PREFIX`].
const fn cmd(name: &'static str, scope: Scope) -> ApiMethod {
    ApiMethod {
        name,
        method: Method::POST,
        path: name,
        scope: Some(scope),
        mutating: true,
        member: false,
        display: false,
    }
}

/// The axum route of a table entry.
#[must_use]
pub fn route_of(api: &ApiMethod) -> String {
    if api.path.starts_with('/') {
        api.path.to_owned()
    } else {
        format!("{COMMAND_PREFIX}{}", api.path)
    }
}

/// The command methods of the table.
pub fn command_methods() -> impl Iterator<Item = &'static ApiMethod> {
    api().filter(|a| !a.path.starts_with('/'))
}

/// The table entry of a matched route.
#[must_use]
pub fn find(method: &Method, path: &str) -> Option<&'static ApiMethod> {
    let key = path
        .strip_prefix(COMMAND_PREFIX)
        .filter(|k| !k.starts_with('{'));
    api().find(|m| m.method == *method && (m.path == path || key.is_some_and(|k| m.path == k)))
}

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;
