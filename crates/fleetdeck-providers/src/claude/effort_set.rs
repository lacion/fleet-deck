//! A change of the reasoning effort of a Claude session (B18 Phase 2,
//! docs/v2/design/effort.md §9.2 item 6).
//!
//! The host writes `apply_flag_settings {"settings": {"effortLevel": e}}`. The
//! CLI answers `{}`: that is the acceptance, not the level (the CLI caps and
//! clamps). The acceptance emits `effort.set_result` with outcome `accepted`
//! (it persists the choice and confirms the station effect). A `get_settings`
//! read that the host writes after the answer verifies the level.
//!
//! The state is in `#[serde(default)]` fields of the core, never in a new
//! `HostRequest` variant: an older build must decode the checkpoint (§9.5).

use fleetdeck_proto::Event;
use fleetdeck_proto::effort::{choice_for, effort_of, same_model};
use fleetdeck_proto::event::{EffortSetOutcome, ModelChoice};
use serde_json::Value;

use super::control;
use super::core::{ClaudeCore, Output};
use super::frames::ControlResponse;
use crate::adapter::{ProviderError, Result};

/// The id prefix of the host's `apply_flag_settings` effort requests.
pub const EFFORT_ID_PREFIX: &str = "fd-effort-";

/// The longest model text that a settings answer can set.
const MAX_MODEL_CHARS: usize = 200;

/// One effort change that the host wrote and has not finished.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct EffortSet {
    pub(super) requested: String,
    pub(super) control_id: Option<String>,
    /// `None`: waits for the `apply_flag_settings` answer. `Some(n)`:
    /// applied; waits for a `get_settings` answer with an id above `n`.
    pub(super) verify_after: Option<u64>,
}

impl ClaudeCore {
    /// Changes the reasoning effort. The write is the `apply_flag_settings`
    /// request; the result comes as `effort.set_result` events.
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the exit;
    /// [`ProviderError::EffortNotOffered`] for a level that the advertised
    /// levels of the current model do not contain (nothing is written).
    pub fn set_effort(&mut self, effort: &str, control_id: Option<String>) -> Result<Vec<Output>> {
        if self.ended {
            return Err(ProviderError::SessionEnded);
        }
        let current = self.applied_model.as_deref().or(self.model.as_deref());
        let levels = current
            .and_then(|m| choice_for(&self.model_choices, m))
            .and_then(|c| c.efforts.as_ref());
        if levels.is_some_and(|list| !list.iter().any(|l| l == effort)) {
            return Err(ProviderError::EffortNotOffered(effort.to_owned()));
        }
        self.effort_seq += 1;
        let n = self.effort_seq;
        self.effort_sets.insert(
            n,
            EffortSet {
                requested: effort.to_owned(),
                control_id,
                verify_after: None,
            },
        );
        let id = format!("{EFFORT_ID_PREFIX}{n}");
        Ok(vec![Output::Write(control::apply_effort(&id, effort))])
    }
}

/// The `N` of an `fd-effort-N` request id.
fn effort_seq_of(request_id: &str) -> Option<u64> {
    request_id.strip_prefix(EFFORT_ID_PREFIX)?.parse().ok()
}

/// The result event of a set.
fn result(
    set: &EffortSet,
    effort: Option<String>,
    outcome: EffortSetOutcome,
    message: Option<String>,
) -> Output {
    Output::Emit(Event::EffortSetResult {
        control_id: set.control_id.clone(),
        requested: set.requested.clone(),
        effort,
        outcome,
        message,
    })
}

/// The answer to an `apply_flag_settings` effort request (its id starts with
/// [`EFFORT_ID_PREFIX`]). An answer never sets `core.effort` and never emits
/// `effort.changed`: the level is known only from a `get_settings` read.
pub(super) fn on_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output> {
    let Some(n) = effort_seq_of(&resp.request_id) else {
        return Vec::new();
    };
    if !resp.ok {
        let Some(set) = core.effort_sets.remove(&n) else {
            return Vec::new();
        };
        return vec![result(
            &set,
            None,
            EffortSetOutcome::Refused,
            resp.error.clone(),
        )];
    }
    let Some(set) = core.effort_sets.get_mut(&n) else {
        return Vec::new();
    };
    // The acceptance comes first: it persists the choice before any read.
    let mut out = vec![result(set, None, EffortSetOutcome::Accepted, None)];
    if core.settings_stopped {
        out.push(result(set, None, EffortSetOutcome::Unverified, None));
        core.effort_sets.remove(&n);
        return out;
    }
    set.verify_after = Some(core.settings_seq);
    out.extend(core.settings_request());
    out
}

