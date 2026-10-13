//! M3 orchestration shapes of the board API (PLAN §7.3, Phase 6): `assign
//! auto` (6-3), mail (6-2) and tasks with waitable completions (6-4). The
//! board's TypeScript types come from here.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::command::DeckCommandState;
use crate::event::ProviderKind;
use crate::ids::{CommandId, SessionId, StationId};

/// The frame in front of an assigned task, as in v0.
pub const ASSIGNMENT_PREFIX: &str = "[FLEETDECK ASSIGNMENT] ";
/// The longest task text of one assignment.
pub const MAX_TASK_CHARS: usize = 16_384;

/// `POST /api/assign`: give a task to the best session of the fleet
/// (`assign auto: <task>`). The board chooses `command_id`; a resend with
/// the same id returns the first answer and sends nothing new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "orchestration/")]
pub struct AssignRequest {
    pub command_id: CommandId,
    pub text: String,
    /// The repository path that the target must work in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<String>,
    /// The provider that the target must run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<ProviderKind>,
}

/// A station that can start a new session for an unrouted task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "orchestration/")]
pub struct SpawnHint {
    pub station_id: StationId,
    pub name: String,
}

/// The answer of `POST /api/assign`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export_to = "orchestration/")]
pub enum AssignOutcome {
    /// The task went to `session_id` as a prompt or a follow-up.
    Assigned {
        station_id: StationId,
        session_id: SessionId,
        callsign: Option<String>,
        command_id: CommandId,
        state: DeckCommandState,
    },
    /// No session fits. `spawn_on` is the best station for a new session,
    /// when one fits.
    Unrouted {
        reason: String,
        spawn_on: Option<SpawnHint>,
    },
}

/// The frame in front of a mail (6-2): `[FLEETDECK MAIL from <sender>] `.
#[must_use]
pub fn mail_frame(sender: &str, text: &str) -> String {
    format!("[FLEETDECK MAIL from {sender}] {text}")
}

/// The frame in front of a task (6-4): `[FLEETDECK TASK <task_id>] `.
#[must_use]
pub fn task_frame(task_id: &str, title: &str) -> String {
    format!(
        "[FLEETDECK TASK {task_id}] {title}\n(When the task is done, report it with \
         `complete_task {task_id}`.)"
    )
}

/// The frame in front of a worker's question to its coordinator (R2.3):
/// `[FLEETDECK QUESTION from <worker callsign>] `.
#[must_use]
pub fn question_frame(worker: &str, question: &str) -> String {
    format!("[FLEETDECK QUESTION from {worker}] {question}")
}

/// A group address of mail (R2.5), resolved on the deck at send time into
/// one mail per live session. The wire form is the address text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "orchestration/")]
pub enum MailGroup {
    /// Every live idle session.
    #[serde(rename = "@idle")]
    Idle,
    /// Every live Claude session.
    #[serde(rename = "@claude")]
    Claude,
    /// Every live Codex session.
    #[serde(rename = "@codex")]
    Codex,
}

impl MailGroup {
    /// The group of an address (`@idle`, `@claude`, `@codex`), or `None`.
    #[must_use]
    pub fn parse(address: &str) -> Option<Self> {
        match address {
            "@idle" => Some(Self::Idle),
            "@claude" => Some(Self::Claude),
            "@codex" => Some(Self::Codex),
            _ => None,
        }
    }

    /// The provider that a provider group names; `None` for `@idle`. The
    /// deck asks this instead of matching a provider name (crate layering).
    #[must_use]
    pub fn provider(self) -> Option<ProviderKind> {
        match self {
            Self::Idle => None,
            Self::Claude => Some(ProviderKind::Claude),
            Self::Codex => Some(ProviderKind::Codex),
        }
    }
}

/// How a mail reaches a session: a native follow-up (after the current
/// turn) or a native steer (into the current turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "orchestration/")]
pub enum MailDelivery {
    FollowUp,
    Steer,
}

/// `POST /api/mail` (6-2): one mail to one session. `mail_id` is chosen by
/// the sender (1 to 64 characters from `[0-9A-Za-z_-]`); a resend returns
/// the stored mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "orchestration/")]
pub struct MailRequest {
    pub mail_id: String,
    /// The station name or id.
    pub station: String,
    pub session_id: SessionId,
    pub text: String,
    /// `follow_up` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delivery: Option<MailDelivery>,
}

/// `POST /api/mail/group` (R2.5): one mail to every live session of a
/// group. The deck resolves the group at send time and stores one mail per
/// session, with the id `<mail_id>-<n>` (`n` from 1). `mail_id` has 1 to 56
/// characters from `[0-9A-Za-z_-]`; a resend returns the stored mails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "orchestration/")]
pub struct GroupMailRequest {
    pub mail_id: String,
    pub group: MailGroup,
    pub text: String,
    /// `follow_up` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delivery: Option<MailDelivery>,
}

/// The delivery state of deck-owned intent (the outbox item, A5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "orchestration/")]
pub enum DeliveryState {
    /// Waits for the station.
    Queued,
    /// The command is on its way.
    Sending,
    /// The station accepted it.
    Delivered,
    /// It will not be delivered; `reason` says why.
    Dead,
}

