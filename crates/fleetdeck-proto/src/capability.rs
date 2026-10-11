//! Provider capabilities and typed degradations (PLAN §6.2, L5, L11).
//!
//! A live session reports its capabilities; the board shows a control only
//! when the capability allows it. An operation that the provider does not
//! support returns [`Unsupported`] with the degradation policy of its matrix
//! row, never a raw provider error.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::ResponseChannel;

/// How reliably a provider reports the end of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "capability/")]
pub enum TerminalStatusQuality {
    /// One definite terminal frame per turn.
    Strong,
    /// Terminal frames can be missing; the station also uses an idle timeout.
    Weak,
    /// No terminal frames; the status comes from an idle timeout only.
    None,
}

/// How a provider reports background work after a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "capability/")]
pub enum BackgroundReporting {
    /// The provider pushes changes (Claude `background_tasks_changed`).
    Pushed,
    /// The adapter polls (Codex `thread/backgroundTerminals/list`).
    Polled,
    /// The provider does not report background work.
    None,
}

/// What a provider reports about usage. Unknown is not zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "capability/")]
pub struct UsageReporting {
    pub tokens: bool,
    pub cost: bool,
}

/// The capability matrix of one live session (PLAN §6.2 rows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "capability/")]
pub struct Capabilities {
    // sessions
    pub resume: bool,
    pub fork: bool,
    /// Open runtime requests survive a provider restart.
    pub requests_survive_restart: bool,
    // turns
    pub interrupt: bool,
    pub steer: bool,
    /// The provider queues a follow-up natively; otherwise the station queues it.
    pub native_follow_up: bool,
    pub terminal_status: TerminalStatusQuality,
    // streaming and tools
    pub live_deltas: bool,
    pub file_changes: bool,
    // approvals
    pub approvals: bool,
    pub response_channels: Vec<ResponseChannel>,
    pub edit_input: bool,
    /// Native permission modes, the most restrictive first (L8). Empty: no modes.
    pub modes: Vec<String>,
    pub set_mode: bool,
    pub set_model: bool,
    // planning, subagents, identity, usage
    pub plan_capture: bool,
    pub subagents: bool,
    pub background_work: BackgroundReporting,
    pub native_turn_ids: bool,
    pub usage: UsageReporting,
}

impl Capabilities {
    /// A provider that supports nothing optional. Useful as a base for fakes.
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            resume: false,
            fork: false,
            requests_survive_restart: false,
            interrupt: false,
            steer: false,
            native_follow_up: false,
            terminal_status: TerminalStatusQuality::None,
            live_deltas: false,
            file_changes: false,
            approvals: false,
            response_channels: Vec::new(),
            edit_input: false,
            modes: Vec::new(),
            set_mode: false,
            set_model: false,
            plan_capture: false,
            subagents: false,
            background_work: BackgroundReporting::None,
            native_turn_ids: false,
            usage: UsageReporting {
                tokens: false,
                cost: false,
            },
        }
    }

    /// True if the session supports `op` natively.
    #[must_use]
    pub fn supports(&self, op: Operation) -> bool {
        match op {
            // Prompt and stop are always possible; a follow-up degrades to a
            // station queue, which is still a supported operation.
            Operation::Prompt | Operation::Stop | Operation::FollowUp => true,
            Operation::Resume => self.resume,
            Operation::Fork => self.fork,
            Operation::Interrupt => self.interrupt,
            Operation::Steer => self.steer,
            Operation::Answer => self.approvals,
            Operation::EditInput => self.approvals && self.edit_input,
            Operation::SetMode => self.set_mode && !self.modes.is_empty(),
            Operation::SetModel => self.set_model,
        }
    }

    /// `Ok` if `op` is supported, otherwise the typed degradation (L5).
    pub fn check(&self, op: Operation) -> Result<(), Unsupported> {
        if self.supports(op) {
            Ok(())
        } else {
            Err(Unsupported::new(op))
        }
    }

    /// The mode a session runs in when its stored mode is not offered (L8).
    #[must_use]
    pub fn most_restrictive_mode(&self) -> Option<&str> {
        self.modes.first().map(String::as_str)
    }

    /// The mode to use for a requested mode: the request if native, else the
    /// most restrictive native mode (L8). `None` if the provider has no modes.
    #[must_use]
    pub fn effective_mode<'a>(&'a self, requested: Option<&'a str>) -> Option<&'a str> {
        match requested {
            Some(mode) if self.modes.iter().any(|m| m == mode) => Some(mode),
            _ => self.most_restrictive_mode(),
        }
    }
}