/// The effects of a `get_settings` answer with id `seq` that carries
/// `applied`: the model correction, then the verification of the sets that
/// were applied before the read was written. `observed` is the validated
/// `applied.effort`.
pub(super) fn after_settings(
    core: &mut ClaudeCore,
    applied: &Value,
    observed: Option<&str>,
    seq: u64,
) -> Vec<Output> {
    let mut out = Vec::new();
    // A read written before the last model change names the old model.
    if let Some(model) = applied
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| seq > core.model_set_seq && model_text_ok(m))
    {
        core.applied_model = Some(model.to_owned());
        match core.reported_model.as_deref() {
            Some(reported) if !same_model(&core.model_choices, model, reported) => {
                core.model = Some(model.to_owned());
                core.reported_model = Some(model.to_owned());
                out.push(Output::Emit(Event::ModelChanged {
                    model: model.to_owned(),
                }));
            }
            Some(_) => {}
            None => core.reported_model = Some(model.to_owned()),
        }
    }
    let due: Vec<u64> = core
        .effort_sets
        .iter()
        .filter(|(_, s)| s.verify_after.is_some_and(|n| n < seq))
        .map(|(id, _)| *id)
        .collect();
    for id in due {
        if let Some(set) = core.effort_sets.remove(&id) {
            let outcome = if observed == Some(set.requested.as_str()) {
                EffortSetOutcome::Applied
            } else {
                EffortSetOutcome::Adjusted
            };
            out.push(result(&set, observed.map(str::to_owned), outcome, None));
        }
    }
    out
}

/// A successful model change: the applied model is not known until a
/// `get_settings` read that is written after the change answers, so
/// [`ClaudeCore::set_effort`] uses the new `model` until then.
pub(super) fn on_model_set(core: &mut ClaudeCore) {
    core.applied_model = None;
    core.model_set_seq = core.settings_seq;
}

/// The asks stopped: no read will verify the applied sets.
pub(super) fn stop_verifying(core: &mut ClaudeCore) -> Vec<Output> {
    let ids: Vec<u64> = core
        .effort_sets
        .iter()
        .filter(|(_, s)| s.verify_after.is_some())
        .map(|(id, _)| *id)
        .collect();
    ids.into_iter()
        .filter_map(|id| core.effort_sets.remove(&id))
        .map(|set| result(&set, None, EffortSetOutcome::Unverified, None))
        .collect()
}

/// A model text of 1 to 200 characters without control characters.
fn model_text_ok(text: &str) -> bool {
    (1..=MAX_MODEL_CHARS).contains(&text.chars().count()) && !text.chars().any(char::is_control)
}

/// The models of an `initialize` response for the board, in the CLI order:
/// `value` is what `set_model` takes; an entry without `value` is dropped
/// (DL-151). `resolved` is `resolvedModel`; `efforts` is
/// `supportedEffortLevels` when it is an array, empty when `supportsEffort`
/// is `false`, else `None` (B18 Phase 2).
pub(super) fn model_choices(response: &Value) -> Vec<ModelChoice> {
    let text = |m: &Value, k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
    response
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let value = text(m, "value")?;
            Some(ModelChoice {
                resolved: text(m, "resolvedModel"),
                efforts: efforts_of(m),
                default_effort: None,
                label: text(m, "displayName").unwrap_or_else(|| value.clone()),
                description: text(m, "description"),
                value,
            })
        })
        .collect()
}

/// The effort levels of one advertised model.
fn efforts_of(model: &Value) -> Option<Vec<String>> {
    if let Some(levels) = model.get("supportedEffortLevels").and_then(Value::as_array) {
        return Some(
            levels
                .iter()
                .filter_map(|l| effort_of(l.as_str()))
                .collect(),
        );
    }
    (model.get("supportsEffort").and_then(Value::as_bool) == Some(false)).then(Vec::new)
}
