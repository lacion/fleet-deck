//! The session projection: the read-model shape of one session (PLAN §6.4,
//! §11). The station keeps it in its projections, sends it in snapshots
//! (G4), and the board renders it. `fleetdeck-core` folds events into it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::event::{
    ApprovalOption, ModelChoice, ProviderKind, RateLimit, RequestKind, ResponseChannel,
    RevertState, SessionState, ToolKind, Usage, UsageRecount,
};
use crate::fleet_git::{ArtifactsDir, CleanupInfo, LandInfo, MoveInInfo, MovedTo, PushInfo};
use crate::ids::{NodeId, ProviderInstanceId, RequestId, RunId, SessionId, TurnId};
use crate::integrations::IssueRef;

/// What a session does now, for the board's lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "session/")]
pub enum Activity {
    /// No run, no background work.
    Idle,
    /// A run is active.
    Working,
    /// A runtime request waits for an answer.
    NeedsYou,
    /// The run ended but background work still runs (L7).
    Background,
    /// The process ended or is lost.
    Ended,
}

/// An open runtime request (approval or question) with native option ids (L3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct OpenRequest {
    pub request_id: RequestId,
    pub node_id: Option<NodeId>,
    pub request_kind: RequestKind,
    pub title: String,
    pub details: String,
    pub input: Option<Value>,
    pub options: Vec<ApprovalOption>,
    pub response: ResponseChannel,
    /// Unix ms.
    pub requested_at: i64,
}

