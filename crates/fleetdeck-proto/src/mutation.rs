//! The envelopes of every mutating link method (ADR-004 §9.3, Revision 2).
//!
//! A mutating request carries `command_id` in the frame and an envelope in
//! `params`. Every envelope has the same three admission fields
//! (`command_id`, `target_incarnation`, `retry_until`) and one payload
//! field. [`ENVELOPES`] is the code table that the ADR table must equal
//! (`tests/adr_revision2.rs`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::command::{CommandEnvelope, Reject, RejectCode, SessionSpec, UserInput, admit};
use crate::coordinator_desktop_wire::CoordinatorDesktopCallEnvelope;
use crate::event::ProviderKind;
use crate::ids::{CommandId, IncarnationId, ProviderInstanceId, RequestId, SessionId, StationId};
use crate::orchestration::{MailGroup, TaskState};
use crate::shell::ShellEnvelope;

/// Domain separation of a key rotation signature (ADR-004 §9.5).
pub const ROTATION_CONTEXT: &str = "fleetdeck-rotate-key-v1";

/// A node whose database decides a command and keeps its receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    Station,
    Deck,
    /// The node that receives the request (either role).
    Receiver,
}

impl Holder {
    /// The word of the ADR table.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Station => "station",
            Self::Deck => "deck",
            Self::Receiver => "receiver",
        }
    }
}

/// One row of the envelope table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeKind {
    pub methods: &'static [&'static str],
    /// The Rust (and TypeScript) type of `params`.
    pub params: &'static str,
    /// The field that holds the payload; the receipt hash covers it.
    pub payload: &'static str,
    /// Whose incarnation `target_incarnation` names; the same node decides
    /// the command and keeps the receipt.
    pub holder: Holder,
}

/// The methods that carry a [`crate::command::SessionCommand`].
pub const SESSION_METHODS: &[&str] = &[
    "session.start",
    "session.resume",
    "session.import",
    "session.fork",
    "session.prompt",
    "session.steer",
    "session.follow_up",
    "session.interrupt",
    "session.set_mode",
    "session.set_model",
    "session.set_effort",
    "session.stop",
    "session.revert",
    "session.rename",
    "session.resolve_revert",
    "session.set_auto_continue",
    "session.coordinator_renew",
    "session.coordinator_revoke",
    "session.coordinator_set",
    "session.worktree_trash",
    "session.worktree_restore",
    "session.worktree_purge",
    "session.worktree_release",
    "session.reconcile_execution",
    "session.stop_execution",
    "session.shell_command",
    "session.release_shell",
    "session.land",
    "session.move_out",
    "session.move_in",
    "session.move_finish",
    "session.move_undo",
    "session.usage_recount",
    "approval.answer",
];

/// The envelope of each mutating method (ADR-004 §9.3).
pub const ENVELOPES: &[EnvelopeKind] = &[
    EnvelopeKind {
        methods: SESSION_METHODS,
        params: "CommandEnvelope",
        payload: "command",
        holder: Holder::Station,
    },
    EnvelopeKind {
        methods: &["station.configure"],
        params: "StationConfigureEnvelope",
        payload: "change",
        holder: Holder::Station,
    },
    EnvelopeKind {
        methods: &["link.rotate_key"],
        params: "KeyRotationEnvelope",
        payload: "rotation",
        holder: Holder::Receiver,
    },
    EnvelopeKind {
        methods: &[
            "shell.open",
            "shell.resize",
            "shell.close",
            "shell.handover",
            "shell.release",
        ],
        params: "ShellEnvelope",
        payload: "change",
        holder: Holder::Station,
    },
    EnvelopeKind {
        methods: &["coordinator.call"],
        params: "CoordinatorCallEnvelope",
        payload: "call",
        holder: Holder::Deck,
    },
    EnvelopeKind {
        methods: &["coordinator.desktop_call"],
        params: "CoordinatorDesktopCallEnvelope",
        payload: "call",
        holder: Holder::Deck,
    },
];

/// The envelope row of a mutating method.
#[must_use]
pub fn envelope_of(method: &str) -> Option<&'static EnvelopeKind> {
    ENVELOPES.iter().find(|e| e.methods.contains(&method))
}

