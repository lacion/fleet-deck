//! The second part of the board-API method table (split for the module size
//! rule); [`super::api`] joins both parts.

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, SHELL_WS_METHOD, WS_METHOD, cmd, m};

/// The rest of the board-API method table.
pub const API_REST: &[ApiMethod] = &[
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
    )
    .display(),
    m(
        "fleet.extras",
        Method::GET,
        "/api/fleet/extras",
        Some(Scope::Read),
        false,
    )
    .member()
    .display(),
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
    m(WS_METHOD, Method::GET, "/api/ws", Some(Scope::Read), false)
        .member()
        .display(),
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
    // The tailnet (DL-178): `admin`, and the handlers also require the
    // owner's board login (DL-138). Tailscale only fills the display.
    m(
        "tailnet.get",
        Method::GET,
        "/api/tailnet",
        Some(Scope::Admin),
        false,
    ),
    m(
        "tailnet.use",
        Method::POST,
        "/api/tailnet/use",
        Some(Scope::Admin),
        true,
    ),
    m(
        "tailnet.devices",
        Method::GET,
        "/api/tailnet/devices",
        Some(Scope::Admin),
        false,
    ),
    // The pairing request window and the pending requests (DL-178): `admin`,
    // and the handlers also require the owner's board login (DL-138). The
    // window and the approval also need the owner's elevation.
    m(
        "pairing.window_get",
        Method::GET,
        "/api/pairing/window",
        Some(Scope::Admin),
        false,
    ),
    m(
        "pairing.window_set",
        Method::POST,
        "/api/pairing/window",
        Some(Scope::Admin),
        true,
    ),
    m(
        "pairing.requests",
        Method::GET,
        "/api/pairing/requests",
        Some(Scope::Admin),
        false,
    ),
    m(
        "pairing.approve",
        Method::POST,
        "/api/pairing/requests/{request_id}/approve",
        Some(Scope::Admin),
        true,
    ),
    m(
        "pairing.deny",
        Method::POST,
        "/api/pairing/requests/{request_id}/deny",
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
    // The project of a session (DL-176): `drive`, owner and tokens, never a
    // member.
    m(
        "projects.session_assign",
        Method::POST,
        "/api/projects/{project}/sessions/assign",
        Some(Scope::Drive),
        true,
    ),
    // Fleet Git (design §17.1, §5.8): the settings of a project (`admin`, and
    // the handler requires the owner's board login) and "Push now" (`drive`,
    // owner and tokens, never a member).
    m(
        "projects.git_set",
        Method::PUT,
        "/api/projects/{project}/git",
        Some(Scope::Admin),
        true,
    ),
    m(
        "sessions.push_now",
        Method::POST,
        "/api/sessions/{station}/{session}/push",
        Some(Scope::Drive),
        true,
    ),
    m(
        "sessions.project_clear",
        Method::POST,
        "/api/sessions/{station}/{session}/project/clear",
        Some(Scope::Drive),
        true,
    ),
    m(
        "sessions.project_auto",
        Method::POST,
        "/api/sessions/{station}/{session}/project/auto",
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
    .member()
    .display(),
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
        "coordinator.grant_caps",
        Method::POST,
        "/api/coordinator/grants/{grant_id}/caps",
        Some(Scope::Drive),
        true,
    ),
    m(
        "coordinator.grant_give",
        Method::POST,
        "/api/coordinator/sessions/{station_id}/{session_id}/grant",
        Some(Scope::Drive),
        true,
    ),
    m(
        "coordinator.grant_classes",
        Method::POST,
        "/api/coordinator/grants/{grant_id}/classes",
        Some(Scope::Drive),
        true,
    ),
    m(
        "coordinator.desktop_ops",
        Method::GET,
        "/api/coordinator/grants/{grant_id}/desktop-ops",
        Some(Scope::Read),
        false,
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
    m(
        "session.execution_check",
        Method::GET,
        "/api/sessions/{station}/{session}/execution",
        Some(Scope::Admin),
        false,
    ),
    // The stop plan of a lost run (BG-4); owner only, as the check.
    m(
        "session.execution_stop_plan",
        Method::GET,
        "/api/sessions/{station}/{session}/execution/stop-plan",
        Some(Scope::Admin),
        false,
    ),
    m(
        "session.fleet_answers",
        Method::GET,
        "/api/sessions/{station}/{session}/fleet-answers",
        Some(Scope::Admin),
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
    // The owner marks the previous run of a session as ended (DL-182); the
    // handler refuses a token (`owner_only`).
    cmd("session.reconcile_execution", Scope::Admin),
    // The owner stops the listed processes of a lost run (BG-4); the handler
    // also needs the feature and an elevation.
    cmd("session.stop_execution", Scope::Admin),
    // The owner runs a `!` shell command in the session folder, and releases
    // a folder (B8); the handler refuses a token and a member (`owner_only`).
    cmd("session.shell_command", Scope::Admin),
    cmd("session.release_shell", Scope::Admin),
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
        "shares.history",
        Method::POST,
        "/api/shares/{share_id}/history",
        Some(Scope::Admin),
        true,
    ),
    // The earlier history of an imported session (DL-189): the owner, and a
    // member only through a share with the history option.
    m(
        "sessions.history",
        Method::GET,
        "/api/sessions/{station}/{session}/history",
        Some(Scope::Read),
        false,
    )
    .member(),
    // The work around a session (DL-191): the owner only, never a member.
    m(
        "sessions.subagent",
        Method::GET,
        "/api/sessions/{station}/{session}/subagents/{tool_use_id}",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.branches",
        Method::GET,
        "/api/sessions/{station}/{session}/branches",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.branch_diff",
        Method::GET,
        "/api/sessions/{station}/{session}/branches/{head}/diff",
        Some(Scope::Read),
        false,
    ),
    // Browse the native sessions of a station (B7): the owner only, never a
    // member.
    m(
        "stations.native_sessions",
        Method::GET,
        "/api/stations/{station}/native-sessions",
        Some(Scope::Read),
        false,
    ),
    m(
        "stations.native_preview",
        Method::POST,
        "/api/stations/{station}/native-sessions/preview",
        Some(Scope::Read),
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
    // Review comments on the Changes diff (DL-179): a read needs `read`, a
    // write `drive`; the handlers check the share, the role and the author.
    m(
        "review.list",
        Method::GET,
        "/api/sessions/{station}/{session}/review",
        Some(Scope::Read),
        false,
    )
    .member(),
    m(
        "review.comment",
        Method::POST,
        "/api/sessions/{station}/{session}/review/comments",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "review.edit",
        Method::PATCH,
        "/api/sessions/{station}/{session}/review/comments/{comment}",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "review.delete",
        Method::DELETE,
        "/api/sessions/{station}/{session}/review/comments/{comment}",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "review.send",
        Method::POST,
        "/api/sessions/{station}/{session}/review/send",
        Some(Scope::Drive),
        true,
    )
    .member(),
    m(
        "review.retry",
        Method::POST,
        "/api/sessions/{station}/{session}/review/reviews/{review}/send-again",
        Some(Scope::Drive),
        true,
    )
    .member(),
    // The turn diff by checkpoint commit pair, the stable selector.
    m(
        "files.turn_diff",
        Method::GET,
        "/api/stations/{station}/sessions/{session}/turn-diff",
        Some(Scope::Drive),
        false,
    )
    .member(),
    // The display principal (R13 §5, DL-201). The pairing start, the pairing
    // poll and the display link need no login: the poll uses the pending
    // cookie and the link is its own proof. The owner rows are `admin` or
    // `drive`, and every handler also requires the owner's board login.
    m(
        "displays.pair_start",
        Method::POST,
        "/api/displays/pair",
        None,
        true,
    ),
    m(
        "displays.pair_poll",
        Method::GET,
        "/api/displays/pair/{id}",
        None,
        false,
    ),
    m(
        "auth.display_link",
        Method::POST,
        "/api/auth/display-link",
        None,
        true,
    ),
    m(
        "displays.list",
        Method::GET,
        "/api/displays",
        Some(Scope::Read),
        false,
    ),
    m(
        "displays.pair_approve",
        Method::POST,
        "/api/displays/pair/{id}/approve",
        Some(Scope::Admin),
        true,
    ),
    m(
        "displays.link_create",
        Method::POST,
        "/api/displays/link",
        Some(Scope::Admin),
        true,
    ),
    m(
        "displays.settings",
        Method::POST,
        "/api/displays/{id}/settings",
        Some(Scope::Admin),
        true,
    ),
    m(
        "displays.revoke",
        Method::POST,
        "/api/displays/{id}/revoke",
        Some(Scope::Drive),
        true,
    ),
    m(
        "displays.self",
        Method::GET,
        "/api/displays/self",
        Some(Scope::Read),
        false,
    )
    .display(),
];
