//! The reasoning effort of the Codex thread (B18, docs/v2/design/effort.md
//! §4.2): from the thread answer and from `thread/settings/updated`.

use fleetdeck_proto::Event;
use fleetdeck_proto::effort::effort_of;
use serde_json::Value;

use super::core::{CodexCore, Output};

impl CodexCore {
    /// Emits `effort.changed` when the valid effort differs from the last
    /// one that this core emitted.
    fn effort_seen(&mut self, value: Option<&str>) -> Vec<Output> {
        let effort = effort_of(value);
        if effort == self.thread_effort {
            return Vec::new();
        }
        self.thread_effort.clone_from(&effort);
        vec![Output::Emit(Event::EffortChanged { effort })]
    }

    /// The effort of a `thread/start`, `thread/resume` or `thread/fork`
    /// answer: `reasoningEffort`, else `thread.reasoningEffort`.
    pub(super) fn effort_of_thread_answer(&mut self, r: &Value) -> Vec<Output> {
        let value = r
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .or_else(|| r.pointer("/thread/reasoningEffort").and_then(Value::as_str));
        self.effort_seen(value)
    }

    /// `thread/settings/updated` of the own thread.
    pub(super) fn on_settings_updated(&mut self, p: &Value) -> Vec<Output> {
        self.effort_seen(p.pointer("/threadSettings/effort").and_then(Value::as_str))
    }
}