/// The projection of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub provider: Option<ProviderKind>,
    pub provider_instance: Option<ProviderInstanceId>,
    pub provider_version: Option<String>,
    pub cwd: Option<String>,
    /// The git branch of the workspace when the session started (M2 P4-2);
    /// `None` outside a repository or with a detached HEAD. Absent in a
    /// summary stored before P4-2.
    #[serde(default)]
    pub branch: Option<String>,
    /// The station's callsign (M3 6-1, DL-200): unique on its station; the
    /// mail and task address of the session. Absent before M3.
    #[serde(default)]
    pub callsign: Option<String>,
    /// The worktree that the station created for the session (M3 6-1).
    #[serde(default)]
    pub worktree: Option<String>,
    /// The repository of `cwd` (M3 6-7, DL-306). Absent before M3.
    #[serde(default)]
    pub repo: Option<RepoInfo>,
    pub title: Option<String>,
    /// The title came from the station's model help and the owner did not
    /// change it since (DL-167, R10.1). Absent before DL-167.
    #[serde(default)]
    pub title_generated: bool,
    /// The generated digest of the session (DL-167, R10.2): a few lines of
    /// "what the agent did". Absent until one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub digest: Option<String>,
    pub state: SessionState,
    pub activity: Activity,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub capabilities: Option<Capabilities>,
    pub current_run: Option<RunId>,
    pub current_turn: Option<TurnId>,
    pub open_requests: Vec<OpenRequest>,
    /// Ids of running background tasks (L7).
    pub background_tasks: Vec<String>,
    /// How many of `background_tasks` are subagents
    /// ([`SUBAGENT_TASK_KINDS`]): a subagent that runs on after its tool
    /// call completed (D15). Absent in a summary stored before M2 P4-3.
    #[serde(default)]
    pub background_agents: u32,
    /// Tools that started and did not complete yet, oldest first, at most
    /// [`MAX_RUNNING_TOOLS`]. The deck derives `verifying` from them (§7.2).
    /// Absent in a summary stored before M2 P3-6.
    #[serde(default)]
    pub running_tools: Vec<RunningTool>,
    /// Sum of the reported usage; `None` parts are unknown.
    pub usage: Usage,
    /// The old count of a session (DL-449): the total that the session had
    /// when the first usage of `USAGE_SCHEME` came, plus every later usage
    /// without a scheme. It is never added to `usage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage_legacy: Option<LegacyUsage>,
    /// The last recount of `usage` from the provider record (B20, design §4.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage_recount: Option<UsageRecount>,
    /// The latest provider rate-limit windows, one per scope, sorted by
    /// scope (M3 6-9, DL-303). Empty: no window reported (unknown, not
    /// zero). Absent in a summary stored before M3.
    #[serde(default)]
    pub rate_limits: Vec<RateLimit>,
    /// The last error, for example `usage_limit` with its retry time.
    pub last_error: Option<SessionError>,
    /// The latest revert of the session (PLAN §6.6, DL-087). While it is
    /// `admitted`, `conversation_rolled_back` or `reconciliation_required`,
    /// the station refuses new input. Absent in a summary stored before M3.
    #[serde(default)]
    pub revert: Option<SessionRevert>,
    /// Auto-continue after a station restart (L19, ADR-007 §2.1). Off by
    /// default, also for a summary stored before it existed.
    #[serde(default)]
    pub auto_continue: bool,
    /// The TUI that holds the session (DL-140). Absent in older summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub terminal: Option<TerminalHold>,
    /// The models that the provider harness offers, from the last
    /// `session.models` (DL-151). Absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub models: Option<Vec<ModelChoice>>,
    /// The slash commands of the provider, from the last
    /// `session.slash_commands` (DL-174). Absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slash_commands: Option<Vec<String>>,
    /// The skill names of the provider, from the last
    /// `session.slash_commands` (DL-174). Absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub skills: Option<Vec<String>>,
    /// The commit that the session started from, from `session.base`
    /// (R1.1). Absent before the first one and outside a repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<SessionBase>,
    /// How many artifacts the session published (R12, DL-165). Absent in
    /// an older summary.
    #[serde(default)]
    pub artifacts_count: u32,
    /// The one-line focus that the session set with `set_focus` (R6,
    /// DL-165). Absent before the first one, and after an empty one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub focus: Option<String>,
    /// The `Nx` batch that started the session, from `session.batch`
    /// (R1.4, DL-170). Absent for a session that is not in a batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub batch: Option<BatchRef>,
    /// The station stopped the session because it was idle (R6.6, DL-169).
    /// It stays set until the session starts again.
    #[serde(default)]
    pub hibernated: bool,
    /// The issue that the session works on, from `session.issue` (R3,
    /// DL-164). Absent when the session was not started from an issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub issue: Option<IssueRef>,
    /// The fetch before the start failed and the session started from the
    /// local `HEAD`, from `workspace.base_stale` (FG-1).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub base_stale: bool,
    /// The push state of the session branch, from `session.push` (FG-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub push: Option<PushInfo>,
    /// The cleanup state of the worktree, from `session.cleanup` (FG-3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup: Option<CleanupInfo>,
    /// The newest step of a land of the session, from `session.land` (FG-8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub land: Option<LandInfo>,
    /// Where the session moved to, from `session.moved` (FG-9). The session
    /// stays `exited`; the station refuses its input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub moved_to: Option<MovedTo>,
    /// The move state of a session that arrived by a move, from
    /// `session.move_in` (FG-9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub move_in: Option<MoveInInfo>,
    /// The artifact folder of the session, from `artifacts.ready` and
    /// `artifacts.revoked` (FG-12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub artifacts_dir: Option<ArtifactsDir>,
    /// The highest `session_seq` folded into this summary.
    pub session_seq: u64,
    /// Unix ms of the last folded event.
    pub updated_at: i64,
}

/// An old count (DL-449).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct LegacyUsage {
    pub usage: Usage,
    /// Unix ms of the first event of `USAGE_SCHEME`: the old count is the
    /// usage before it.
    pub until: i64,
}

impl SessionSummary {
    /// An empty summary for a session that has no events yet.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            provider: None,
            provider_instance: None,
            provider_version: None,
            cwd: None,
            branch: None,
            callsign: None,
            worktree: None,
            repo: None,
            title: None,
            title_generated: false,
            digest: None,
            state: SessionState::Starting,
            activity: Activity::Idle,
            mode: None,
            model: None,
            capabilities: None,
            current_run: None,
            current_turn: None,
            open_requests: Vec::new(),
            background_tasks: Vec::new(),
            background_agents: 0,
            running_tools: Vec::new(),
            usage: Usage::default(),
            usage_legacy: None,
            usage_recount: None,
            rate_limits: Vec::new(),
            last_error: None,
            revert: None,
            auto_continue: false,
            terminal: None,
            models: None,
            slash_commands: None,
            skills: None,
            base: None,
            artifacts_count: 0,
            focus: None,
            batch: None,
            hibernated: false,
            issue: None,
            base_stale: false,
            push: None,
            cleanup: None,
            land: None,
            moved_to: None,
            move_in: None,
            artifacts_dir: None,
            session_seq: 0,
            updated_at: 0,
        }
    }
}

