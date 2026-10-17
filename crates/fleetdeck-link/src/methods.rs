//! The link method table and the authorization of control frames
//! (ADR-004 §9.6, GATE-M1 G6 and G7).
//!
//! Every check uses the roles that were bound at pairing (the role of the
//! pinned peer key and the own role), never the dial direction. The table
//! must equal the table in ADR-004 §9.6; `tests/method_table.rs` compares
//! them.

use std::collections::BTreeSet;

use fleetdeck_proto::link::{Scope, WireError};
use fleetdeck_proto::version::{Negotiated, feature};

use crate::tls::Role;

/// Who may call a method, by the roles bound at pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callers {
    /// The deck calls, the station answers.
    DeckToStation,
    /// The station calls, the deck answers.
    StationToDeck,
    /// Either side calls the other.
    Either,
}

impl Callers {
    /// True if a node with role `caller` may call a node with role `receiver`.
    #[must_use]
    pub fn allows(self, caller: Role, receiver: Role) -> bool {
        match self {
            Self::DeckToStation => caller == Role::Deck && receiver == Role::Station,
            Self::StationToDeck => caller == Role::Station && receiver == Role::Deck,
            Self::Either => caller != receiver,
        }
    }
}

/// One row of the method table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub callers: Callers,
    pub scope: Scope,
    /// A feature that both sides must have negotiated.
    pub feature: Option<&'static str>,
    /// A mutating method needs a `command_id` and a typed envelope.
    pub mutating: bool,
}

const fn m(
    name: &'static str,
    callers: Callers,
    scope: Scope,
    feature: Option<&'static str>,
    mutating: bool,
) -> MethodSpec {
    MethodSpec {
        name,
        callers,
        scope,
        feature,
        mutating,
    }
}

const fn drive(name: &'static str) -> MethodSpec {
    m(name, Callers::DeckToStation, Scope::Drive, None, true)
}

use Callers::{DeckToStation, Either, StationToDeck};

