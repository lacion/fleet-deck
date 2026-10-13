//! The link method table and the authorization of control frames
//! (ADR-004 §9.6, GATE-M1 G6 and G7).
//!
//! Every check uses the roles that were bound at pairing (the role of the
//! pinned peer key and the own role), never the dial direction. The table
//! must equal the table in ADR-004 §9.6; `tests/method_table.rs` compares
//! them.

use std::collections::BTreeSet;

use fleetdeck_proto::link::{ControlFrame, Scope, WireError};
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

/// Why a stream frame may not cross the link. The link closes with `reason`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}: {detail}")]
pub struct FrameRefused {
    pub reason: &'static str,
    pub detail: String,
}

/// Checks the fixed direction and the feature of a stream frame sent by a
/// node with role `sender` (ADR-004 §9.6): events and snapshots go station →
/// deck, acks go deck → station, snapshots need `snapshots.v1`. Request,
/// response, ping, pong and close go both ways.
pub fn check_frame(
    frame: &ControlFrame,
    sender: Role,
    negotiated: &Negotiated,
) -> Result<(), FrameRefused> {
    let (from, snapshot) = match frame {
        ControlFrame::Event { .. } => (Role::Station, false),
        ControlFrame::SnapshotBegin { .. }
        | ControlFrame::SnapshotSession { .. }
        | ControlFrame::SnapshotEnd { .. } => (Role::Station, true),
        ControlFrame::Ack { .. } => (Role::Deck, false),
        ControlFrame::Request { .. }
        | ControlFrame::Response { .. }
        | ControlFrame::Ping { .. }
        | ControlFrame::Pong { .. }
        | ControlFrame::Close { .. } => return Ok(()),
    };
    if sender != from {
        return Err(FrameRefused {
            reason: code::WRONG_ROLE,
            detail: format!(
                "a {} sent a `{}` frame, which only a {} sends",
                role_name(sender),
                frame.type_name(),
                role_name(from)
            ),
        });
    }
    if snapshot && !negotiated.has(feature::SNAPSHOTS) {
        return Err(FrameRefused {
            reason: code::PROTOCOL_ERROR,
            detail: format!(
                "a `{}` frame needs feature {}, which this link did not negotiate",
                frame.type_name(),
                feature::SNAPSHOTS
            ),
        });
    }
    Ok(())
}

/// `deck` or `station`.
#[must_use]
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Deck => "deck",
        Role::Station => "station",
    }
}

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;