/// A provider TUI that holds the session (DL-140). While it exists the
/// station refuses resume, auto-continue, import and a second handover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct TerminalHold {
    pub shell_id: String,
    /// The TUI process, once it runs.
    pub pid: Option<u32>,
    /// Unix ms of the first `session.terminal` of this hold.
    #[ts(type = "number")]
    pub since: i64,
    /// The reason of the newest `session.terminal` of this hold.
    pub reason: Option<String>,
}

/// The commit that a session started from (R1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionBase {
    /// The branch or ref name that the base came from, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub git_ref: Option<String>,
    /// The commit id.
    pub oid: String,
    /// The station guessed the base (an import or a resume).
    #[serde(default)]
    pub inferred: bool,
}

/// The batch that a session started in (R1.4, DL-170): one `batch_id` for each
/// press of "Start N sessions", the position of the session in it, and the
/// size of the batch. The board makes the id (a ULID); the station keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct BatchRef {
    pub batch_id: String,
    /// The place in the batch, from 1.
    pub index: u32,
    /// How many sessions the batch asked for.
    pub size: u32,
}

/// The most sessions that one batch holds (a refusal above it).
pub const MAX_BATCH_SIZE: u32 = 100;

impl BatchRef {
    /// Why the reference is not valid, or `None`: a `batch_id` of 1 to 64
    /// characters (`[0-9A-Za-z_-]`), `index` from 1 to `size`, and `size`
    /// from 1 to [`MAX_BATCH_SIZE`].
    #[must_use]
    pub fn problem(&self) -> Option<&'static str> {
        let id_ok = !self.batch_id.is_empty()
            && self.batch_id.len() <= 64
            && self
                .batch_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !id_ok {
            return Some("the batch id must have 1 to 64 characters of [0-9A-Za-z_-]");
        }
        if self.size == 0 || self.size > MAX_BATCH_SIZE {
            return Some("the batch size must be from 1 to 100");
        }
        if self.index == 0 || self.index > self.size {
            return Some("the batch index must be from 1 to the batch size");
        }
        None
    }
}

/// The most running tools that a summary keeps; the oldest goes first.
pub const MAX_RUNNING_TOOLS: usize = 32;

/// The repository that holds a session's `cwd` on its station (M3 6-7,
/// DL-306).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct RepoInfo {
    /// The absolute work tree root on the station.
    pub root: String,
    /// The normalized origin (`host/path`) when the repository has exactly
    /// one remote. A heuristic, not an identity (PLAN §6.6).
    pub origin: Option<String>,
}

/// Background task kinds that are subagents (D15): Claude reports a
/// backgrounded `Agent`/`Task` call as `local_agent`.
pub const SUBAGENT_TASK_KINDS: &[&str] = &["local_agent", "remote_agent"];

/// A tool call that runs now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct RunningTool {
    pub node_id: NodeId,
    pub tool_kind: ToolKind,
    pub title: String,
    pub input_summary: String,
}

/// The status column of a card (PLAN §7.2). The deck derives it from the
/// session projection and the state of the station link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "session/")]
pub enum CardStatus {
    /// The session is starting; no run yet.
    Queued,
    Working,
    /// A running shell tool matches the test-runner pattern.
    Verifying,
    /// An open approval or question.
    NeedsYou,
    Idle,
    Ended,
    /// The station link is down; the card shows the last known state.
    Unreachable,
    /// The station restarted and the process is gone; the card offers resume.
    Lost,
}

/// The last error of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionError {
    pub code: String,
    pub message: String,
    pub retry_at: Option<i64>,
}

/// The latest revert of a session, as the board shows it (DL-087).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionRevert {
    pub revert_id: String,
    pub turn_id: TurnId,
    pub state: RevertState,
    pub reason: Option<String>,
}

impl SessionRevert {
    /// True while the revert blocks new session input.
    #[must_use]
    pub fn blocks_input(&self) -> bool {
        matches!(
            self.state,
            RevertState::Admitted
                | RevertState::ConversationRolledBack
                | RevertState::ReconciliationRequired
        )
    }
}
