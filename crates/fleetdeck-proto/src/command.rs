//! Commands, command envelopes and receipts (PLAN §6.4, §8.4, GATE-M1 G3).
//!
//! A command is a mutating request. It carries a ULID `command_id`, the
//! station incarnation that it targets, and a `retry_until` deadline. The
//! station decides it in one transaction and stores a receipt; a duplicate
//! `command_id` returns the stored receipt and does nothing else.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Unsupported;
use crate::fleet_git::FleetGitStart;
use crate::ids::{
    CommandId, IncarnationId, InputId, ProviderInstanceId, RequestId, SessionId, TurnId,
};
pub use crate::input::{ImageInput, UserInput};
use crate::integrations::IssueRef;
use crate::mutation::CommandAuthority;
use crate::native::TranscriptPin;
use crate::session::BatchRef;

/// The deck retries a command for at most this long after it accepted it (G3).
pub const RETRY_WINDOW_MS: i64 = 15 * 60 * 1000;
/// Clock skew that the station tolerates on `retry_until` (G3).
pub const CLOCK_SKEW_MS: i64 = 2 * 60 * 1000;
/// The station keeps receipts at least this long; it is longer than any retry window (G3).
pub const RECEIPT_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

/// What a new session runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
pub struct SessionSpec {
    pub provider_instance: ProviderInstanceId,
    pub cwd: String,
    /// A native mode; a mode that the provider does not offer runs as the most
    /// restrictive mode (L8).
    pub mode: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
    /// The coordinator grant of the new session (M3 6-5, ADR-006 §4); only
    /// an owner start through the deck may carry one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator: Option<Box<CoordinatorSpec>>,
    /// Station-local: set by the station right before a launch, cleared on
    /// every incoming command (B9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub launch: Option<Box<crate::launch::LaunchIdentity>>,
}

/// The coordinator grant that a session starts with (ADR-006 §4). It holds
/// no credential: the station mints the credential in the start effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
#[serde(deny_unknown_fields)]
pub struct CoordinatorSpec {
    pub grant_id: String,
    /// Grant classes (`observe`, `message`, `spawn`, ...).
    pub classes: Vec<String>,
    #[ts(type = "number")]
    pub idle_ttl_ms: u64,
    #[ts(type = "number")]
    pub expires_at: i64,
    pub control_skill: ControlSkill,
    /// The reach of the grant (ADR-008 §3). Skipped when `lineage`, so a
    /// lineage grant serializes as under `coordinator.v1`.
    #[serde(default, skip_serializing_if = "GrantScope::is_lineage")]
    #[ts(optional, as = "Option<GrantScope>")]
    pub scope: GrantScope,
    /// The answer budget of a fleet-control grant (ADR-008 §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer_cap: Option<u32>,
}

/// The reach of a coordinator grant (ADR-008 §2, §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum GrantScope {
    /// The ADR-006 drive rule: the target has this grant in its lineage.
    #[default]
    Lineage,
    /// Any session on a station of the grant, minus the ADR-008 §4.2
    /// exclusions.
    Fleet,
}

impl GrantScope {
    /// True for the default scope; the serde skip rule of `scope`.
    #[must_use]
    pub fn is_lineage(&self) -> bool {
        matches!(self, Self::Lineage)
    }
}

/// The versioned control skill that the coordinator MCP endpoint serves as
/// its `instructions` (PLAN §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
#[serde(deny_unknown_fields)]
pub struct ControlSkill {
    pub version: String,
    pub text: String,
}

/// How a start gets its workspace (PLAN §6.6, M3 6-1, DL-200). The station
/// does the Git work in the start effect, never in the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "command/")]
pub enum WorkspaceRequest {
    /// A new worktree `<repo>--fd-<callsign>` beside the repository at
    /// `spec.cwd`, by default on the new branch `fd/<callsign>` (v0 naming).
    /// Batch and `Nx` spawns always use it (forced isolation).
    Worktree {
        /// The branch of the worktree: an existing branch is checked out, a
        /// new one is created. Absent: the new branch `fd/<callsign>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        branch: Option<String>,
        /// The commit-ish that a new `branch` starts from (default `HEAD`).
        /// It needs a `branch`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        base: Option<String>,
    },
    /// Clone `url` into the new directory `spec.cwd` with the station's own
    /// Git credentials. A URL with credentials is refused.
    Clone { url: String },
}

