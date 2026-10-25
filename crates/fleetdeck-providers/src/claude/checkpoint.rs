//! The Claude adapter checkpoint (ADR-005 §2.1; DL-108).
//!
//! The checkpoint is the serialized [`ClaudeCore`] without its
//! [`CoreConfig`]: the native session id, ordinal and control-request
//! counters with their correlations, inputs and their fates, mode and model,
//! the running turn, tools, open and closed requests, and background work.
//! [`ClaudeCore::restore`] writes no frame: the CLI keeps running under its
//! shim and never sees a second `initialize`.

use fleetdeck_proto::event::ProviderKind;

use super::args::capabilities;
use super::core::{ClaudeCore, CoreConfig};
use super::process_usage::{ProcessUsage, UsageBase};
use crate::checkpoint::{AdapterCheckpoint, CheckpointError};

/// The checkpoint version that this build writes.
pub const CHECKPOINT_VERSION: u32 = 1;
/// The checkpoint versions that this build reads (its own and the previous
/// one; version 1 is the first).
pub const CHECKPOINT_READS: &[u32] = &[1];

/// The config of a core while it is decoded; [`ClaudeCore::restore`]
/// replaces it before the core is used.
pub(super) fn placeholder_config() -> CoreConfig {
    CoreConfig {
        provider_instance: "claude".into(),
        cwd: String::new(),
        capabilities: capabilities(false),
        resumed: None,
        usage_base: UsageBase::Zero,
    }
}

impl ClaudeCore {
    /// The checkpoint of this core.
    ///
    /// # Errors
    /// [`CheckpointError::Corrupt`] if the state does not serialize (not
    /// expected).
    pub fn checkpoint(&self) -> Result<AdapterCheckpoint, CheckpointError> {
        let state =
            serde_json::to_value(self).map_err(|e| CheckpointError::Corrupt(e.to_string()))?;
        Ok(AdapterCheckpoint {
            provider: ProviderKind::Claude,
            version: CHECKPOINT_VERSION,
            state,
        })
    }

    /// A core in the state of `checkpoint`, with `config`. No output.
    ///
    /// # Errors
    /// [`CheckpointError::Unsupported`] for another provider or an
    /// unreadable version; [`CheckpointError::Corrupt`] for a state that
    /// does not decode.
    pub fn restore(
        config: CoreConfig,
        checkpoint: &AdapterCheckpoint,
    ) -> Result<Self, CheckpointError> {
        checkpoint.check(ProviderKind::Claude, CHECKPOINT_READS)?;
        let mut core: Self = serde_json::from_value(checkpoint.state.clone())
            .map_err(|e| CheckpointError::Corrupt(e.to_string()))?;
        core.config = config;
        // A checkpoint of a build before the per-model totals (DL-442).
        if core.usage.is_none() {
            core.usage = Some(ProcessUsage::legacy(core.last_total_cost));
        }
        Ok(core)
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::Event;
    use serde_json::json;

    use super::super::tests::{answer, can_use_tool, config, running_core, tool_use, writes};
    use super::*;

    #[test]
    fn a_restored_claude_core_answers_an_approval_opened_before_the_crash() {
        let mut core = running_core();
        tool_use(&mut core, "tu1", "Bash", json!({"command": "ls"}));
        let ev = can_use_tool(&mut core, "req-9", "tu1");
        let Some(Event::ApprovalRequested { request_id, .. }) = ev
            .iter()
            .find(|e| matches!(e, Event::ApprovalRequested { .. }))
            .cloned()
        else {
            panic!("no approval: {ev:?}")
        };
        let text = core.checkpoint().unwrap().to_json().unwrap();
        drop(core);
        let stored = AdapterCheckpoint::from_json(&text).unwrap();
        let mut core = ClaudeCore::restore(config(false), &stored).unwrap();
        assert_eq!(core.checkpoint().unwrap(), stored);
        let out = core.answer(&answer(request_id.as_str(), "allow")).unwrap();
        let w = writes(&out);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0]["response"]["request_id"], "req-9");
        assert!(w.iter().all(|f| f["request"]["subtype"] != "initialize"));
        let wrong = AdapterCheckpoint {
            version: CHECKPOINT_VERSION + 1,
            ..stored
        };
        assert_eq!(
            ClaudeCore::restore(config(false), &wrong)
                .unwrap_err()
                .reason(),
            crate::checkpoint::REASON_UNSUPPORTED
        );
    }
}
