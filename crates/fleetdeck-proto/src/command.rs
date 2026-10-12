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
use crate::ids::{
    CommandId, IncarnationId, InputId, ProviderInstanceId, RequestId, SessionId, TurnId,
};
pub use crate::input::{ImageInput, UserInput};

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
}

/// How a start gets its workspace (PLAN §6.6, M3 6-1, DL-200). The station
/// does the Git work in the start effect, never in the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "command/")]
pub enum WorkspaceRequest {
    /// A new worktree `<repo>--fd-<callsign>` beside the repository at
    /// `spec.cwd`, on the new branch `fd/<callsign>` (v0 naming). Batch and
    /// `Nx` spawns always use it (forced isolation).
    Worktree,
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
        workspace: Option<WorkspaceRequest>,
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
            | Self::SetAutoContinue { session_id, .. } => Some(session_id),
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
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(retry_until: i64, target: Option<&str>) -> CommandEnvelope {
        CommandEnvelope {
            command_id: "01J0000000000000000000000A".into(),
            target_incarnation: target.map(IncarnationId::from),
            retry_until,
            origin: CommandOrigin::Board,
            command: SessionCommand::Interrupt {
                session_id: "s1".into(),
            },
        }
    }

    #[test]
    fn commands_serialize_with_an_op_tag_and_map_to_methods() {
        let cmd = SessionCommand::Answer {
            session_id: "s1".into(),
            request_id: "req-1".into(),
            decision: ApprovalDecision {
                option_id: "allow".into(),
                updated_input: None,
                text: None,
            },
        };
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v["op"], "answer");
        assert_eq!(cmd.method(), "approval.answer");
        assert_eq!(cmd.session_id().map(SessionId::as_str), Some("s1"));
    }

    #[test]
    fn admission_checks_the_incarnation_and_the_deadline() {
        let inc = IncarnationId::from("inc-1");
        let now = 1_000_000_000;
        assert!(envelope(now, Some("inc-1")).admit(now, &inc).is_ok());
        assert!(envelope(now, None).admit(now, &inc).is_ok());
        let err = envelope(now, Some("inc-0")).admit(now, &inc).unwrap_err();
        assert_eq!(err.code, RejectCode::IncarnationMismatch);
        assert!(!err.code.stores_receipt());
        // Inside the skew allowance, then expired.
        assert!(envelope(now - CLOCK_SKEW_MS, None).admit(now, &inc).is_ok());
        let err = envelope(now - CLOCK_SKEW_MS - 1, None)
            .admit(now, &inc)
            .unwrap_err();
        assert_eq!(err.code, RejectCode::Expired);
        assert!(!err.code.stores_receipt());
        let far = now + RETRY_WINDOW_MS + CLOCK_SKEW_MS + 1;
        let err = envelope(far, None).admit(now, &inc).unwrap_err();
        assert_eq!(err.code, RejectCode::InvalidCommand);
        assert!(err.code.stores_receipt());
    }

    // Receipts outlive the retry window (G3).
    const _: () = assert!(RECEIPT_RETENTION_MS > RETRY_WINDOW_MS + 2 * CLOCK_SKEW_MS);

    #[test]
    fn an_unknown_reject_code_degrades() {
        let r: Reject = serde_json::from_value(json!({
            "code": "quota_exceeded", "message": "m", "unsupported": null
        }))
        .unwrap();
        assert_eq!(r.code, RejectCode::Unknown);
    }

    #[test]
    fn equal_commands_have_equal_canonical_payloads() {
        let a = envelope(1, None);
        let mut b = envelope(2, Some("x"));
        assert_eq!(a.canonical_payload(), b.canonical_payload());
        b.command = SessionCommand::Fork {
            session_id: "s1".into(),
        };
        assert_ne!(a.canonical_payload(), b.canonical_payload());
    }

    #[test]
    fn an_import_carries_a_native_id_that_can_never_read_as_a_flag() {
        let cmd: SessionCommand = serde_json::from_value(json!({
            "op": "import",
            "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null, "model": null, "title": null},
            "native_session_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
            "prompt": null,
        }))
        .unwrap();
        assert_eq!(cmd.method(), "session.import");
        assert_eq!(cmd.session_id(), None);
        assert!(check_native_session_id("0b6c1d2e-aaaa-4bbb-8ccc-123456789abc").is_ok());
        assert!(check_native_session_id("thr_01.x").is_ok());
        for bad in [
            "",
            "-r",
            "--resume",
            "a b",
            "a/../b",
            "a;rm",
            &"a".repeat(129),
        ] {
            assert!(check_native_session_id(bad).is_err(), "{bad}");
        }
    }
}