/// The answer to a runtime request, with the provider's own option id (L3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
pub struct ApprovalDecision {
    pub option_id: String,
    /// An edited tool input (only with the `edit_input` capability).
    pub updated_input: Option<Value>,
    /// Free text: the answer to a question, or a reason for a deny.
    pub text: Option<String>,
    /// Who answered, when it is not the owner: `member:<name>` (R11.5,
    /// DL-166). The deck sets it for every answer of a member and clears it
    /// for every other caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answered_by: Option<String>,
}

/// How to stop a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum StopMode {
    /// Ask the provider to end, then stop the process after a grace period.
    Graceful,
    /// Stop the process now.
    Kill,
}

/// One mutating operation on a station. The tag is `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum SessionCommand {
    Start {
        spec: SessionSpec,
        prompt: Option<UserInput>,
        /// Where the session's workspace comes from (M3 6-1, DL-200):
        /// absent = `spec.cwd` as it is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        workspace: Option<Box<WorkspaceRequest>>,
        /// A shell command that the station runs in the workspace before the
        /// provider starts (DL-148). Only the owner sets it, and only a
        /// station with `allow_setup_cmd = true` runs it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        setup_cmd: Option<String>,
        /// The `Nx` batch of this start (R1.4, DL-170). The station emits
        /// `session.batch` after the start; the board compares the siblings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        batch: Option<Box<BatchRef>>,
        /// The issue that the session works on (R3, DL-164). The station
        /// emits `session.issue` after the start.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        issue: Option<Box<IssueRef>>,
        /// The first prompt holds third-party text (an issue, a pull
        /// request): the station runs the session in the most restrictive
        /// mode of its provider and refuses any other mode (R3.1).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        origin_text: bool,
        /// The Fleet Git snapshot of the project (FG-1, design §3.3). Only a
        /// deck that negotiated `fleet_git.v1` sends it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        fleet_git: Option<Box<FleetGitStart>>,
    },
    Resume {
        session_id: SessionId,
    },
    /// A new driven session that continues an existing native CLI session
    /// (Claude `session_id`, Codex `thread_id`) in `spec.cwd` (M3 6-12,
    /// DL-308). It needs the `import.v1` feature.
    Import {
        spec: SessionSpec,
        native_session_id: String,
        prompt: Option<UserInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        transcript: Option<TranscriptPin>, // B7: native-sessions.md §2.4
    },
    Prompt {
        session_id: SessionId,
        input: UserInput,
    },
    Steer {
        session_id: SessionId,
        input: UserInput,
    },
    FollowUp {
        session_id: SessionId,
        input: UserInput,
    },
    Interrupt {
        session_id: SessionId,
    },
    Answer {
        session_id: SessionId,
        request_id: RequestId,
        decision: ApprovalDecision,
    },
    SetMode {
        session_id: SessionId,
        mode: String,
    },
    SetModel {
        session_id: SessionId,
        model: String,
    },
    Fork {
        session_id: SessionId,
    },
    Stop {
        session_id: SessionId,
        how: StopMode,
    },
    /// Roll back the native conversation, then restore the workspace, to the
    /// start of `turn_id` (PLAN §6.6). The turn and every later turn go away.
    Revert {
        session_id: SessionId,
        turn_id: TurnId,
    },
    /// Set the board title of a session; `None` clears it (PLAN §4.1 row
    /// "rename", DL-095). It changes no provider state, so it works on an
    /// ended session too. [`normalize_title`] gives the stored text.
    Rename {
        session_id: SessionId,
        title: Option<String>,
    },
    /// The owner's resolution of a revert in `reconciliation_required`
    /// (PLAN §6.6, DL-097).
    ResolveRevert {
        session_id: SessionId,
        revert_id: String,
        resolution: RevertResolution,
    },
    /// Turn auto-continue of a session on or off (L19, ADR-007 §2.1). Off
    /// by default; only an owner board command can turn it on.
    SetAutoContinue {
        session_id: SessionId,
        enabled: bool,
    },
    /// Renew the idle-expired coordinator binding of `session_id` under
    /// `grant_id` at `generation` (ADR-006 §7). Deck only, owner action.
    CoordinatorRenew {
        session_id: SessionId,
        grant_id: String,
        generation: u32,
    },
    /// Revoke the coordinator binding of `session_id` under `grant_id`
    /// (ADR-006 §7). Deck only, owner action.
    CoordinatorRevoke {
        session_id: SessionId,
        grant_id: String,
    },
    /// Move the worktree of an ended session into the trash folder beside
    /// it (DL-150). Nothing is deleted. Owner action, scope `admin`.
    WorktreeTrash {
        session_id: SessionId,
    },
    /// Move a trashed worktree back to its place (DL-150).
    WorktreeRestore {
        session_id: SessionId,
    },
    /// Delete a trashed worktree for good (DL-150). `confirm_name` is the
    /// name of the trash folder.
    WorktreePurge {
        session_id: SessionId,
        confirm_name: String,
    },
    /// Stop tracking a worktree operation that failed (DL-150). Nothing is
    /// moved or deleted.
    WorktreeRelease {
        session_id: SessionId,
    },
    /// The owner marks the previous run of a session under a shim as ended
    /// (DL-182, design `shim-resume.md` §4.2). The station checks the
    /// processes again and refuses while any of them lives. Owner action,
    /// scope `admin`.
    ReconcileExecution {
        session_id: SessionId,
        shim_id: String,
    },
    /// Run `command` in the session folder, as `!` does in the terminal UIs
    /// (B8). Owner board command; needs `shell_command.v1`.
    ShellCommand {
        session_id: SessionId,
        command: String,
    },
    /// The owner states that the shell command `effect_id` no longer runs
    /// (B8). Needs `shell_command.v1`.
    ReleaseShell {
        session_id: SessionId,
        effect_id: String,
    },
}

