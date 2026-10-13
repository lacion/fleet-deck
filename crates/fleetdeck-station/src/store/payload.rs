//! Effect payloads: what an outbox effect does (moved out of `effects.rs`
//! in M3 round 10 to keep modules below 800 lines; the stored JSON shape is
//! unchanged).

use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin};
use fleetdeck_proto::{InputId, RequestId};
use serde::{Deserialize, Serialize};

/// A host input as the effect carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub input_id: InputId,
    pub origin: RunOrigin,
    pub input: UserInput,
}

/// The Git work of a start effect (M3 6-1, DL-200): decided and checked
/// in the command transaction, done by the effect worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspacePlan {
    /// `git worktree add -b <branch> <path> [<base>]` in `repo`; an existing
    /// `branch` is checked out instead (DL-144).
    Worktree {
        repo: String,
        path: String,
        branch: String,
        /// The commit-ish that a new `branch` starts from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// `git clone <url> <path>`.
    Clone { url: String, path: String },
}

/// What a worktree effect does with its `worktree_ops` row (DL-150).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeAction {
    Trash,
    Restore,
    Purge,
}

/// What an effect does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectPayload {
    Start {
        spec: SessionSpec,
        prompt: Option<InputRecord>,
        /// Git work before the provider starts (M3 6-1, DL-200).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<WorkspacePlan>,
        /// The owner's setup command, run after the Git work and before the
        /// provider starts (DL-148).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        setup_cmd: Option<String>,
    },
    /// A native resume of an ended session (DL-118): a new provider
    /// process continues `native_session_id` with `spec` (the stored
    /// provider instance, workspace and mode).
    Resume {
        native_session_id: String,
        spec: SessionSpec,
        /// The first input of the resumed process: the fixed continuation
        /// of an auto-continue (ADR-007 §2.3); absent for an owner resume.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<InputRecord>,
        /// Unix ms after which an auto-continue is not dispatched: the
        /// window of ADR-007 §2.2 rule 9, rechecked at dispatch (§2.4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continue_by: Option<i64>,
    },
    Prompt {
        input: InputRecord,
    },
    Steer {
        input: InputRecord,
    },
    FollowUp {
        input: InputRecord,
    },
    Interrupt,
    Answer {
        request_id: RequestId,
        decision: ApprovalDecision,
        by: ResolvedBy,
    },
    SetMode {
        mode: String,
    },
    SetModel {
        model: String,
    },
    Stop {
        how: StopMode,
    },
    /// Roll back the conversation by `turns` whole turns, then restore the
    /// workspace to the start checkpoint of `app_turn` (PLAN §6.6, DL-087).
    /// The `reverts` row holds the steps.
    Revert {
        revert_id: String,
        app_turn: String,
        turns: u32,
    },
    /// The file work of one worktree trash operation (DL-150). It needs no
    /// provider process: the `worktree_ops` row holds the steps.
    Worktree {
        op_id: String,
        action: WorktreeAction,
    },
}

impl EffectPayload {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Resume { .. } => "resume",
            Self::Prompt { .. } => "prompt",
            Self::Steer { .. } => "steer",
            Self::FollowUp { .. } => "follow_up",
            Self::Interrupt => "interrupt",
            Self::Answer { .. } => "answer",
            Self::SetMode { .. } => "set_mode",
            Self::SetModel { .. } => "set_model",
            Self::Stop { .. } => "stop",
            Self::Revert { .. } => "revert",
            Self::Worktree { .. } => "worktree",
        }
    }

    /// True for effects that act on a running process (all but start, resume
    /// and worktree).
    #[must_use]
    pub fn needs_process(&self) -> bool {
        !matches!(
            self,
            Self::Start { .. } | Self::Resume { .. } | Self::Worktree { .. }
        )
    }

    /// The host input that the effect delivers.
    #[must_use]
    pub fn input(&self) -> Option<&InputRecord> {
        match self {
            Self::Prompt { input } | Self::Steer { input } | Self::FollowUp { input } => {
                Some(input)
            }
            Self::Start { prompt, .. } => prompt.as_ref(),
            _ => None,
        }
    }

    /// The key that the confirming evidence carries.
    pub(super) fn correlation(&self) -> Option<String> {
        match self {
            Self::Prompt { input } | Self::Steer { input } | Self::FollowUp { input } => {
                Some(input.input_id.0.clone())
            }
            Self::Answer { request_id, .. } => Some(request_id.0.clone()),
            Self::SetMode { mode } => Some(mode.clone()),
            Self::SetModel { model } => Some(model.clone()),
            _ => None,
        }
    }
}
