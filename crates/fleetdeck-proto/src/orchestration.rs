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

/// The state of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "orchestration/")]
pub enum TaskState {
    Open,
    Done,
    Failed,
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
    /// `done` or `failed`.
    pub state: TaskState,
    #[serde(default)]
    #[ts(optional)]
    pub result: Option<String>,
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
    /// The delivery of the task text to the session.
    pub delivery: DeliveryState,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

/// The longest wait of `GET /api/tasks/{task_id}/wait` (ADR-006 §6.6).
pub const MAX_WAIT_MS: u64 = 60_000;
/// The longest result text of a task.
pub const MAX_RESULT_CHARS: usize = 16_384;