/// How the owner resolves a revert that needs reconciliation (DL-097).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum RevertResolution {
    /// The owner checked the conversation and the files and accepts their
    /// state. The revert ends; the session takes input again. No turn from
    /// before the reconciliation can be reverted after it, because the
    /// station no longer knows the conversation's turn count.
    Reconciled,
    /// The conversation rollback is confirmed; restore the files again. The
    /// files must still equal the completion or the target checkpoint.
    RetryRestore,
}

/// The longest session title, in characters (DL-095).
pub const MAX_TITLE_CHARS: usize = 80;

/// The stored form of a session title: trimmed, `None` when empty. A title
/// longer than [`MAX_TITLE_CHARS`] or with a control character is refused.
///
/// # Errors
///
/// The reason text when the title is refused.
pub fn normalize_title(title: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err("the title is longer than 80 characters");
    }
    if title.chars().any(char::is_control) {
        return Err("the title has a control character");
    }
    Ok(Some(title.to_owned()))
}

/// The longest native session id that `session.import` accepts.
pub const MAX_NATIVE_SESSION_ID: usize = 128;

/// Checks a native session id for `session.import`: 1 to
/// [`MAX_NATIVE_SESSION_ID`] ASCII letters, digits, `-`, `_` or `.`, and an
/// alphanumeric first character, so that it can never read as a CLI flag.
///
/// # Errors
/// The reason for a refused id.
pub fn check_native_session_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() || id.len() > MAX_NATIVE_SESSION_ID {
        return Err("the session id must have 1 to 128 characters");
    }
    if !id.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("the session id must start with a letter or a digit");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("the session id may hold only letters, digits, `-`, `_` and `.`");
    }
    Ok(())
}

