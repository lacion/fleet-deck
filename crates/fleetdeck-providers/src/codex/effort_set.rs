//! A change of the reasoning effort of a Codex session (B18 Phase 2,
//! docs/v2/design/effort.md §9.2 item 7).
//!
//! Codex takes the effort with each `turn/start`, like the model: a change
//! waits in `pending_effort` and goes with the next turn. The request carries
//! the pick that it sent, so its answer settles that pick and leaves a newer
//! choice for the next turn. The pick is `accepted` at the success answer of
//! its turn (it persists the choice), then `applied` or `unverified`.
//!
//! The state is in `#[serde(default)]` fields of the core and a defaulted
//! field of the existing `HostRequest::TurnStart` variant (§9.5).

use fleetdeck_proto::Event;
use fleetdeck_proto::effort::{choice_for, effort_of};
use fleetdeck_proto::event::EffortSetOutcome;
use serde_json::{Map, Value, json};

use super::core::{CodexCore, EffortPick, Output};
use crate::adapter::{ProviderError, Result};

/// The per-session effort of `thread/start`, `thread/resume` and
/// `thread/fork`: `config.model_reasoning_effort`, so that the thread answer
/// reports it at once.
pub(super) fn add_thread_config(params: &mut Map<String, Value>, effort: Option<&str>) {
    if let Some(e) = effort {
        params.insert("config".into(), json!({"model_reasoning_effort": e}));
    }
}

/// The effort levels of one `model/list` entry
/// (`supportedReasoningEfforts[].reasoningEffort`); `None` when the entry
/// has no list. An invalid level is dropped.
pub(super) fn model_efforts(model: &Value) -> Option<Vec<String>> {
    let list = model.get("supportedReasoningEfforts")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|o| effort_of(o.get("reasoningEffort").and_then(Value::as_str)))
            .collect(),
    )
}

/// The `defaultReasoningEffort` of one `model/list` entry.
pub(super) fn model_default_effort(model: &Value) -> Option<String> {
    effort_of(model.get("defaultReasoningEffort").and_then(Value::as_str))
}

/// The result event of a pick.
fn result(pick: &EffortPick, effort: Option<String>, outcome: EffortSetOutcome) -> Output {
    Output::Emit(Event::EffortSetResult {
        control_id: pick.control_id.clone(),
        requested: pick.effort.clone(),
        effort,
        outcome,
        message: None,
    })
}

impl CodexCore {
    /// Changes the reasoning effort for the next turn. Nothing is written:
    /// the pick goes with the next `turn/start`. An older pick that no turn
    /// sent is superseded.
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the exit;
    /// [`ProviderError::EffortNotOffered`] for a level that the advertised
    /// levels of the current model do not contain.
    pub fn set_effort(&mut self, effort: &str, control_id: Option<String>) -> Result<Vec<Output>> {
        self.live()?;
        let current = self
            .pending_model
            .as_deref()
            .or(self.thread_model.as_deref())
            .or(self.spec.model.as_deref());
        let levels = current
            .and_then(|m| choice_for(&self.model_choices, m))
            .and_then(|c| c.efforts.as_ref());
        if levels.is_some_and(|list| !list.iter().any(|l| l == effort)) {
            return Err(ProviderError::EffortNotOffered(effort.to_owned()));
        }
        let old = self.pending_effort.replace(EffortPick {
            effort: effort.to_owned(),
            control_id,
        });
        Ok(old
            .map(|old| vec![result(&old, None, EffortSetOutcome::Superseded)])
            .unwrap_or_default())
    }

    /// Takes the pending pick for a `turn/start`. Returns the pick (it
    /// travels with its request) and the effort that the turn sends: the
    /// pick, else the session effort.
    pub(super) fn take_effort(&mut self) -> (Option<EffortPick>, Option<String>) {
        let pick = self.pending_effort.take();
        let effort = pick
            .as_ref()
            .map(|p| p.effort.clone())
            .or_else(|| self.spec.effort.clone());
        (pick, effort)
    }

    /// The success answer of a `turn/start` that carried `pick`: the
    /// session effort becomes the pick; `accepted` persists the choice, then
    /// `applied` when the thread reports the level, else `unverified`.
    /// A choice that was made while the request was open stays pending.
    pub(super) fn on_turn_effort(&mut self, pick: Option<EffortPick>) -> Vec<Output> {
        let Some(p) = pick else {
            return Vec::new();
        };
        self.spec.effort = Some(p.effort.clone());
        let verified = if self.thread_effort.as_deref() == Some(p.effort.as_str()) {
            result(&p, self.thread_effort.clone(), EffortSetOutcome::Applied)
        } else {
            result(&p, None, EffortSetOutcome::Unverified)
        };
        vec![result(&p, None, EffortSetOutcome::Accepted), verified]
    }

    /// The error answer of a `turn/start` that carried `pick`: the pick
    /// waits again when nothing newer waits, else it is superseded.
    pub(super) fn on_turn_refused_effort(&mut self, pick: Option<EffortPick>) -> Vec<Output> {
        let Some(p) = pick else {
            return Vec::new();
        };
        if self.pending_effort.is_none() {
            self.pending_effort = Some(p);
            return Vec::new();
        }
        vec![result(&p, None, EffortSetOutcome::Superseded)]
    }
}
