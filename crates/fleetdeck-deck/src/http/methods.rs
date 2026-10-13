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

/// The board WebSocket: the guard authenticates it by ticket or by a
/// cookie with an `Origin` header (P3-9b).
pub const WS_METHOD: &str = "board.ws";
/// The WebSocket of one human shell (PLAN §6.8).
pub const SHELL_WS_METHOD: &str = "shells.attach";

/// True for a WebSocket method: its login is a ticket or a cookie with an
/// `Origin` of this deck's board.
#[must_use]
pub fn is_socket(name: &str) -> bool {
    name == WS_METHOD || name == SHELL_WS_METHOD
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
    // A device link (DL-146): `admin`, and the handler also requires the
    // owner's board login (DL-138).
    m(
        "auth.device_link",
        Method::POST,
        "/api/auth/device-link",
        Some(Scope::Admin),
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
        "stations.add",
        Method::POST,
        "/api/stations",
        Some(Scope::Admin),
        true,
    ),
    m(
        "stations.invite",
        Method::POST,
        "/api/stations/invite",
        Some(Scope::Admin),
        true,
    ),
    m(
        "stations.remove",
        Method::POST,
        "/api/stations/{station}/remove",
        Some(Scope::Admin),
        true,
    ),
    m(
        "stations.workspaces",
        Method::GET,
        "/api/stations/{station}/workspaces",
        Some(Scope::Drive),
        false,
    ),
    m(
        "workspace.preflight",
        Method::POST,
        "/api/stations/{station}/workspace/preflight",
        Some(Scope::Drive),
        true,
    ),
    // A pasted image (DL-147): `admin`, and the handler also requires the
    // owner's board login (DL-138).
    m(
        "files.paste",
        Method::POST,
        "/api/stations/{station}/paste",
        Some(Scope::Admin),
        true,
    ),
    m(
        "files.list",
        Method::GET,
        "/api/stations/{station}/files/list",
        Some(Scope::Drive),
        false,
    ),
    m(
        "files.read",
        Method::GET,
        "/api/stations/{station}/files/read",
        Some(Scope::Drive),
        false,
    ),
    m(
        "files.search",
        Method::GET,
        "/api/stations/{station}/files/search",
        Some(Scope::Drive),
        false,
    ),
    m(
        "files.diff",
        Method::GET,
        "/api/stations/{station}/sessions/{session}/turns/{turn}/diff",
        Some(Scope::Drive),
        false,
    ),
    // Artifacts (R12, DL-165). Reads need `read`; a publish needs `drive`;
    // a share link is `admin` and the handler also requires the owner's
    // board login (DL-138). The board's own view link only reads, but it
    // writes a short-lived row, so it is mutating and needs the CSRF token.
    m(
        "artifacts.list",
        Method::GET,
        "/api/artifacts",
        Some(Scope::Read),
        false,
    ),
    m(
        "artifacts.publish",
        Method::POST,
        "/api/artifacts/publish",
        Some(Scope::Drive),
        true,
    ),
    m(
        "artifacts.get",
        Method::GET,
        "/api/artifacts/{station}/{id}",
        Some(Scope::Read),
        false,
    ),
    m(
        "artifacts.raw",
        Method::GET,
        "/api/artifacts/{station}/{id}/{version}/raw/{*path}",
        Some(Scope::Read),
        false,
    ),
    m(
        "artifacts.view",
        Method::POST,
        "/api/artifacts/{station}/{id}/view",
        Some(Scope::Read),
        true,
    ),
    m(
        "artifacts.share",
        Method::POST,
        "/api/artifacts/{station}/{id}/shares",
        Some(Scope::Admin),
        true,
    ),
    m(
        "artifacts.share_revoke",
        Method::DELETE,
        "/api/artifacts/{station}/{id}/shares/{token}",
        Some(Scope::Admin),
        true,
    ),
    m(
        "files.session_diff",
        Method::GET,
        "/api/stations/{station}/sessions/{session}/diff",
        Some(Scope::Read),
        false,
    ),
    m(
        "conflicts.list",
        Method::GET,
        "/api/conflicts",
        Some(Scope::Read),
        false,
    ),
    // The archived sessions of the board (DL-149): a bearer token with `drive`
    // may change the set, because a change only hides cards.
    m(
        "archive.list",
        Method::GET,
        "/api/archive",
        Some(Scope::Read),
        false,
    ),
    m(
        "archive.change",
        Method::POST,
        "/api/archive",
        Some(Scope::Drive),
        true,
    ),
    m(
        "plans.list",
        Method::GET,
        "/api/plans",
        Some(Scope::Read),
        false,
    ),
    m(
        "plans.mark",
        Method::POST,
        "/api/plans/mark",
        Some(Scope::Drive),
        true,
    ),
    // The automations (R5, DL-168): `admin`, and the handlers also require
    // the owner's board login (DL-138). The board client sends only GET and
    // POST, so an update and a delete also have a POST route.
    m(
        "schedules.list",
        Method::GET,
        "/api/schedules",
        Some(Scope::Admin),
        false,
    ),
    m(
        "schedules.create",
        Method::POST,
        "/api/schedules",
        Some(Scope::Admin),
        true,
    ),
    m(
        "schedules.update",
        Method::PUT,
        "/api/schedules/{schedule_id}",
        Some(Scope::Admin),
        true,
    ),
    m(
        "schedules.delete",
        Method::DELETE,
        "/api/schedules/{schedule_id}",
        Some(Scope::Admin),
        true,
    ),
    m(
        "schedules.update_post",
        Method::POST,
        "/api/schedules/{schedule_id}/update",
        Some(Scope::Admin),
        true,
    ),
    m(
        "schedules.delete_post",
        Method::POST,
        "/api/schedules/{schedule_id}/delete",
        Some(Scope::Admin),
        true,
    ),
    m(
        "schedules.run",
        Method::POST,
        "/api/schedules/{schedule_id}/run",
        Some(Scope::Admin),
        true,
    ),
    m(
        "schedules.runs",
        Method::GET,
        "/api/schedules/{schedule_id}/runs",
        Some(Scope::Admin),
        false,
    ),
    m(
        "sessions.list",
        Method::GET,
        "/api/sessions",
        Some(Scope::Read),
        false,
    ),
    // The read cursor of the caller only (R6.1, DL-169): `read` is enough.
    m(
        "sessions.read",
        Method::POST,
        "/api/sessions/{station}/{session}/read",
        Some(Scope::Read),
        true,
    ),
    m(
        "activity.list",
        Method::GET,
        "/api/activity",
        Some(Scope::Read),
        false,
    ),
    m(
        "fleet.extras",
        Method::GET,
        "/api/fleet/extras",
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
    m(
        "commands.get",
        Method::GET,
        "/api/commands/{command_id}",
        Some(Scope::Read),
        false,
    ),
    m(WS_METHOD, Method::GET, "/api/ws", Some(Scope::Read), false),
    // The worktree inventory (DL-142): `admin`, and the handler also
    // requires the owner's board login (DL-138).
    m(
        "stations.worktrees",
        Method::GET,
        "/api/stations/{station}/worktrees",
        Some(Scope::Admin),
        false,
    ),
    // The worktree trash (DL-150): `admin`, and the handlers also require
    // the owner's board login (DL-138).
    m(
        "worktree.trash",
        Method::POST,
        "/api/stations/{station}/worktrees/trash",
        Some(Scope::Admin),
        true,
    ),
    m(
        "worktree.restore",
        Method::POST,
        "/api/stations/{station}/worktrees/restore",
        Some(Scope::Admin),
        true,
    ),
    m(
        "worktree.purge",
        Method::POST,
        "/api/stations/{station}/worktrees/purge",
        Some(Scope::Admin),
        true,
    ),
    m(
        "worktree.release",
        Method::POST,
        "/api/stations/{station}/worktrees/release",
        Some(Scope::Admin),
        true,
    ),
    // The human shell (PLAN §6.8, DL-404): `admin`, and the handlers also
    // require the owner's board login (DL-138).
    m(
        "shells.list",
        Method::GET,
        "/api/stations/{station}/shells",
        Some(Scope::Admin),
        false,
    ),
    m(
        "shells.command",
        Method::POST,
        "/api/stations/{station}/shells",
        Some(Scope::Admin),
        true,
    ),
    m(
        SHELL_WS_METHOD,
        Method::GET,
        "/api/stations/{station}/shells/{shell}/attach",
        Some(Scope::Admin),
        false,
    ),
    m(
        "board.ws_ticket",
        Method::POST,
        "/api/ws/ticket",
        Some(Scope::Read),
        true,
    ),
    m(
        "session.assign",
        Method::POST,
        "/api/assign",
        Some(Scope::Drive),
        true,
    ),
    m(
        "mail.list",
        Method::GET,
        "/api/mail",
        Some(Scope::Read),
        false,
    ),
    m(
        "mail.send",
        Method::POST,
        "/api/mail",
        Some(Scope::Drive),
        true,
    ),
    m(
        "coordinator.grants",
        Method::GET,
        "/api/coordinator/grants",
        Some(Scope::Read),
        false,
    ),
    m(
        "coordinator.revoke",
        Method::POST,
        "/api/coordinator/grants/{grant_id}/revoke",
        Some(Scope::Drive),
        true,
    ),
    m(
        "coordinator.renew",
        Method::POST,
        "/api/coordinator/grants/{grant_id}/renew",
        Some(Scope::Drive),
        true,
    ),
    m(
        "mail.send_group",
        Method::POST,
        "/api/mail/group",
        Some(Scope::Drive),
        true,
    ),
    m(
        "tasks.list",
        Method::GET,
        "/api/tasks",
        Some(Scope::Read),
        false,
    ),
    m(
        "tasks.create",
        Method::POST,
        "/api/tasks",
        Some(Scope::Drive),
        true,
    ),
    m(
        "tasks.complete",
        Method::POST,
        "/api/tasks/{task_id}/complete",
        Some(Scope::Drive),
        true,
    ),
    m(
        "tasks.wait",
        Method::GET,
        "/api/tasks/{task_id}/wait",
        Some(Scope::Read),
        false,
    ),
    m(
        "tasks.wait_any",
        Method::GET,
        "/api/tasks/wait",
        Some(Scope::Read),
        false,
    ),
    cmd("session.start", Scope::Drive),
    cmd("session.resume", Scope::Drive),
    cmd("session.import", Scope::Drive),
    cmd("session.prompt", Scope::Drive),
    cmd("session.steer", Scope::Drive),
    cmd("session.follow_up", Scope::Drive),
    cmd("session.interrupt", Scope::Drive),
    cmd("session.set_mode", Scope::Drive),
    cmd("session.set_model", Scope::Drive),
    cmd("session.fork", Scope::Drive),
    cmd("session.stop", Scope::Drive),
    cmd("session.revert", Scope::Drive),
    cmd("session.rename", Scope::Drive),
    cmd("session.resolve_revert", Scope::Drive),
    cmd("session.set_auto_continue", Scope::Drive),
    // `session.coordinator_renew` / `_revoke` have no generic route: only
    // the owner-checked grant routes send them (ADR-006 §3, DL-138).
    cmd("approval.answer", Scope::Approve),
];

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
    API.iter().filter(|a| !a.path.starts_with('/'))
}