/// A station setting that the deck may change (`station.configure`).
///
/// Allowed roots, provider instances, shells and setup commands stay
/// station-local: only the station owner widens what a station may touch
/// (DL-041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub enum StationChange {
    /// At most the value of station.toml; a higher value is rejected.
    SetMaxSessions { max_sessions: u32 },
    /// `None` = wait for the deck; `Some(ms)` = deny after that time.
    SetOfflineApprovalPolicy { deny_after_ms: Option<u64> },
}

/// `station.configure` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "mutation/")]
pub struct StationConfigureEnvelope {
    pub command_id: CommandId,
    pub target_incarnation: IncarnationId,
    pub retry_until: i64,
    pub change: StationChange,
}

/// A new identity key, signed by the old one (ADR-004 §9.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "mutation/")]
pub struct KeyRotation {
    /// The fingerprint of the old key (64 hex digits), the pinned key.
    pub old_key: String,
    /// The new Ed25519 public key (64 hex digits).
    pub new_key: String,
    /// Ed25519 signature by the old key over [`KeyRotation::signed_message`]
    /// (128 hex digits).
    pub signature: String,
}

impl KeyRotation {
    /// The signed bytes: context, old key, new key, the receiver's
    /// fingerprint and the command id, each ended by a zero byte. The
    /// receiver and the id bind the signature to one peer and one command.
    #[must_use]
    pub fn signed_message(&self, receiver: &str, command_id: &CommandId) -> Vec<u8> {
        let mut out = Vec::new();
        for part in [
            ROTATION_CONTEXT,
            &self.old_key,
            &self.new_key,
            receiver,
            command_id.as_str(),
        ] {
            out.extend_from_slice(part.as_bytes());
            out.push(0);
        }
        out
    }
}

/// `link.rotate_key` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "mutation/")]
pub struct KeyRotationEnvelope {
    pub command_id: CommandId,
    pub target_incarnation: IncarnationId,
    pub retry_until: i64,
    pub rotation: KeyRotation,
}

/// A session that a coordinator operation names (ADR-006 §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct CoordinatorTarget {
    pub station: StationId,
    pub session_id: SessionId,
}

/// What `assign_auto` needs from a session (the board's `assign auto`
/// rules, DL-204).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct AssignRequirements {
    /// A repository path: the session works in it or in one of its
    /// worktrees; a start uses it as `cwd`.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub provider: Option<ProviderKind>,
}

/// The delivery of coordinator mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "mutation/")]
pub enum CoordinatorDelivery {
    FollowUp,
    Steer,
}