impl SessionCommand {
    /// The link method that carries this command (ADR-004 §9.6 method table).
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Start { .. } => "session.start",
            Self::Resume { .. } => "session.resume",
            Self::Import { .. } => "session.import",
            Self::Prompt { .. } => "session.prompt",
            Self::Steer { .. } => "session.steer",
            Self::FollowUp { .. } => "session.follow_up",
            Self::Interrupt { .. } => "session.interrupt",
            Self::Answer { .. } => "approval.answer",
            Self::SetMode { .. } => "session.set_mode",
            Self::SetModel { .. } => "session.set_model",
            Self::Fork { .. } => "session.fork",
            Self::Stop { .. } => "session.stop",
            Self::Revert { .. } => "session.revert",
            Self::Rename { .. } => "session.rename",
            Self::ResolveRevert { .. } => "session.resolve_revert",
            Self::SetAutoContinue { .. } => "session.set_auto_continue",
            Self::CoordinatorRenew { .. } => "session.coordinator_renew",
            Self::CoordinatorRevoke { .. } => "session.coordinator_revoke",
            Self::WorktreeTrash { .. } => "session.worktree_trash",
            Self::WorktreeRestore { .. } => "session.worktree_restore",
            Self::WorktreePurge { .. } => "session.worktree_purge",
            Self::WorktreeRelease { .. } => "session.worktree_release",
            Self::ReconcileExecution { .. } => "session.reconcile_execution",
            Self::ShellCommand { .. } => "session.shell_command",
            Self::ReleaseShell { .. } => "session.release_shell",
        }
    }

    /// Clears the station-local fields that an incoming command must never
    /// carry (B9 §3.4): `spec.launch` of every command that holds a spec.
    pub fn clear_station_local(&mut self) {
        match self {
            Self::Start { spec, .. } | Self::Import { spec, .. } => spec.launch = None,
            _ => {}
        }
    }

    /// The session that the command targets, if it exists already.
    #[must_use]
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Start { .. } | Self::Import { .. } => None,
            Self::Resume { session_id }
            | Self::Prompt { session_id, .. }
            | Self::Steer { session_id, .. }
            | Self::FollowUp { session_id, .. }
            | Self::Interrupt { session_id }
            | Self::Answer { session_id, .. }
            | Self::SetMode { session_id, .. }
            | Self::SetModel { session_id, .. }
            | Self::Fork { session_id }
            | Self::Stop { session_id, .. }
            | Self::Revert { session_id, .. }
            | Self::Rename { session_id, .. }
            | Self::ResolveRevert { session_id, .. }
            | Self::SetAutoContinue { session_id, .. }
            | Self::CoordinatorRenew { session_id, .. }
            | Self::CoordinatorRevoke { session_id, .. }
            | Self::WorktreeTrash { session_id }
            | Self::WorktreeRestore { session_id }
            | Self::WorktreePurge { session_id, .. }
            | Self::WorktreeRelease { session_id }
            | Self::ReconcileExecution { session_id, .. }
            | Self::ShellCommand { session_id, .. }
            | Self::ReleaseShell { session_id, .. } => Some(session_id),
        }
    }
}

/// Who sent a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum CommandOrigin {
    Board,
    Coordinator,
    Mail,
    Local,
}

/// A command with its idempotency and freshness data (G3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    /// The station incarnation the deck saw; `None` only for the local CLI.
    pub target_incarnation: Option<IncarnationId>,
    /// Unix ms after which the station refuses the command without a receipt.
    pub retry_until: i64,
    pub origin: CommandOrigin,
    /// The authority of a fleet-control command (ADR-008 §6.1), sent only
    /// on `coordinator.v2` links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authority: Option<CommandAuthority>,
    pub command: SessionCommand,
}

impl CommandEnvelope {
    /// The admission checks that run before a receipt lookup (G3).
    ///
    /// `incarnation_mismatch` and `expired` do not store a receipt: a resend
    /// with the same `command_id` gets the same answer.
    pub fn admit(&self, now: i64, incarnation: &IncarnationId) -> Result<(), Reject> {
        admit(
            self.target_incarnation.as_ref(),
            self.retry_until,
            now,
            incarnation,
        )
    }

    /// A stable text of the command payload. A reused `command_id` with a
    /// different payload is rejected (PLAN §6.4).
    #[must_use]
    pub fn canonical_payload(&self) -> String {
        // Struct fields serialize in declaration order and JSON maps are
        // sorted, so the text is stable for equal commands.
        serde_json::to_string(&self.command).unwrap_or_default()
    }
}

