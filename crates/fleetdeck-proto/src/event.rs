//! Canonical, provider-blind events (PLAN §6.3, L6, L7, L16).
//!
//! Wire rules:
//! - The tag is `kind` with dotted names (`turn.completed`).
//! - A known kind is strict: a missing required field is an error.
//! - An unknown kind from a newer peer is kept as [`EventBody::Unknown`] and
//!   is passed through, so an old deck does not drop a newer station (L16).
//! - Extra fields are ignored; optional fields default to `null`.
//! - Times are Unix milliseconds (`i64`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::fleet_git::{CleanupState, PushState, SkippedFile};
use crate::ids::{InputId, NodeId, PlanId, ProviderInstanceId, RequestId, RunId, TurnId};
use crate::integrations::IssueRef;
use crate::session::{BatchRef, RepoInfo};

#[path = "event_body.rs"]
mod body;
pub use body::*;
#[path = "event_types.rs"]
mod types;
pub use types::*;

/// A canonical event of a known kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind")]
#[ts(export_to = "event/")]
pub enum Event {
    #[serde(rename = "session.started")]
    SessionStarted {
        provider: ProviderKind,
        provider_instance: ProviderInstanceId,
        provider_version: Option<String>,
        model: Option<String>,
        cwd: String,
        mode: Option<String>,
        /// Boxed: it is the largest field of the largest event (DL-061).
        capabilities: Box<Capabilities>,
        /// `Some(false)`: a resume was asked for but a new session started.
        resumed: Option<bool>,
        provider_refs: ProviderRefs,
        /// The title and the git branch that the station recorded when it
        /// created the session (M2 P4-4, DL-061). A provider core leaves
        /// them out; the station's store fills them in, so the deck's read
        /// model has them without a snapshot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        branch: Option<String>,
        /// The repository of `cwd` that the station recorded (M3 6-7,
        /// DL-306); the conflict radar groups file touches by it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Boxed to keep the event small (clippy `large_enum_variant`).
        repo: Option<Box<RepoInfo>>,
    },
    #[serde(rename = "session.state")]
    SessionStateChanged {
        state: SessionState,
        detail: Option<String>,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        run_id: RunId,
        input_summary: String,
        origin: RunOrigin,
        /// Who typed the input, when it is not the owner: `member:<name>`
        /// (R11.5, DL-166). Older peers send no field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        author: Option<String>,
        /// The input of the station that started the run (DL-179): the
        /// board matches it with the `input_id` of a review. Absent for a
        /// run that a provider started and in events of older peers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        input_id: Option<InputId>,
        /// The full text of the input that started the run, at most 32 KiB
        /// (cut at a character boundary, ending with `…`), when the station
        /// started it (B10). Absent for a provider-started run and in events
        /// of older peers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        input_text: Option<String>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        run_id: RunId,
        turn_id: TurnId,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "message.delta")]
    MessageDelta {
        turn_id: TurnId,
        /// The subagent tool call that produced the text, if any (L7).
        parent_node_id: Option<NodeId>,
        text: String,
    },
    #[serde(rename = "message.completed")]
    MessageCompleted {
        turn_id: TurnId,
        parent_node_id: Option<NodeId>,
        text: String,
    },
    #[serde(rename = "reasoning.delta")]
    ReasoningDelta { turn_id: TurnId, text: String },
    #[serde(rename = "tool.started")]
    ToolStarted {
        node_id: NodeId,
        parent_node_id: Option<NodeId>,
        /// PLAN §6.3 calls this field `kind`; `kind` is the event tag (DL-027).
        tool_kind: ToolKind,
        title: String,
        input_summary: String,
        #[serde(default)]
        files: Vec<String>,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "tool.output")]
    ToolOutput {
        node_id: NodeId,
        chunk: String,
        /// True if the station cut the output to its budget.
        #[serde(default)]
        truncated: bool,
    },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        node_id: NodeId,
        status: ToolStatus,
        #[serde(default)]
        files: Vec<String>,
        exit_code: Option<i32>,
        /// Provider ids of the result, for example `agent_id` of a subagent (DL-191).
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "file.changed")]
    FileChanged {
        path: String,
        change: FileChange,
        node_id: Option<NodeId>,
    },
    #[serde(rename = "approval.requested")]
    ApprovalRequested {
        request_id: RequestId,
        node_id: Option<NodeId>,
        /// PLAN §6.3 calls this field `kind` (DL-027).
        request_kind: RequestKind,
        title: String,
        details: String,
        /// The tool input that an allow can edit (only with `edit_input`).
        input: Option<Value>,
        options: Vec<ApprovalOption>,
        response: ResponseChannel,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "approval.resolved")]
    ApprovalResolved {
        request_id: RequestId,
        option_id: String,
        by: ResolvedBy,
        /// Who answered, when it is not the owner: `member:<name>` (R11.5,
        /// DL-166). Older peers send no field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        answered_by: Option<String>,
    },
    #[serde(rename = "approval.expired")]
    ApprovalExpired {
        request_id: RequestId,
        reason: ExpiredReason,
    },
    #[serde(rename = "plan.proposed")]
    PlanProposed {
        plan_id: PlanId,
        turn_id: Option<TurnId>,
        markdown: String,
    },
    #[serde(rename = "usage.updated")]
    UsageUpdated {
        usage: Usage,
        #[serde(default)]
        rate_limits: Vec<RateLimit>,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        turn_id: TurnId,
        status: TurnStatus,
        usage: Option<Usage>,
    },
    /// The checkpoint record of a completed turn (PLAN §6.3, §6.6, L13).
    /// It comes after `turn.completed`, at most once per app turn, and never
    /// changes the turn's duration or the session activity.
    #[serde(rename = "turn.finalized")]
    TurnFinalized {
        turn_id: TurnId,
        /// Always set by a station with the workspace service.
        checkpoint: Option<TurnCheckpoint>,
        /// Set when both checkpoint sides are available.
        diff_summary: Option<DiffSummary>,
    },
    /// A revert changed state (PLAN §6.6, DL-087). `turn_id` is the turn
    /// whose start is the target; `turns` is the count of whole turns that
    /// the provider rolls back. `reason` is a stable code for `failed` and
    /// `reconciliation_required`.
    #[serde(rename = "revert.updated")]
    RevertUpdated {
        revert_id: String,
        turn_id: TurnId,
        turns: u32,
        state: RevertState,
        reason: Option<String>,
    },
    /// The owner set or cleared the board title (DL-095). The station is
    /// the source of the title; the title is already normalized.
    #[serde(rename = "session.renamed")]
    SessionRenamed { title: Option<String> },
    /// The station named a new session (M3 6-1, DL-200): its callsign,
    /// unique on the station, and the worktree that its start creates.
    /// Older peers decode this kind as unknown.
    #[serde(rename = "session.named")]
    SessionNamed {
        callsign: String,
        worktree: Option<String>,
    },
    /// The session works on an issue (R3, DL-164): the station emits it
    /// once after a start that names an issue.
    #[serde(rename = "session.issue")]
    SessionIssue { issue: IssueRef },
    /// The owner turned auto-continue on or off (ADR-007 §2.1).
    #[serde(rename = "session.auto_continue_changed")]
    AutoContinueChanged { enabled: bool },
    /// A lost session was not continued at a station start; `reason` is
    /// the stable code of the first failed rule (ADR-007 §2.2).
    #[serde(rename = "session.auto_continue_skipped")]
    AutoContinueSkipped { reason: String },
    /// The station bound a coordinator credential to the session's native
    /// identity, or renewed it (ADR-006 §5.5, §6.1). No secret.
    #[serde(rename = "coordinator.bound")]
    CoordinatorBound {
        binding_id: String,
        grant_id: String,
        generation: u32,
        provider_instance: String,
        native_id: String,
    },
    /// A coordinator binding ended: `idle` (expired, renewable) or
    /// another reason (revoked) (ADR-006 §5.5).
    #[serde(rename = "coordinator.ended")]
    CoordinatorEnded {
        binding_id: String,
        generation: u32,
        reason: String,
    },
    /// Provider stdout bytes `[from, to)` were not parsed as events
    /// (ADR-005 §4.2; DL-113). Older peers decode this kind as unknown.
    #[serde(rename = "session.output_gap")]
    OutputGap {
        from: u64,
        to: u64,
        reason: OutputGapReason,
    },
    /// A provider TUI holds the session (`shell_id` set), or the hold ended
    /// (`shell_id` absent) (DL-140). `pid` is the TUI process once it runs.
    #[serde(rename = "session.terminal")]
    SessionTerminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        shell_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<String>,
    },
    /// The slash commands that the provider offers in this session (DL-140),
    /// and the names of its skills (DL-174); a skill name is also a command.
    #[serde(rename = "session.slash_commands")]
    SlashCommands {
        commands: Vec<String>,
        #[serde(default)]
        skills: Vec<String>,
    },
    /// A notice for the session chat. `code` names the cause; `text` is for
    /// the user. The fold ignores it.
    #[serde(rename = "session.notice")]
    SessionNotice { code: String, text: String },
    /// The models that the provider offers in this session (DL-151). The
    /// fold ignores it.
    #[serde(rename = "session.models")]
    SessionModels { models: Vec<ModelChoice> },
    /// The commit that the session started from (R1.1). The station
    /// emits it, not the provider. The fold keeps it in `summary.base`.
    #[serde(rename = "session.base")]
    SessionBase {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        git_ref: Option<String>,
        oid: String,
        /// The station guessed the base (an import or a resume).
        #[serde(default)]
        inferred: bool,
    },
    /// The station stored a version of an artifact for the session (R12,
    /// DL-165). The fold counts the artifacts of the session.
    #[serde(rename = "artifact.published")]
    ArtifactPublished {
        artifact_id: String,
        version: u32,
        title: String,
        /// Named `artifact_kind` because `kind` is the event tag.
        artifact_kind: crate::artifacts::ArtifactKind,
    },
    /// The session wrote an HTML file that may be meant to be opened; the
    /// board offers "Keep as artifact". Once per path and session.
    #[serde(rename = "artifact.suggested")]
    ArtifactSuggested { path: String },
    /// The session's one-line current focus, set by its `set_focus` MCP
    /// tool (R6, DL-165). Empty text clears it.
    #[serde(rename = "session.focus")]
    SessionFocus { text: String },
    /// The `Nx` batch that the session started in (R1.4). The station
    /// emits it after the start, like `session.base`. The fold keeps it in
    /// `summary.batch`.
    #[serde(rename = "session.batch")]
    SessionBatch { batch: BatchRef },
    /// The station stops this idle session after `after_min` minutes
    /// without activity (R6.6, DL-169). It comes before the stop; the board
    /// shows the session as hibernated and resumes it on the next input.
    #[serde(rename = "session.hibernated")]
    SessionHibernated { after_min: u32 },
    /// The owner marked the previous run of the session (shim `shim_id`) as
    /// ended after the station checked its processes (DL-182). Older peers
    /// decode this kind as unknown.
    #[serde(rename = "session.execution_reconciled")]
    ExecutionReconciled { shim_id: String },
    /// The station's model help named the session (DL-167, R10.1). It
    /// follows the internal `session.renamed` that set `title`; the fold
    /// marks the title as generated while the title is still the same.
    /// Older peers decode this kind as unknown.
    #[serde(rename = "session.title_generated")]
    SessionTitleGenerated { title: String },
    /// The station's model help summarized the session (DL-167, R10.2).
    /// The text is generated; a newer digest replaces the older one.
    /// Older peers decode this kind as unknown.
    #[serde(rename = "session.digest")]
    SessionDigest { text: String },
    /// The fetch before a worktree start failed; the session started from
    /// the local `HEAD` (FG-1, O8). The fold sets `summary.base_stale`.
    #[serde(rename = "workspace.base_stale")]
    WorkspaceBaseStale { code: String, text: String },
    /// Files of `worktree_include` that the station did not copy (FG-4).
    /// The fold ignores it.
    #[serde(rename = "workspace.include_skipped")]
    WorkspaceIncludeSkipped { files: Vec<SkippedFile> },
    /// The push state of the session branch changed (FG-2). The fold keeps
    /// it in `summary.push`.
    #[serde(rename = "session.push")]
    SessionPush {
        state: PushState,
        ref_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        oid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        desired_oid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        stop_code: Option<String>,
        attempts: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        next_attempt_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        text: Option<String>,
    },
    /// The cleanup state of the session worktree changed (FG-3). The fold
    /// keeps it in `summary.cleanup`.
    #[serde(rename = "session.cleanup")]
    SessionCleanup {
        state: CleanupState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<String>,
    },
    /// The artifact folder of the session is ready (FG-12). The fold keeps
    /// it in `summary.artifacts_dir`.
    #[serde(rename = "artifacts.ready")]
    ArtifactsReady {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
    },
    /// The session started without the artifact variables (FG-12). The fold
    /// ignores it.
    #[serde(rename = "artifacts.unavailable")]
    ArtifactsUnavailable { code: String, text: String },
    /// The artifact share is not verified now (FG-12, B6). The fold clears
    /// `summary.artifacts_dir`.
    #[serde(rename = "artifacts.revoked")]
    ArtifactsRevoked { code: String, text: String },
    #[serde(rename = "run.completed")]
    RunCompleted {
        run_id: RunId,
        status: TurnStatus,
        /// Background work still runs; the session is not idle (L7).
        background_pending: bool,
    },
    #[serde(rename = "mode.changed")]
    ModeChanged { mode: String },
    #[serde(rename = "model.changed")]
    ModelChanged { model: String },
    /// The provider answered an interrupt request (ADR-001 §3.1): this is
    /// the evidence that confirms or refuses the station's interrupt. The
    /// turn ends later with its own `turn.completed` (trap 10).
    #[serde(rename = "interrupt.result")]
    InterruptResult {
        /// The host's id for the request (the station's effect id); `None`
        /// when the host gave no id.
        control_id: Option<String>,
        accepted: bool,
        message: Option<String>,
    },
    #[serde(rename = "input.updated")]
    InputUpdated {
        input_id: InputId,
        turn_id: Option<TurnId>,
        state: InputState,
    },
    #[serde(rename = "background.updated")]
    BackgroundUpdated { tasks: Vec<BackgroundTask> },
    /// A task or sub-agent changed state (B6). The fold keeps no state for
    /// it. Older peers decode this kind as unknown.
    #[serde(rename = "task.updated")]
    TaskUpdated {
        task_id: String,
        /// The tool node that the task belongs to, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        node_id: Option<String>,
        /// The provider's kind name: `local_agent`, `local_bash`, `codex_agent`.
        task_kind: String,
        /// `running`, `completed`, `failed`, `stopped`, or the provider's text.
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        summary: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        /// Stable code, for example `usage_limit` or `auth_required`.
        code: String,
        message: String,
        /// Unix ms when a retry can succeed (for example a usage limit reset).
        retry_at: Option<i64>,
    },
    #[serde(rename = "session.ended")]
    SessionEnded {
        reason: String,
        exit_code: Option<i32>,
    },
}

#[path = "event_kinds.rs"]
mod kinds;

// `BackgroundTask`, `RunOrigin` and `input_text_of` moved to their own file
// for the module size rule (B6, B10).
#[path = "event_tasks.rs"]
mod tasks;
pub use tasks::{BackgroundTask, RunOrigin, input_text_of};

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