/// The table entry of a matched route.
#[must_use]
pub fn find(method: &Method, path: &str) -> Option<&'static ApiMethod> {
    let key = path
        .strip_prefix(COMMAND_PREFIX)
        .filter(|k| !k.starts_with('{'));
    API.iter()
        .find(|m| m.method == *method && (m.path == path || key.is_some_and(|k| m.path == k)))
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
        assert_eq!(
            find(&Method::GET, "/api/fleet/extras").map(|m| (m.name, m.scope, m.mutating)),
            Some(("fleet.extras", Some(Scope::Read), false))
        );
        assert_eq!(
            find(&Method::POST, "/api/commands/approval.answer").map(|m| m.scope),
            Some(Some(Scope::Approve))
        );
        assert_eq!(
            find(&Method::GET, "/api/commands/{command_id}").map(|m| m.name),
            Some("commands.get")
        );
        assert_eq!(find(&Method::POST, "/api/commands/session.nope"), None);
    }

    #[test]
    fn every_command_method_has_the_scope_of_its_link_method() {
        let mut n = 0;
        for api in command_methods() {
            let link = fleetdeck_link::methods::method(api.name)
                .unwrap_or_else(|| panic!("{} is not a link method", api.name));
            assert_eq!(api.scope, Some(link.scope), "{}", api.name);
            assert!(api.mutating);
            n += 1;
        }
        assert_eq!(n, 16);
    }
}