/// An operation of a coordinator agent that changes fleet intent (G7,
/// ADR-006 §6.1). Read operations use `coordinator.query`. An unknown op is
/// rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub enum CoordinatorOp {
    /// Start a session on a station (counts against the grant's spawn cap).
    Spawn {
        station: StationId,
        spec: SessionSpec,
        prompt: Option<UserInput>,
    },
    /// Mail to a session.
    Send {
        to: CoordinatorTarget,
        text: String,
        delivery: CoordinatorDelivery,
    },
    /// Interrupt the turn of a session of this grant's lineage.
    Interrupt {
        station: StationId,
        session_id: SessionId,
    },
    /// Stop a session of this grant's lineage.
    Stop {
        station: StationId,
        session_id: SessionId,
    },
    /// Assign a task to a session.
    Assign { task: String, to: CoordinatorTarget },
    /// Assign a task to the best session, or start one (spawn class).
    AssignAuto {
        task: String,
        requirements: AssignRequirements,
    },
    /// Complete a task that was assigned to the caller. `state` is `done`
    /// when absent; `reason` and `question` follow the rules of
    /// `TaskCompletion::check` (R2.1).
    CompleteTask {
        task_id: String,
        result: Option<String>,
        #[serde(default)]
        state: Option<TaskState>,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        question: Option<String>,
    },
    /// The caller asks its coordinator a question (R2.3): a `question`
    /// completion of the caller's open task and a mail to the coordinator
    /// session that created the task.
    AskCoordinator { question: String },
    /// Mail to every live session of a group (R2.5), on the grant's
    /// stations, without the caller.
    SendGroup {
        group: MailGroup,
        text: String,
        delivery: CoordinatorDelivery,
    },
    /// Resume a session (`manage` class, ADR-008 §4.1, §5.2).
    Resume {
        station: StationId,
        session_id: SessionId,
    },
    /// Rename a session (`manage` class).
    Rename {
        station: StationId,
        session_id: SessionId,
        title: String,
    },
    /// Set the mode of a session (`manage` class, ADR-008 §5.2).
    SetMode {
        station: StationId,
        session_id: SessionId,
        mode: String,
    },
    /// Move a session to a project, or out of it (`manage` class).
    ProjectAssign {
        station: StationId,
        session_id: SessionId,
        target: ProjectTarget,
    },
    /// Start a session in a project (`spawn` and `manage` classes; counts
    /// against the spawn cap).
    ProjectStart {
        project_id: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        provider_instance: Option<ProviderInstanceId>,
        worktree: bool,
    },
    /// Answer an approval or a question of a session (`answer` class,
    /// ADR-008 §5).
    Answer {
        station: StationId,
        session_id: SessionId,
        request_id: RequestId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        question_answers: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        text: Option<String>,
    },
    /// Land a session (`manage` class, Fleet Git FG-10, design §13.2). Needs
    /// `fleet_git.land.v1`.
    Land {
        station: StationId,
        session_id: SessionId,
    },
    /// Move a session to another station (`manage` class, Fleet Git FG-9,
    /// design §13.2). Needs `fleet_git.move.v1` on both stations.
    Move {
        station: StationId,
        session_id: SessionId,
        to_station: StationId,
    },
}

impl CoordinatorOp {
    /// Every station that the op names, as mutable references: the `station`
    /// of a variant, the `station` of each [`CoordinatorTarget`] it holds,
    /// and the `to_station` of a `Move`. A variant without a station returns
    /// an empty list. The deck resolves a station name to its id with it.
    pub fn stations_mut(&mut self) -> Vec<&mut StationId> {
        match self {
            Self::Spawn { station, .. }
            | Self::Interrupt { station, .. }
            | Self::Stop { station, .. }
            | Self::Resume { station, .. }
            | Self::Rename { station, .. }
            | Self::SetMode { station, .. }
            | Self::ProjectAssign { station, .. }
            | Self::Answer { station, .. }
            | Self::Land { station, .. } => vec![station],
            Self::Send { to, .. } | Self::Assign { to, .. } => vec![&mut to.station],
            Self::Move {
                station,
                to_station,
                ..
            } => vec![station, to_station],
            Self::AssignAuto { .. }
            | Self::CompleteTask { .. }
            | Self::AskCoordinator { .. }
            | Self::SendGroup { .. }
            | Self::ProjectStart { .. } => Vec::new(),
        }
    }
}

/// Where `project_assign` moves a session (ADR-008 §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub enum ProjectTarget {
    /// The project with this id.
    Project { id: String },
    /// Out of every project.
    None,
    /// The project that the board's `assign auto` rules pick.
    Auto,
}

/// The list that `project_items` reads (ADR-008 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "mutation/")]
pub enum ProjectItemKind {
    Issues,
    Prs,
}

/// The authority that a fleet-control command carries from the deck to the
/// target station (ADR-008 §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct CommandAuthority {
    pub grant_id: String,
    pub caller_station: StationId,
    pub caller_session: SessionId,
    pub generation: u32,
}

/// `coordinator.authority_check` params (ADR-008 §6.2): the target station
/// asks the deck whether the authority of a command still holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct AuthorityCheck {
    pub grant_id: String,
    pub generation: u32,
    pub caller_station: StationId,
    pub caller_session: SessionId,
    pub target_session: SessionId,
    pub command_id: CommandId,
    /// The provider launch or mode change that the target station is about
    /// to make, with the mode it will really use: the deck checks it
    /// against the grant's mode ceiling and the bypass rule (ADR-008 §5.2).
    /// Absent for a write that changes no mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub launch: Option<LaunchMode>,
}

/// The provider family and the effective mode of a launch or a mode change
/// (`mode: None`: the provider default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct LaunchMode {
    pub provider: crate::event::ProviderKind,
    pub mode: Option<String>,
}

