//! Effect payloads: what an outbox effect does (moved out of `effects.rs`
//! in M3 round 10 to keep modules below 800 lines; the stored JSON shape is
//! unchanged).

use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ProviderKind, ResolvedBy, RunOrigin};
use fleetdeck_proto::fleet_git::{MoveKind, MovePublication};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{InputId, ProviderInstanceId, RequestId, SessionId};
use fleetdeck_providers::ShellContext;
use serde::{Deserialize, Serialize};

/// `skip_serializing_if` for a flag that is false by default.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// The run limit of a `!` command that a stored row of an older build lacks.
fn default_shell_timeout_ms() -> u64 {
    600_000
}

/// A host input as the effect carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub input_id: InputId,
    pub origin: RunOrigin,
    pub input: UserInput,
    /// The `!` shell results that this input carries (B8): resolved at the
    /// decision, replayed with the input.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shell_context: Vec<ShellContext>,
}

/// The fetch of a synced worktree start (FG-1, design §4.2): which remote to
/// fetch and which default branch the settings name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    /// `push_remote` of the project settings.
    pub remote: String,
    /// `default_branch` of the project settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
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
        /// Set for a start of a synced project (FG-1): the fetch before the
        /// worktree. An effect stored without it runs without a fetch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<SyncPlan>,
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
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
        /// The Fleet Git snapshot of a start in a synced project (FG-1, FG-4):
        /// the settings the start effect uses (design §3.3, §7.3).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fleet_git: Option<Box<fleetdeck_proto::fleet_git::FleetGitStart>>,
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
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
        /// True for `session.import`: the dispatch then also fails closed
        /// for runs whose conversation id is not known (design
        /// `shim-resume.md` §13). An effect stored without it is a resume.
        #[serde(default, skip_serializing_if = "is_false")]
        import: bool,
        /// The pin of an import from the native-session list (B7): the
        /// dispatch verifies the transcript before any launch. It travels
        /// in the effect so that a restart before dispatch keeps it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<fleetdeck_proto::native::TranscriptPin>,
    },
    Prompt {
        input: InputRecord,
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    Steer {
        input: InputRecord,
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    FollowUp {
        input: InputRecord,
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    Interrupt {
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    Answer {
        request_id: RequestId,
        decision: ApprovalDecision,
        by: ResolvedBy,
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    SetMode {
        mode: String,
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    SetModel {
        model: String,
    },
    Stop {
        how: StopMode,
        /// The authority of a fleet-control command (ADR-008 section 6.1). An
        /// effect queued before this field existed has none and gets no check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
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
    /// A `!` shell command in the session folder (B8). The `shell_runs` row
    /// of the effect holds the state and the result.
    ShellCommand {
        command: String,
        provider: ProviderKind,
        /// The canonical root of the folder, as the fences name it.
        worktree_root: String,
        /// The run limit in ms, fixed when the command starts (knobs K3,
        /// `limits.shell_command_minutes`). A row of an older build has the
        /// old limit, 600 000.
        #[serde(default = "default_shell_timeout_ms")]
        timeout_ms: u64,
    },
    /// A land run (FG-8, design §11.4 step 3). It needs no provider process:
    /// the `land_runs` row holds the steps, and the land engine runs them.
    Land {
        land_id: String,
        /// The authority of a coordinator land (ADR-008 section 6.1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    /// The launch of a moved session on the target (FG-9, design §12.3 step
    /// 4). The decision stores the plan; the effect does not derive it again.
    MoveIn {
        move_id: String,
        /// The move kind (`kind` is the tag of the payload).
        move_kind: MoveKind,
        provider_instance: ProviderInstanceId,
        /// The normalized title of the new session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        callsign: String,
        /// The verified home of the project.
        home: String,
        /// The path of the new worktree.
        worktree: String,
        /// `B = publication.local_branch`, `oid = publication.oid`.
        publication: MovePublication,
        /// The source session, for the held prompt of a branch-only move
        /// (design §12.6).
        source_session: SessionId,
        /// The summary of the source session, cut to 4000 characters by the
        /// station (design §12.6).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        fleet_git: Box<fleetdeck_proto::fleet_git::FleetGitStart>,
        /// The authority of a coordinator move (ADR-008 section 6.1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<CommandAuthority>,
    },
    /// The stop of the processes of a lost run (BG-4, design `board-gaps.md`
    /// §5). It needs no provider process: the `execution_stops` row of
    /// `command_id` holds the state, and the stop plan `plan_id` lives in
    /// the station memory.
    StopExecution {
        command_id: String,
        plan_id: String,
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
            Self::Interrupt { .. } => "interrupt",
            Self::Answer { .. } => "answer",
            Self::SetMode { .. } => "set_mode",
            Self::SetModel { .. } => "set_model",
            Self::Stop { .. } => "stop",
            Self::Revert { .. } => "revert",
            Self::Worktree { .. } => "worktree",
            Self::ShellCommand { .. } => "shell_command",
            Self::Land { .. } => "land",
            Self::MoveIn { .. } => "move_in",
            Self::StopExecution { .. } => "stop_execution",
        }
    }

    /// True for effects that act on a running process (all but start, resume
    /// and worktree).
    #[must_use]
    pub fn needs_process(&self) -> bool {
        !matches!(
            self,
            Self::Start { .. }
                | Self::Resume { .. }
                | Self::Worktree { .. }
                | Self::Land { .. }
                | Self::MoveIn { .. }
                | Self::StopExecution { .. }
        )
    }

    /// A prompt effect; the decision adds the authority of its command.
    #[must_use]
    pub fn prompt(input: InputRecord) -> Self {
        Self::Prompt {
            input,
            authority: None,
        }
    }

    /// A steer effect; the decision adds the authority of its command.
    #[must_use]
    pub fn steer(input: InputRecord) -> Self {
        Self::Steer {
            input,
            authority: None,
        }
    }

    /// A follow-up effect; the decision adds the authority of its command.
    #[must_use]
    pub fn follow_up(input: InputRecord) -> Self {
        Self::FollowUp {
            input,
            authority: None,
        }
    }

    /// The authority of the fleet-control command that queued the effect
    /// (ADR-008 section 6.1). `None` for every other effect.
    #[must_use]
    pub fn authority(&self) -> Option<&CommandAuthority> {
        match self {
            Self::Start { authority, .. }
            | Self::Resume { authority, .. }
            | Self::Prompt { authority, .. }
            | Self::Steer { authority, .. }
            | Self::FollowUp { authority, .. }
            | Self::Interrupt { authority }
            | Self::Answer { authority, .. }
            | Self::SetMode { authority, .. }
            | Self::Stop { authority, .. }
            | Self::Land { authority, .. }
            | Self::MoveIn { authority, .. } => authority.as_ref(),
            Self::SetModel { .. }
            | Self::Revert { .. }
            | Self::Worktree { .. }
            | Self::ShellCommand { .. }
            | Self::StopExecution { .. } => None,
        }
    }

    /// The same effect with the authority of the command that queues it.
    /// An effect kind that a fleet-control command never queues keeps none.
    #[must_use]
    pub fn with_authority(mut self, value: Option<CommandAuthority>) -> Self {
        match &mut self {
            Self::Start { authority, .. }
            | Self::Resume { authority, .. }
            | Self::Prompt { authority, .. }
            | Self::Steer { authority, .. }
            | Self::FollowUp { authority, .. }
            | Self::Interrupt { authority }
            | Self::Answer { authority, .. }
            | Self::SetMode { authority, .. }
            | Self::Stop { authority, .. }
            | Self::Land { authority, .. }
            | Self::MoveIn { authority, .. } => *authority = value,
            Self::SetModel { .. }
            | Self::Revert { .. }
            | Self::Worktree { .. }
            | Self::ShellCommand { .. }
            | Self::StopExecution { .. } => {}
        }
        self
    }

    /// The host input that the effect delivers.
    #[must_use]
    pub fn input(&self) -> Option<&InputRecord> {
        match self {
            Self::Prompt { input, .. }
            | Self::Steer { input, .. }
            | Self::FollowUp { input, .. } => Some(input),
            Self::Start { prompt, .. } => prompt.as_ref(),
            _ => None,
        }
    }

    /// The key that the confirming evidence carries.
    pub(super) fn correlation(&self) -> Option<String> {
        match self {
            Self::Prompt { input, .. }
            | Self::Steer { input, .. }
            | Self::FollowUp { input, .. } => Some(input.input_id.0.clone()),
            Self::Answer { request_id, .. } => Some(request_id.0.clone()),
            Self::SetMode { mode, .. } => Some(mode.clone()),
            Self::SetModel { model } => Some(model.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn resume(import: bool) -> EffectPayload {
        EffectPayload::Resume {
            native_session_id: "N".to_owned(),
            spec: SessionSpec {
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                mode: None,
                model: None,
                title: None,
                coordinator: None,
                launch: None,
                extra_env: None,
            },
            prompt: None,
            continue_by: None,
            authority: None,
            import,
            transcript: None,
        }
    }

    #[test]
    fn a_stored_resume_without_the_import_flag_reads_as_a_resume() {
        let text = serde_json::to_string(&resume(true)).unwrap();
        assert!(text.contains("\"import\":true"), "{text}");
        let back: EffectPayload = serde_json::from_str(&text).unwrap();
        assert_eq!(back, resume(true));
        // A resume omits the flag, so an effect stored before it existed
        // has the same shape.
        let old = serde_json::to_string(&resume(false)).unwrap();
        assert!(!old.contains("import"), "{old}");
        let back: EffectPayload = serde_json::from_str(&old).unwrap();
        assert_eq!(back, resume(false));
    }

    #[test]
    fn the_import_pin_is_stored_in_the_effect_and_absent_without_one() {
        let mut pinned = resume(true);
        if let EffectPayload::Resume { transcript, .. } = &mut pinned {
            *transcript = Some(fleetdeck_proto::native::TranscriptPin {
                home: "/h/.claude".to_owned(),
                dev: 7,
                ino: 9,
            });
        }
        let text = serde_json::to_string(&pinned).unwrap();
        assert!(text.contains("\"transcript\""), "{text}");
        let back: EffectPayload = serde_json::from_str(&text).unwrap();
        assert_eq!(back, pinned);
        let plain = serde_json::to_string(&resume(true)).unwrap();
        assert!(!plain.contains("transcript"), "{plain}");
    }

    #[test]
    fn a_stored_shell_command_without_a_limit_reads_with_600_seconds() {
        let old =
            r#"{"kind":"shell_command","command":"ls","provider":"claude","worktree_root":"/w"}"#;
        let EffectPayload::ShellCommand { timeout_ms, .. } = serde_json::from_str(old).unwrap()
        else {
            panic!("not a shell command");
        };
        assert_eq!(timeout_ms, 600_000);
        let kept = old.replace("\"/w\"}", "\"/w\",\"timeout_ms\":60000}");
        let EffectPayload::ShellCommand { timeout_ms, .. } = serde_json::from_str(&kept).unwrap()
        else {
            panic!("not a shell command");
        };
        assert_eq!(timeout_ms, 60_000);
    }
}
