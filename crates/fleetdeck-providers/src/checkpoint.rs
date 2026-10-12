//! Versioned adapter checkpoints (ADR-005 §2.1; DL-108).
//!
//! An adapter checkpoint is the complete recoverable state of a sans-IO
//! adapter core: native identities, event-id counters, the RPC id counter
//! and its correlations, pending inputs and turns, open request mappings,
//! and permission, mode and model state. It holds no secret.
//!
//! The station commits the checkpoint of a core in the same transaction as
//! the work that changed it: with a stdin frame claim before the frame
//! leaves (outbound transitions), and with the events and the cursor of an
//! output transaction. After a station restart the station restores the
//! latest committed checkpoint into a new core, without spawn, `initialize`,
//! `thread/start` or a native resume, and then replays the shim output from
//! the committed cursor.
//!
//! Each adapter declares the version it writes and the versions it reads
//! (its own and the previous one). A checkpoint of another provider or of a
//! version the adapter cannot read is [`CheckpointError::Unsupported`]: the
//! session becomes `not_resumable` with [`REASON_UNSUPPORTED`]; the station
//! never substitutes a fresh adapter.

use fleetdeck_proto::event::ProviderKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `lost`/`not_resumable` reason of an unreadable checkpoint version.
pub const REASON_UNSUPPORTED: &str = "adapter_checkpoint_unsupported";
/// The reason of a checkpoint that does not decode.
pub const REASON_CORRUPT: &str = "adapter_checkpoint_corrupt";

/// The stored adapter state of one provider process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterCheckpoint {
    pub provider: ProviderKind,
    pub version: u32,
    pub state: Value,
}

/// Why a checkpoint cannot be restored.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CheckpointError {
    #[error("adapter checkpoint of {provider:?} version {version} is not supported")]
    Unsupported {
        provider: ProviderKind,
        version: u32,
    },
    #[error("the adapter checkpoint is corrupt: {0}")]
    Corrupt(String),
}

impl CheckpointError {
    /// The session reason of this error.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Unsupported { .. } => REASON_UNSUPPORTED,
            Self::Corrupt(_) => REASON_CORRUPT,
        }
    }
}

impl AdapterCheckpoint {
    /// The stored text form.
    ///
    /// # Errors
    /// A serialization error (not expected for JSON values).
    pub fn to_json(&self) -> Result<String, CheckpointError> {
        serde_json::to_string(self).map_err(|e| CheckpointError::Corrupt(e.to_string()))
    }

    /// Decodes the stored text form.
    ///
    /// # Errors
    /// [`CheckpointError::Corrupt`] for text that is not a checkpoint.
    pub fn from_json(text: &str) -> Result<Self, CheckpointError> {
        serde_json::from_str(text).map_err(|e| CheckpointError::Corrupt(e.to_string()))
    }

    /// Checks that `provider` can read this checkpoint with one of the
    /// versions in `reads`.
    ///
    /// # Errors
    /// [`CheckpointError::Unsupported`] otherwise.
    pub fn check(&self, provider: ProviderKind, reads: &[u32]) -> Result<(), CheckpointError> {
        if self.provider == provider && reads.contains(&self.version) {
            Ok(())
        } else {
            Err(CheckpointError::Unsupported {
                provider: self.provider,
                version: self.version,
            })
        }
    }
}

/// The checkpoint versions that this build reads for `provider`.
#[must_use]
pub fn reads(provider: ProviderKind) -> &'static [u32] {
    match provider {
        ProviderKind::Claude => crate::claude::CHECKPOINT_READS,
        ProviderKind::Codex => crate::codex::CHECKPOINT_READS,
        ProviderKind::Unknown => &[],
    }
}

/// Decodes a stored checkpoint of a `provider` session and checks that this
/// build reads it, before any restore (ADR-005 §2.1 and test 37).
///
/// # Errors
/// [`CheckpointError::Corrupt`] or [`CheckpointError::Unsupported`].
pub fn readable(provider: ProviderKind, text: &str) -> Result<AdapterCheckpoint, CheckpointError> {
    let checkpoint = AdapterCheckpoint::from_json(text)?;
    checkpoint.check(provider, reads(provider))?;
    Ok(checkpoint)
}