/// The answer to [`AuthorityCheck`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct AuthorityAnswer {
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

/// `coordinator.policy_now` params (ADR-008 §6.2): the deck asks the caller
/// station for its live fleet-control policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct PolicyNow {
    pub caller_session: SessionId,
    /// The binding that the deck expects (R13 P6 §5.6); `desktop_mcp.v1` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub binding: Option<CoordinatorBinding>,
}

/// The answer to [`PolicyNow`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct PolicyNowAnswer {
    /// The live station setting `allow_fleet_control`.
    pub fleet_control: bool,
    /// The caller session still holds its binding.
    pub bound: bool,
    /// The expected binding equals the current one (R13 P6 §5.6); only with
    /// a `binding` in the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub binding_matches: Option<bool>,
}

/// The binding facts of a coordinator session: the deck checks them
/// against its own projection (ADR-006 §6.1, §6.2 step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct CoordinatorBinding {
    pub binding_id: String,
    pub grant_id: String,
    pub generation: u32,
    pub provider_instance: ProviderInstanceId,
    pub native_id: String,
}

/// The calling agent and its operation. The credential and its digest
/// never leave the station (ADR-006 §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct CoordinatorCall {
    /// The session of the coordinator agent on the sending station.
    pub caller: SessionId,
    pub binding: CoordinatorBinding,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub op: CoordinatorOp,
}

/// `coordinator.call` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "mutation/")]
pub struct CoordinatorCallEnvelope {
    pub command_id: CommandId,
    /// The deck's database incarnation.
    pub target_incarnation: IncarnationId,
    pub retry_until: i64,
    pub call: CoordinatorCall,
}

/// A read of a coordinator agent (`coordinator.query`, ADR-006 §6.6). It
/// keeps no receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "query", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub enum CoordinatorQuery {
    /// The sessions on the grant's stations (`read` class).
    ListSessions,
    /// Wait for the end of a task on the grant's stations (`read` class).
    WaitTask { task_id: String },
    /// Wait for the first of several tasks that is not `open` any more
    /// (`read` class, R2.4).
    WaitTasks { task_ids: Vec<String> },
    /// The receipt of an own `coordinator.call` (any mutating class).
    CommandStatus { command_id: CommandId },
    /// The last items of a session (`inspect` class, ADR-008 §4.1).
    SessionTail {
        session_id: SessionId,
        max_items: u32,
    },
    /// The diff of a session against its base (`inspect` class, ADR-008
    /// §4.4).
    SessionDiff { session_id: SessionId },
    /// The open requests of the sessions that the grant may answer for.
    NeedsYou,
    /// The projects on the grant's stations.
    Projects,
    /// The issues or PRs of a project.
    ProjectItems {
        project_id: String,
        kind: ProjectItemKind,
    },
    /// The artifacts of one session, or of every session on the grant's
    /// stations.
    Artifacts {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        session_id: Option<SessionId>,
    },
    /// One artifact, at its latest version or at `version`.
    Artifact {
        artifact_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        version: Option<u32>,
    },
    /// A file in the session root.
    FileRead { session_id: SessionId, path: String },
    /// A search in the session root.
    FileSearch {
        session_id: SessionId,
        pattern: String,
        mode: String,
    },
    /// The branches of other stations in a project (`inspect` class, Fleet
    /// Git FG-10, design §13.2). Needs `fleet_git.branches.v1`.
    ProjectBranches { project_id: String },
    /// One fetch on the radar home of a project (`inspect` class, FG-10).
    ProjectFetch { project_id: String },
}

/// The `params` of `coordinator.query`. `deadline` ≤ arrival + 30 s, or
/// ≤ arrival + 60 s for `wait_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "mutation/")]
pub struct CoordinatorQueryEnvelope {
    pub query_id: String,
    /// The deck's database incarnation.
    pub target_incarnation: IncarnationId,
    pub caller: SessionId,
    pub binding: CoordinatorBinding,
    pub deadline: i64,
    pub query: CoordinatorQuery,
}

