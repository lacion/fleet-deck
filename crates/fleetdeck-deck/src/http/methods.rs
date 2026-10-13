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
    }
}

impl ApiMethod {
    /// The method that a member of a shared session may call too.
    const fn member(mut self) -> Self {
        self.member = true;
        self
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
    )
    .member(),
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
    )
    .member(),
    m(
        "stations.list",
        Method::GET,
        "/api/stations",
        Some(Scope::Read),
        false,
    )
    .member(),
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
    // Model help (R10, DL-167): `drive`, because it spends the owner's
    // tokens; mutating, for the CSRF check and the audit row.
    m(
        "assist.generate",
        Method::POST,
        "/api/assist/{kind}",
        Some(Scope::Drive),
        true,
    ),
    m(
        "session.digest",
        Method::POST,
        "/api/stations/{station}/sessions/{session}/digest",
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
    )
    .member(),
    m(
        "files.read",
        Method::GET,
        "/api/stations/{station}/files/read",
        Some(Scope::Drive),
        false,
    )
    .member(),
    m(
        "files.search",
        Method::GET,
        "/api/stations/{station}/files/search",
        Some(Scope::Drive),
        false,
    )
    .member(),
    m(
        "files.diff",
        Method::GET,
        "/api/stations/{station}/sessions/{session}/turns/{turn}/diff",
        Some(Scope::Drive),
        false,
    )
    .member(),
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
    )
    .member(),
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
    )
    .member(),
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
    )
    .member(),
    m(WS_METHOD, Method::GET, "/api/ws", Some(Scope::Read), false).member(),
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
    // The projects of the board and their integrations (R3, DL-164). The
    // project routes and the five writes are `admin`, and their handlers also
    // require the owner's board login (DL-138). The reads of a project's
    // issues and pull requests, and the session starts from them, are
    // `drive`.
    m(
        "projects.list",
        Method::GET,
        "/api/projects",
        Some(Scope::Admin),
        false,
    ),
    m(
        "projects.create",
        Method::POST,
        "/api/projects",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.update",
        Method::PUT,
        "/api/projects/{project}",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.delete",
        Method::DELETE,
        "/api/projects/{project}",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.preflight",
        Method::POST,
        "/api/projects/{project}/preflight",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.issues",
        Method::GET,
        "/api/projects/{project}/issues",
        Some(Scope::Drive),
        false,
    ),
    m(
        "projects.issue",
        Method::GET,
        "/api/projects/{project}/issues/{key}",
        Some(Scope::Drive),
        false,
    ),
    m(
        "projects.assets",
        Method::GET,
        "/api/projects/{project}/assets",
        Some(Scope::Drive),
        false,
    ),
    m(
        "projects.prs",
        Method::GET,
        "/api/projects/{project}/prs",
        Some(Scope::Drive),
        false,
    ),
    m(
        "projects.pr",
        Method::GET,
        "/api/projects/{project}/prs/{number}",
        Some(Scope::Drive),
        false,
    ),
    m(
        "projects.pr_checks",
        Method::GET,
        "/api/projects/{project}/prs/{number}/checks",
        Some(Scope::Drive),
        false,
    ),
    m(
        "projects.pr_create",
        Method::POST,
        "/api/projects/{project}/prs",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.pr_comment",
        Method::POST,
        "/api/projects/{project}/prs/{number}/comments",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.pr_review",
        Method::POST,
        "/api/projects/{project}/prs/{number}/reviews",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.issue_comment",
        Method::POST,
        "/api/projects/{project}/issues/{key}/comments",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.issue_transition",
        Method::POST,
        "/api/projects/{project}/issues/{key}/transition",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.sessions",
        Method::POST,
        "/api/projects/{project}/sessions",
        Some(Scope::Drive),
        true,
    ),
    m(
        "board.ws_ticket",
        Method::POST,
        "/api/ws/ticket",
        Some(Scope::Read),
        true,
    )
    .member(),
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
    )
    .member(),
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
    cmd("session.prompt", Scope::Drive).member(),
    cmd("session.steer", Scope::Drive).member(),
    cmd("session.follow_up", Scope::Drive).member(),
    cmd("session.interrupt", Scope::Drive).member(),
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
    // A member answers a question as a collaborator and a tool approval as
    // an approver; the handler decides by the request (DL-166).
    cmd("approval.answer", Scope::Approve).member(),
    // The shared fleet (R11, DL-166). The owner makes shares and spaces
    // (`admin`, and the handlers also require the owner's board login); a
    // member joins with an invite (no login needed).
    m("auth.join", Method::POST, "/api/auth/join", None, true),
    m(
        "shares.create",
        Method::POST,
        "/api/shares",
        Some(Scope::Admin),
        true,
    ),
    m(
        "shares.list",
        Method::GET,
        "/api/shares",
        Some(Scope::Admin),
        false,
    ),
    m(
        "shares.revoke",
        Method::POST,
        "/api/shares/{share_id}/revoke",
        Some(Scope::Admin),
        true,
    ),
    m(
        "spaces.create",
        Method::POST,
        "/api/spaces",
        Some(Scope::Admin),
        true,
    ),
    m(
        "spaces.list",
        Method::GET,
        "/api/spaces",
        Some(Scope::Admin),
        false,
    ),
    m(
        "spaces.add_session",
        Method::POST,
        "/api/spaces/{space_id}/sessions",
        Some(Scope::Admin),
        true,
    ),
    // The side chat and mob steering: the owner and the members of the
    // share; the handlers check the share (DL-166).
    m(
        "chat.list",
        Method::GET,
        "/api/sessions/{station}/{session}/chat",
        Some(Scope::Read),
        false,
    )
    .member(),
    m(
        "chat.post",
        Method::POST,
        "/api/sessions/{station}/{session}/chat",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "drive.request",
        Method::POST,
        "/api/sessions/{station}/{session}/drive/request",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "drive.grant",
        Method::POST,
        "/api/sessions/{station}/{session}/drive/grant",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "drive.release",
        Method::POST,
        "/api/sessions/{station}/{session}/drive/release",
        Some(Scope::Drive),
        true,
    )
    .member(),
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
        member: false,
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
                matches!(a.name, "auth.login" | "auth.link" | "auth.join"),
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

    /// The member column (R11, DL-166): exactly these methods are open to a
    /// member of a shared session, and each of their handlers checks the share.
    /// A new method is owner-only until it is added here on purpose.
    #[test]
    fn exactly_these_methods_are_open_to_a_member() {
        let mut open: Vec<&str> = API.iter().filter(|a| a.member).map(|a| a.name).collect();
        open.sort_unstable();
        assert_eq!(
            open,
            [
                "approval.answer",
                "auth.logout",
                "auth.session",
                "board.ws",
                "board.ws_ticket",
                "chat.list",
                "chat.post",
                "commands.get",
                "drive.grant",
                "drive.release",
                "drive.request",
                "files.diff",
                "files.list",
                "files.read",
                "files.search",
                "fleet.extras",
                "mail.send",
                "session.follow_up",
                "session.interrupt",
                "session.prompt",
                "session.steer",
                "sessions.list",
                "stations.list",
            ]
        );
        // The login methods need no principal at all.
        for name in [
            "shares.create",
            "shares.revoke",
            "spaces.create",
            "shells.command",
        ] {
            assert!(
                !API.iter().find(|a| a.name == name).unwrap().member,
                "{name}"
            );
        }
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
