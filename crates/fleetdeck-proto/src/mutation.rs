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
use crate::event::ProviderKind;
use crate::ids::{CommandId, IncarnationId, ProviderInstanceId, SessionId, StationId};
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
    "session.stop",
    "session.revert",
    "session.rename",
    "session.resolve_revert",
    "session.set_auto_continue",
    "session.coordinator_renew",
    "session.coordinator_revoke",
    "session.worktree_trash",
    "session.worktree_restore",
    "session.worktree_purge",
    "session.worktree_release",
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
            Self::Shell(e) => serde_json::to_string(&e.change),
        };
        payload.unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "mutation_tests.rs"]
mod tests;