/// One mail as the board shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "orchestration/")]
pub struct MailView {
    pub mail_id: String,
    /// The audit actor or the coordinator session that sent it.
    pub sender: String,
    pub station_id: StationId,
    pub session_id: SessionId,
    pub delivery: MailDelivery,
    pub text: String,
    pub state: DeliveryState,
    pub reason: Option<String>,
    pub created_at: i64,
}

/// The state of a task. `done`, `failed` and `blocked` end it for good. A
/// task in `question` is still open: the coordinator answers with mail and
/// the worker can complete it again (R2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "orchestration/")]
pub enum TaskState {
    Open,
    Done,
    Failed,
    /// The worker cannot go on (or its session ended); `completion_reason`
    /// says why.
    Blocked,
    /// The worker asked its coordinator a question.
    Question,
}

impl TaskState {
    /// True while the task accepts a completion: `open` or `question`.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Question)
    }
}

/// `POST /api/tasks` (6-4): a task for one session, delivered through the
/// outbox as a follow-up. `task_id` is chosen by the creator; a resend
/// returns the stored task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "orchestration/")]
pub struct TaskRequest {
    pub task_id: String,
    pub title: String,
    /// The station name or id.
    pub station: String,
    pub session_id: SessionId,
}

/// `POST /api/tasks/{task_id}/complete`: the end of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "orchestration/")]
pub struct TaskCompletion {
    /// `done`, `failed`, `blocked` or `question`.
    pub state: TaskState,
    #[serde(default)]
    #[ts(optional)]
    pub result: Option<String>,
    /// Why the worker is blocked, or why it asks. Needed for `blocked` and
    /// `question`; refused for `done` and `failed`.
    #[serde(default)]
    #[ts(optional)]
    pub reason: Option<String>,
    /// The question for the coordinator. Needed for `question`; refused
    /// for every other state.
    #[serde(default)]
    #[ts(optional)]
    pub question: Option<String>,
}

impl TaskCompletion {
    /// The rules of a completion: `open` is no completion, `blocked` and
    /// `question` carry a reason, `question` carries a question, and no
    /// text is longer than [`MAX_RESULT_CHARS`].
    ///
    /// # Errors
    /// The message of the first rule that the completion breaks.
    pub fn check(&self) -> Result<(), String> {
        let filled = |t: &Option<String>| t.as_deref().is_some_and(|t| !t.trim().is_empty());
        let (needs_reason, needs_question) = match self.state {
            TaskState::Open => return Err("a completion is not `open`".to_owned()),
            TaskState::Done | TaskState::Failed => (false, false),
            TaskState::Blocked => (true, false),
            TaskState::Question => (true, true),
        };
        if needs_reason != self.reason.is_some() || needs_question != self.question.is_some() {
            return Err(
                "`blocked` and `question` carry a reason, `question` carries a question, \
                 and `done` and `failed` carry neither"
                    .to_owned(),
            );
        }
        if (needs_reason && !filled(&self.reason)) || (needs_question && !filled(&self.question)) {
            return Err("a reason and a question are not empty".to_owned());
        }
        for text in [&self.result, &self.reason, &self.question]
            .into_iter()
            .flatten()
        {
            if text.chars().count() > MAX_RESULT_CHARS {
                return Err(format!("a text has at most {MAX_RESULT_CHARS} characters"));
            }
        }
        Ok(())
    }
}

/// One task as the board shows it, and the answer of a wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "orchestration/")]
pub struct TaskView {
    pub task_id: String,
    pub title: String,
    pub creator: String,
    pub station_id: StationId,
    pub session_id: SessionId,
    pub state: TaskState,
    pub result: Option<String>,
    /// Why the task is `blocked`, or why the worker asks (`question`).
    pub completion_reason: Option<String>,
    /// The question of a `question` task.
    pub question: Option<String>,
    /// The delivery of the task text to the session.
    pub delivery: DeliveryState,
    /// Why the delivery is `dead`.
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

/// The longest wait of `GET /api/tasks/{task_id}/wait` (ADR-006 §6.6).
pub const MAX_WAIT_MS: u64 = 60_000;
/// The most tasks that one `GET /api/tasks/wait` names.
pub const MAX_WAIT_IDS: usize = 50;
/// The longest result text of a task.
pub const MAX_RESULT_CHARS: usize = 16_384;

/// The answer of a wait over several tasks (R2.4): the first task that is
/// not `open` any more, or `None` at the deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "orchestration/")]
pub struct TaskWaitAnswer {
    pub task: Option<TaskView>,
}

/// A coordinator grant as the board shows it (M3 6-5, ADR-006 §4). It
/// holds no credential and no digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "orchestration/")]
pub struct GrantView {
    pub grant_id: String,
    pub station_id: StationId,
    pub session_id: SessionId,
    /// `read`, `mail`, `drive`, `spawn`.
    pub classes: Vec<String>,
    pub stations: Vec<StationId>,
    pub spawn_cap: u32,
    pub spawn_used: u32,
    /// `active`, `revoked` or `expired`.
    pub state: String,
    pub revoked_reason: Option<String>,
    /// The binding generation; `None` until the station binds it.
    pub generation: Option<u32>,
    pub expires_at: i64,
    pub last_used_at: Option<i64>,
}
