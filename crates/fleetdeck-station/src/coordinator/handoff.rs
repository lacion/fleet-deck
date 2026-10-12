//! The private credential handoff to a coordinator child (ADR-006 §5.4,
//! §8.2; DL-215).
//!
//! The station mints the credential for one start. The raw value lives in
//! a [`Secret`] and goes only into the child's environment variable
//! [`TOKEN_ENV`]; the binding stores its digest. The provider arguments
//! name the variable, never the value, so the value is not in the child's
//! argv, in a debug format, in a log line or in an event.

use fleetdeck_core::childenv::ChildEnvError;
use fleetdeck_core::{ChildEnv, Secret};
use fleetdeck_proto::event::ProviderKind;
pub use fleetdeck_providers::coordinator::TOKEN_ENV;

use super::bindings;

/// Why a handoff cannot be prepared.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("the provider has no coordinator arguments for this endpoint")]
    NoArgs,
    #[error("the station could not mint a coordinator credential")]
    Mint,
}

/// One prepared handoff: the provider arguments, the digest for the
/// binding and the credential for the child environment.
#[derive(Debug)]
pub struct Handoff {
    /// Extra provider arguments; they name [`TOKEN_ENV`] only.
    pub args: Vec<String>,
    /// The SHA-256 digest that the binding stores.
    pub digest: String,
    token: Secret<String>,
}

impl Handoff {
    /// Mints a credential for a child of `provider` that reaches the
    /// loopback endpoint `url`.
    ///
    /// # Errors
    /// [`HandoffError`].
    pub fn prepare(provider: ProviderKind, url: &str) -> Result<Self, HandoffError> {
        let args = fleetdeck_providers::coordinator::mcp_args(provider, url)
            .ok_or(HandoffError::NoArgs)?;
        let (token, digest) = bindings::mint().map_err(|_| HandoffError::Mint)?;
        Ok(Self {
            args,
            digest,
            token,
        })
    }

    /// Puts the credential into the child environment, the only place
    /// where the raw value goes.
    ///
    /// # Errors
    /// [`ChildEnvError`] for an invalid value.
    pub fn apply(&self, env: &mut ChildEnv) -> Result<(), ChildEnvError> {
        env.set(TOKEN_ENV, self.token.expose())
    }
}