/// The admission checks of every mutating envelope (G3, ADR-004 §9.3):
/// the receiver's database incarnation and the `retry_until` deadline.
/// `target` is `None` only on the station-local socket.
pub fn admit(
    target: Option<&IncarnationId>,
    retry_until: i64,
    now: i64,
    incarnation: &IncarnationId,
) -> Result<(), Reject> {
    if let Some(target) = target
        && target != incarnation
    {
        return Err(Reject::new(
            RejectCode::IncarnationMismatch,
            format!("the command targets incarnation {target}, the receiver is {incarnation}"),
        ));
    }
    if now > retry_until.saturating_add(CLOCK_SKEW_MS) {
        return Err(Reject::new(RejectCode::Expired, "retry_until has passed"));
    }
    let latest = now
        .saturating_add(RETRY_WINDOW_MS)
        .saturating_add(CLOCK_SKEW_MS);
    if retry_until > latest {
        return Err(Reject::new(
            RejectCode::InvalidCommand,
            "retry_until is further away than the retry window",
        ));
    }
    Ok(())
}

/// Accepted or rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum ReceiptState {
    Accepted,
    Rejected,
}

/// Why a station refused a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum RejectCode {
    UnknownSession,
    UnknownProvider,
    UnknownRequest,
    /// The request was already answered or expired.
    RequestClosed,
    /// The session is in a state that does not allow the command.
    InvalidState,
    /// The provider does not support the operation (see `unsupported`).
    Unsupported,
    InvalidCommand,
    /// The `command_id` was used before with a different payload or caller.
    CommandIdReused,
    Forbidden,
    /// No receipt is stored (G3).
    IncarnationMismatch,
    /// No receipt is stored (G3).
    Expired,
    /// The coordinator binding facts do not match the deck's grant
    /// (ADR-006 §6.2 step 3); no receipt is stored.
    BindingStale,
    /// The grant's lifetime spawn cap is used (ADR-006 §6.4); stored.
    SpawnCapReached,
    /// The grant's lifetime answer cap is used (ADR-008 §3); stored.
    AnswerCapReached,
    /// The request has an answer already (ADR-008 §5.3); stored.
    AlreadyAnswered,
    /// A code of a newer peer.
    #[serde(other)]
    Unknown,
}

impl RejectCode {
    /// False for the codes that the station answers without a stored receipt.
    #[must_use]
    pub fn stores_receipt(self) -> bool {
        !matches!(
            self,
            Self::IncarnationMismatch | Self::Expired | Self::BindingStale
        )
    }
}

/// A typed rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{code:?}: {message}")]
#[ts(export_to = "command/")]
pub struct Reject {
    pub code: RejectCode,
    pub message: String,
    pub unsupported: Option<Unsupported>,
}

impl Reject {
    /// A rejection without a capability result.
    #[must_use]
    pub fn new(code: RejectCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            unsupported: None,
        }
    }
}

impl From<Unsupported> for Reject {
    fn from(unsupported: Unsupported) -> Self {
        Self {
            code: RejectCode::Unsupported,
            message: unsupported.to_string(),
            unsupported: Some(unsupported),
        }
    }
}

/// What an accepted command created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
pub struct CommandResult {
    pub session_id: Option<SessionId>,
    pub input_id: Option<InputId>,
}

/// The stored answer to a command (PLAN §6.4). Acceptance, not completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
pub struct Receipt {
    pub command_id: CommandId,
    pub state: ReceiptState,
    /// The `station_seq` of the decision transaction (accepted only).
    pub station_seq: Option<u64>,
    pub result: Option<CommandResult>,
    pub reject: Option<Reject>,
    /// Unix ms.
    pub at: i64,
}

impl Receipt {
    /// An accepted receipt.
    #[must_use]
    pub fn accepted(
        command_id: CommandId,
        station_seq: u64,
        result: CommandResult,
        at: i64,
    ) -> Self {
        Self {
            command_id,
            state: ReceiptState::Accepted,
            station_seq: Some(station_seq),
            result: Some(result),
            reject: None,
            at,
        }
    }

    /// A rejected receipt.
    #[must_use]
    pub fn rejected(command_id: CommandId, reject: Reject, at: i64) -> Self {
        Self {
            command_id,
            state: ReceiptState::Rejected,
            station_seq: None,
            result: None,
            reject: Some(reject),
            at,
        }
    }
}

/// The deck-side state of a command in its outbox (G3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum DeckCommandState {
    /// Not yet answered by the station.
    Pending,
    /// The station stored a receipt (accepted or rejected).
    Done,
    /// The command cannot run, for example `station_reset`.
    Failed,
    /// The retry window passed without a receipt; the outcome is unknown.
    Ambiguous,
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