/// Method table v1 (Revision 2).
pub const METHODS: &[MethodSpec] = &[
    m("station.info", DeckToStation, Scope::Read, None, false),
    m("session.list", DeckToStation, Scope::Read, None, false),
    m("events.replay", DeckToStation, Scope::Read, None, false),
    m(
        "session.snapshot",
        DeckToStation,
        Scope::Read,
        Some(feature::SNAPSHOTS),
        false,
    ),
    m(
        "command.receipt",
        DeckToStation,
        Scope::Read,
        Some(feature::COMMAND_RECEIPTS),
        false,
    ),
    m(
        "artifact.open",
        DeckToStation,
        Scope::Read,
        Some(feature::DATA_STREAMS),
        false,
    ),
    // The spawn form's directory picker (M2 P4-4, DL-061): a read with the
    // drive scope, because only a caller that may start a session needs it.
    m("workspace.list", DeckToStation, Scope::Drive, None, false),
    // The read-only inventory of the session worktrees (DL-142).
    m(
        "workspace.worktrees",
        DeckToStation,
        Scope::Drive,
        Some(feature::WORKTREES),
        false,
    ),
    // The Git access check before a clone (DL-144): it reads the remote
    // with the station's credentials, so it needs the drive scope.
    m(
        "workspace.preflight",
        DeckToStation,
        Scope::Drive,
        Some(feature::PREFLIGHT),
        false,
    ),
    // The shell precheck of a schedule (R5, DL-168): it runs the owner's
    // shell command on the station, so it needs `admin` (like a setup
    // command, DL-148) and the station's `allow_setup_cmd`.
    m(
        "workspace.precheck",
        DeckToStation,
        Scope::Admin,
        Some(feature::PRECHECK),
        false,
    ),
    // A pasted image on the station (DL-147). Not a receipt mutation: the
    // upload id makes a retry return the same path.
    m(
        "files.write_temp",
        DeckToStation,
        Scope::Admin,
        Some(feature::WRITE_TEMP),
        false,
    ),
    // GitHub, GitLab, Jira and Linear through the station (R3, DL-164). The
    // reads use the station's credentials, so a caller needs `read` at most
    // for a list and `drive` for a write. A write carries its `command_id`
    // inside its params and is not a receipt mutation: the station keeps the
    // answers of the last 500 writes in memory.
    m(
        "integrations.status",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "issues.search",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "issues.get",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "issues.asset",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "prs.list",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "prs.get",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "prs.checks",
        DeckToStation,
        Scope::Read,
        Some(feature::INTEGRATIONS),
        false,
    ),
    // The fetch writes only a local ref of the repository (R3.7); it needs
    // `drive` like a write, and it is not a receipt mutation.
    m(
        "prs.fetch_head",
        DeckToStation,
        Scope::Drive,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "prs.create",
        DeckToStation,
        Scope::Drive,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "prs.comment",
        DeckToStation,
        Scope::Drive,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "prs.review",
        DeckToStation,
        Scope::Drive,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "issues.comment",
        DeckToStation,
        Scope::Drive,
        Some(feature::INTEGRATIONS),
        false,
    ),
    m(
        "issues.transition",
        DeckToStation,
        Scope::Drive,
        Some(feature::INTEGRATIONS),
        false,
    ),
    // Model help on the station (R10, DL-167). It runs a model call on the
    // owner's subscription, so it needs `drive`; it changes no session.
    m(
        "assist.generate",
        DeckToStation,
        Scope::Drive,
        Some(feature::ASSIST),
        false,
    ),
    // The settings of a station and its restart (DL-172). They change the
    // station's policy and its process, so they need `admin`. None is a
    // receipt mutation: the revision makes a retry of a save safe, and a
    // secret never goes through the receipts (the credential relay).
    m(
        "settings.get",
        DeckToStation,
        Scope::Admin,
        Some(feature::SETTINGS),
        false,
    ),
    m(
        "settings.set",
        DeckToStation,
        Scope::Admin,
        Some(feature::SETTINGS),
        false,
    ),
    m(
        "settings.secret_set",
        DeckToStation,
        Scope::Admin,
        Some(feature::SETTINGS),
        false,
    ),
    m(
        "station.restart",
        DeckToStation,
        Scope::Admin,
        Some(feature::SETTINGS),
        false,
    ),
    m(
        "files.list",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "files.read",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "files.search",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "files.diff",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    // The working tree of a session against its base commit (R1.2).
    m(
        "files.session_diff",
        DeckToStation,
        Scope::Read,
        Some(feature::SESSION_DIFF),
        false,
    ),
    // The earlier conversation of a session, from the station's snapshot of
    // the provider transcript (DL-189).
    m(
        "session.history",
        DeckToStation,
        Scope::Read,
        Some(feature::HISTORY),
        false,
    ),
    // The log of one subagent of a session, from the provider's transcript of
    // that subagent (DL-191).
    m(
        "session.subagent",
        DeckToStation,
        Scope::Read,
        Some(feature::SUBAGENT),
        false,
    ),
    // The native sessions of the station's provider homes, and the preview of
    // one (B7).
    m(
        "station.native_sessions",
        DeckToStation,
        Scope::Read,
        Some(feature::NATIVE_SESSIONS),
        false,
    ),
    m(
        "station.native_preview",
        DeckToStation,
        Scope::Read,
        Some(feature::NATIVE_SESSIONS),
        false,
    ),
    // The branches of the repository of a session, and the committed diff of
    // one of them (DL-191).
    m(
        "session.branches",
        DeckToStation,
        Scope::Read,
        Some(feature::SESSION_BRANCHES),
        false,
    ),
    m(
        "files.branch_diff",
        DeckToStation,
        Scope::Read,
        Some(feature::SESSION_BRANCHES),
        false,
    ),
    // The diff of a session for a coordinator, with no old-side byte (ADR-008
    // §4.4).
    m(
        "files.coordinator_diff",
        DeckToStation,
        Scope::Read,
        Some(feature::COORDINATOR_V2),
        false,
    ),
    m(
        "shell.list",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        false,
    ),
    m(
        "shell.attach",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        false,
    ),
    m(
        "shell.open",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        true,
    ),
    m(
        "shell.resize",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        true,
    ),
    m(
        "shell.close",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        true,
    ),
    m(
        "shell.handover",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL_HANDOVER),
        true,
    ),
    m(
        "shell.release",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL_HANDOVER),
        true,
    ),
    // The artifact service (R12, DL-165). A publish copies bytes into the
    // station's data directory, so it needs `drive`; the reads need `read`.
    // None is a receipt mutation: a publish of identical content makes no
    // new version, so a retry is safe.
    m(
        "artifacts.publish",
        DeckToStation,
        Scope::Drive,
        Some(feature::ARTIFACTS),
        false,
    ),
    m(
        "artifacts.list",
        DeckToStation,
        Scope::Read,
        Some(feature::ARTIFACTS),
        false,
    ),
    m(
        "artifacts.get",
        DeckToStation,
        Scope::Read,
        Some(feature::ARTIFACTS),
        false,
    ),
    m(
        "artifacts.file",
        DeckToStation,
        Scope::Read,
        Some(feature::ARTIFACTS),
        false,
    ),
    // The accounts of a station (DL-173). They change what the station may
    // use, so they need `admin`. None is a receipt mutation: the credential
    // relay never goes through commands, the outbox or receipts. `app_set`,
    // `token_set` and `oauth_finish` carry a secret in their params.
    m(
        "accounts.list",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.forge_login",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.forge_login_status",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.logout",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.app_set",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.token_set",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.oauth_start",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "accounts.oauth_finish",
        DeckToStation,
        Scope::Admin,
        Some(feature::ACCOUNTS),
        false,
    ),
    m(
        "issues.containers",
        DeckToStation,
        Scope::Read,
        Some(feature::ACCOUNTS),
        false,
    ),
    drive("session.start"),
    drive("session.resume"),
    drive("session.fork"),
    drive("session.prompt"),
    drive("session.steer"),
    drive("session.follow_up"),
    drive("session.interrupt"),
    drive("session.set_mode"),
    drive("session.set_model"),
    drive("session.stop"),
    m(
        "session.revert",
        DeckToStation,
        Scope::Drive,
        Some(feature::REVERT),
        true,
    ),
    m(
        "session.resolve_revert",
        DeckToStation,
        Scope::Drive,
        Some(feature::REVERT),
        true,
    ),
    m(
        "session.set_auto_continue",
        DeckToStation,
        Scope::Drive,
        Some(feature::AUTO_CONTINUE),
        true,
    ),
    m(
        "session.coordinator_renew",
        DeckToStation,
        Scope::Drive,
        Some(feature::COORDINATOR),
        true,
    ),
    m(
        "session.coordinator_revoke",
        DeckToStation,
        Scope::Drive,
        Some(feature::COORDINATOR),
        true,
    ),
    m(
        "session.import",
        DeckToStation,
        Scope::Drive,
        Some(feature::IMPORT),
        true,
    ),
    m(
        "session.rename",
        DeckToStation,
        Scope::Drive,
        Some(feature::RENAME),
        true,
    ),
    // The worktree trash (DL-150): it moves and deletes folders, so `admin`.
    m(
        "session.worktree_trash",
        DeckToStation,
        Scope::Admin,
        Some(feature::TRASH),
        true,
    ),
    m(
        "session.worktree_restore",
        DeckToStation,
        Scope::Admin,
        Some(feature::TRASH),
        true,
    ),
    m(
        "session.worktree_purge",
        DeckToStation,
        Scope::Admin,
        Some(feature::TRASH),
        true,
    ),
    m(
        "session.worktree_release",
        DeckToStation,
        Scope::Admin,
        Some(feature::TRASH),
        true,
    ),
    // Resume on the shim host (DL-182): the check reads processes of the
    // station (`read`); marking a run as ended is an owner action (`admin`).
    m(
        "session.execution_check",
        DeckToStation,
        Scope::Read,
        Some(feature::SHIM_RESUME),
        false,
    ),
    m(
        "session.reconcile_execution",
        DeckToStation,
        Scope::Admin,
        Some(feature::SHIM_RESUME),
        true,
    ),
    // `!` shell commands (B8, DL-196): the station runs a command with its own
    // rights, so both methods are owner actions (`admin`).
    m(
        "session.shell_command",
        DeckToStation,
        Scope::Admin,
        Some(feature::SHELL_COMMAND),
        true,
    ),
    m(
        "session.release_shell",
        DeckToStation,
        Scope::Admin,
        Some(feature::SHELL_COMMAND),
        true,
    ),
    // Desktop control on the station (R13 P3): the board gates it for the
    // owner; the station checks `allow_desktop` live.
    m(
        "desktop.info",
        DeckToStation,
        Scope::Read,
        Some(feature::DESKTOP),
        false,
    ),
    // The deck authorizes each action: the row needs `desktop_authority.v1`
    // (R13 §5E item 1).
    m(
        "desktop.act",
        DeckToStation,
        Scope::Drive,
        Some(feature::DESKTOP_AUTHORITY),
        false,
    ),
    // Protected change statistics and patches for a wall display (R13 §5D).
    m(
        "files.display_changes",
        DeckToStation,
        Scope::Read,
        Some(feature::DISPLAY_CHANGES),
        false,
    ),
    m("approval.answer", DeckToStation, Scope::Approve, None, true),
    m("station.configure", DeckToStation, Scope::Admin, None, true),
    m("link.rotate_key", Either, Scope::Admin, None, true),
    m(
        "coordinator.query",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR),
        false,
    ),
    m(
        "coordinator.call",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR),
        true,
    ),
    // ADR-008 §6.2: the target station asks the deck whether the authority of
    // a routed command still holds, and the deck asks the caller station for
    // its live policy.
    m(
        "coordinator.authority_check",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR_V2),
        false,
    ),
    m(
        "coordinator.policy_now",
        DeckToStation,
        Scope::Read,
        Some(feature::COORDINATOR_V2),
        false,
    ),
];

