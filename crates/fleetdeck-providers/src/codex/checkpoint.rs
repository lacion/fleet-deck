//! The Codex adapter checkpoint (ADR-005 §2.1; DL-108).
//!
//! The checkpoint is the serialized [`CodexCore`] without its
//! [`CoreConfig`]: thread id, ordinal counters, the JSON-RPC id counter and
//! every pending host request correlation, inputs, queued follow-ups,
//! pending mode and model, the running turn, tools, a pending `!` shell
//! command (B8), and open and closed requests. The restoring station
//! passes the config again.
//! [`CodexCore::restore`] writes no frame: the app-server keeps running
//! under its shim and never sees a second `initialize` or `thread/start`.

use fleetdeck_proto::event::ProviderKind;

use super::core::CodexCore;
use super::{ClientInfo, CoreConfig, capabilities};
use crate::checkpoint::{AdapterCheckpoint, CheckpointError};

/// The checkpoint version that this build writes.
pub const CHECKPOINT_VERSION: u32 = 1;
/// The checkpoint versions that this build reads (its own and the previous
/// one; version 1 is the first).
pub const CHECKPOINT_READS: &[u32] = &[1];

/// The config of a core while it is decoded; [`CodexCore::restore`]
/// replaces it before the core is used.
pub(super) fn placeholder_config() -> CoreConfig {
    CoreConfig {
        provider_instance: "codex".into(),
        cwd: String::new(),
        capabilities: capabilities(false),
        resumed: None,
        client: ClientInfo::fleetdeck(),
        request_models: false,
    }
}

impl CodexCore {
    /// The checkpoint of this core.
    ///
    /// # Errors
    /// [`CheckpointError::Corrupt`] if the state does not serialize (not
    /// expected).
    pub fn checkpoint(&self) -> Result<AdapterCheckpoint, CheckpointError> {
        let state =
            serde_json::to_value(self).map_err(|e| CheckpointError::Corrupt(e.to_string()))?;
        Ok(AdapterCheckpoint {
            provider: ProviderKind::Codex,
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
        checkpoint.check(ProviderKind::Codex, CHECKPOINT_READS)?;
        let mut core: Self = serde_json::from_value(checkpoint.state.clone())
            .map_err(|e| CheckpointError::Corrupt(e.to_string()))?;
        core.config = config;
        // A checkpoint of a build before the usage totals (DL-445).
        if core.usage.is_none() {
            core.usage = Some(super::thread_usage::ThreadUsage::legacy());
        }
        Ok(core)
    }
}

/// `Option<Option<String>>` without the JSON loss of `Some(None)` (a held
/// interrupt without a control id): stored as `null` or `[control]`.
pub(super) mod held {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[allow(clippy::ref_option)]
    pub(in super::super) fn serialize<S: Serializer>(
        value: &Option<Option<String>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().map(|inner| [inner]).serialize(s)
    }

    pub(in super::super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<String>>, D::Error> {
        Ok(Option::<[Option<String>; 1]>::deserialize(d)?.map(|[inner]| inner))
    }
}
