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
    /// The axum route pattern.
    pub path: &'static str,
    /// `None`: no login needed (the login methods only).
    pub scope: Option<Scope>,
    /// A mutating method needs the CSRF token of a cookie login and is
    /// audited.
    pub mutating: bool,
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
    }
}

/// Every board-API method.
pub const API: &[ApiMethod] = &[
    m("auth.login", Method::POST, "/api/auth/login", None, true),
    m("auth.link", Method::POST, "/api/auth/link", None, true),
    m(
        "auth.logout",
        Method::POST,
        "/api/auth/logout",
        Some(Scope::Read),
        true,
    ),
    m(
        "auth.session",
        Method::GET,
        "/api/auth/session",
        Some(Scope::Read),
        false,
    ),
    m(
        "stations.list",
        Method::GET,
        "/api/stations",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.list",
        Method::GET,
        "/api/sessions",
        Some(Scope::Read),
        false,
    ),
    m(
        "commands.list",
        Method::GET,
        "/api/commands",
        Some(Scope::Read),
        false,
    ),
    m(
        "audit.list",
        Method::GET,
        "/api/audit",
        Some(Scope::Admin),
        false,
    ),
];

/// The table entry of a matched route.
#[must_use]
pub fn find(method: &Method, path: &str) -> Option<&'static ApiMethod> {
    API.iter().find(|m| m.method == *method && m.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_routes_are_unique_and_only_login_is_public() {
        for (i, a) in API.iter().enumerate() {
            for b in &API[i + 1..] {
                assert_ne!(a.name, b.name);
                assert!(a.method != b.method || a.path != b.path, "{}", a.name);
            }
            assert_eq!(
                a.scope.is_none(),
                a.name == "auth.login" || a.name == "auth.link",
                "{}",
                a.name
            );
        }
        assert_eq!(
            find(&Method::GET, "/api/audit").map(|m| m.scope),
            Some(Some(Scope::Admin))
        );
        assert_eq!(find(&Method::POST, "/api/audit"), None);
    }
}