/// An operation on a live session that a capability can forbid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "capability/")]
pub enum Operation {
    Prompt,
    Steer,
    FollowUp,
    Interrupt,
    Answer,
    EditInput,
    SetMode,
    SetModel,
    Fork,
    Resume,
    Stop,
}

impl Operation {
    /// Every operation, for exhaustive tests.
    pub const ALL: [Self; 11] = [
        Self::Prompt,
        Self::Steer,
        Self::FollowUp,
        Self::Interrupt,
        Self::Answer,
        Self::EditInput,
        Self::SetMode,
        Self::SetModel,
        Self::Fork,
        Self::Resume,
        Self::Stop,
    ];

    /// The degradation policy of the matrix row of this operation (PLAN §6.2).
    #[must_use]
    pub fn degradation(self) -> Degradation {
        match self {
            Self::Resume => Degradation::StartNewSession,
            Self::Fork => Degradation::Hide,
            Self::Interrupt => Degradation::KillProcess,
            Self::Steer => Degradation::InterruptAndRestart,
            Self::FollowUp => Degradation::StationQueue,
            Self::Answer => Degradation::NoApprovalControls,
            Self::EditInput => Degradation::AllowDenyOnly,
            Self::SetMode => Degradation::MostRestrictiveMode,
            Self::SetModel | Self::Prompt | Self::Stop => Degradation::Hide,
        }
    }
}

/// What the station and the board do instead of a missing capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "capability/")]
pub enum Degradation {
    /// Start a new session and mark it "not resumed".
    StartNewSession,
    /// Hide the control.
    Hide,
    /// Stop the provider process (`StopMode::Kill`).
    KillProcess,
    /// Interrupt, then restart with the steer text as a replacement attempt.
    InterruptAndRestart,
    /// The station queues the input until `turn.completed`.
    StationQueue,
    /// The provider runs under its own sandbox policy; no approval controls.
    NoApprovalControls,
    /// Allow or deny only, without an edited input.
    AllowDenyOnly,
    /// The session runs in the most restrictive native mode.
    MostRestrictiveMode,
}

/// The typed result of an unsupported operation (L5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("operation `{op:?}` is not supported by this provider (degradation: {degradation:?})")]
#[ts(export_to = "capability/")]
pub struct Unsupported {
    pub op: Operation,
    pub degradation: Degradation,
}

impl Unsupported {
    /// The unsupported result for `op` with its matrix degradation.
    #[must_use]
    pub fn new(op: Operation) -> Self {
        Self {
            op,
            degradation: op.degradation(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_provider_supports_only_prompt_stop_and_a_station_queue() {
        let caps = Capabilities::minimal();
        let supported: Vec<Operation> = Operation::ALL
            .into_iter()
            .filter(|op| caps.supports(*op))
            .collect();
        assert_eq!(
            supported,
            vec![Operation::Prompt, Operation::FollowUp, Operation::Stop]
        );
        let err = caps.check(Operation::Interrupt).unwrap_err();
        assert_eq!(err.degradation, Degradation::KillProcess);
    }

    #[test]
    fn edit_input_needs_approvals() {
        let mut caps = Capabilities::minimal();
        caps.edit_input = true;
        assert!(!caps.supports(Operation::EditInput));
        caps.approvals = true;
        assert!(caps.supports(Operation::EditInput));
    }

    #[test]
    fn an_unknown_stored_mode_runs_as_the_most_restrictive_mode() {
        let mut caps = Capabilities::minimal();
        caps.modes = vec!["default".into(), "acceptEdits".into()];
        assert_eq!(
            caps.effective_mode(Some("acceptEdits")),
            Some("acceptEdits")
        );
        assert_eq!(caps.effective_mode(Some("bypass")), Some("default"));
        assert_eq!(caps.effective_mode(None), Some("default"));
        assert_eq!(Capabilities::minimal().effective_mode(Some("x")), None);
    }

    #[test]
    fn unsupported_serializes_as_a_typed_result() {
        let v = serde_json::to_value(Unsupported::new(Operation::Steer)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"op": "steer", "degradation": "interrupt_and_restart"})
        );
    }
}