/// The row of `name`.
#[must_use]
pub fn method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|s| s.name == name)
}

/// The request error codes of ADR-004 §2.11 and §9.8.
pub mod code {
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    pub const WRONG_ROLE: &str = "wrong_role";
    pub const FEATURE_NOT_NEGOTIATED: &str = "feature_not_negotiated";
    pub const FORBIDDEN: &str = "forbidden";
    pub const MISSING_COMMAND_ID: &str = "missing_command_id";
    pub const TOO_MANY_REQUESTS: &str = "too_many_requests";
    pub const PROTOCOL_ERROR: &str = "protocol_error";
    pub const TIMEOUT: &str = "timeout";
}

/// Checks a request in the ADR order: `unknown_method`, `wrong_role`,
/// `feature_not_negotiated`, `forbidden`, `missing_command_id`.
///
/// `granted` is `None` for the caller's own pre-check, which cannot know the
/// scopes that the receiver granted.
pub fn authorize(
    name: &str,
    caller: Role,
    receiver: Role,
    command_id: Option<&str>,
    negotiated: &Negotiated,
    granted: Option<&BTreeSet<Scope>>,
) -> Result<&'static MethodSpec, WireError> {
    let spec = method(name)
        .ok_or_else(|| WireError::new(code::UNKNOWN_METHOD, format!("no link method `{name}`")))?;
    if !spec.callers.allows(caller, receiver) {
        return Err(WireError::new(
            code::WRONG_ROLE,
            format!(
                "a {} cannot call `{name}` on a {}",
                role_name(caller),
                role_name(receiver)
            ),
        ));
    }
    if let Some(f) = spec.feature
        && !negotiated.has(f)
    {
        return Err(WireError::new(
            code::FEATURE_NOT_NEGOTIATED,
            format!("`{name}` needs feature {f}, which this link did not negotiate"),
        ));
    }
    if let Some(granted) = granted
        && !granted.contains(&spec.scope)
    {
        return Err(WireError::new(
            code::FORBIDDEN,
            format!(
                "`{name}` needs scope {}, which the pairing did not grant",
                spec.scope.name()
            ),
        ));
    }
    if spec.mutating && command_id.is_none_or(str::is_empty) {
        return Err(WireError::new(
            code::MISSING_COMMAND_ID,
            format!("`{name}` changes state and needs a command_id"),
        ));
    }
    Ok(spec)
}

#[path = "methods_frame.rs"]
mod frame;
pub use frame::{FrameRefused, check_frame, role_name};

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;