/// The parsed `params` of a mutating request.
#[derive(Debug, Clone, PartialEq)]
pub enum Mutation {
    Session(CommandEnvelope),
    Configure(StationConfigureEnvelope),
    RotateKey(KeyRotationEnvelope),
    Coordinator(CoordinatorCallEnvelope),
    DesktopCall(CoordinatorDesktopCallEnvelope),
    Shell(ShellEnvelope),
}

fn invalid(message: impl Into<String>) -> Reject {
    Reject::new(RejectCode::InvalidCommand, message)
}

fn decode<T: serde::de::DeserializeOwned>(method: &str, params: Value) -> Result<T, Reject> {
    serde_json::from_value(params).map_err(|e| invalid(format!("bad `{method}` params: {e}")))
}

impl Mutation {
    /// Reads the envelope of `method`. The frame's `command_id` and the
    /// envelope's must agree, and a session command must belong to the
    /// method (ADR-004 §9.3); else `invalid_command`.
    pub fn parse(
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Self, Reject> {
        let kind = envelope_of(method)
            .ok_or_else(|| invalid(format!("`{method}` is not a mutating method")))?;
        let parsed = match kind.params {
            "CommandEnvelope" => Self::Session(decode(method, params)?),
            "StationConfigureEnvelope" => Self::Configure(decode(method, params)?),
            "KeyRotationEnvelope" => Self::RotateKey(decode(method, params)?),
            "ShellEnvelope" => Self::Shell(decode(method, params)?),
            "CoordinatorDesktopCallEnvelope" => Self::DesktopCall(decode(method, params)?),
            _ => Self::Coordinator(decode(method, params)?),
        };
        if parsed.method() != method {
            return Err(invalid(format!(
                "the envelope carries `{}`, the request is `{method}`",
                parsed.method()
            )));
        }
        if command_id != Some(parsed.command_id()) {
            return Err(invalid(
                "the frame's command_id and the envelope's do not agree",
            ));
        }
        Ok(parsed)
    }

    /// The link method of the envelope.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Session(e) => e.command.method(),
            Self::Configure(_) => "station.configure",
            Self::RotateKey(_) => "link.rotate_key",
            Self::Coordinator(_) => "coordinator.call",
            Self::DesktopCall(_) => "coordinator.desktop_call",
            Self::Shell(e) => e.change.method(),
        }
    }

    #[must_use]
    pub fn command_id(&self) -> &CommandId {
        match self {
            Self::Session(e) => &e.command_id,
            Self::Configure(e) => &e.command_id,
            Self::RotateKey(e) => &e.command_id,
            Self::Coordinator(e) => &e.command_id,
            Self::DesktopCall(e) => &e.command_id,
            Self::Shell(e) => &e.command_id,
        }
    }

    /// The incarnation and deadline checks; no receipt is stored for their
    /// rejections (G3).
    pub fn admit(&self, now: i64, incarnation: &IncarnationId) -> Result<(), Reject> {
        let (target, retry_until) = match self {
            Self::Session(e) => (e.target_incarnation.as_ref(), e.retry_until),
            Self::Configure(e) => (Some(&e.target_incarnation), e.retry_until),
            Self::RotateKey(e) => (Some(&e.target_incarnation), e.retry_until),
            Self::Coordinator(e) => (Some(&e.target_incarnation), e.retry_until),
            Self::DesktopCall(e) => (Some(&e.target_incarnation), e.retry_until),
            Self::Shell(e) => (Some(&e.target_incarnation), e.retry_until),
        };
        admit(target, retry_until, now, incarnation)
    }

    /// The stable payload text that the receipt hash covers.
    #[must_use]
    pub fn canonical_payload(&self) -> String {
        let payload = match self {
            Self::Session(e) => return e.canonical_payload(),
            Self::Configure(e) => serde_json::to_string(&e.change),
            Self::RotateKey(e) => serde_json::to_string(&e.rotation),
            Self::Coordinator(e) => serde_json::to_string(&e.call),
            Self::DesktopCall(e) => serde_json::to_string(&e.call),
            Self::Shell(e) => serde_json::to_string(&e.change),
        };
        payload.unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "mutation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "fleet_control_tests.rs"]
mod fleet_control_tests;
