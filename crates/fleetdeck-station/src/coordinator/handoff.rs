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
pub use fleetdeck_providers::coordinator::{CoordinatorLaunch, TOKEN_ENV};

use super::bindings::{self, BridgeEndpoint};

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

    /// Mints a credential for a shim-hosted child of `provider` that
    /// reaches the station through the MCP bridge (DL-194 §2.2).
    ///
    /// # Errors
    /// [`HandoffError`].
    pub fn prepare_bridge(
        provider: ProviderKind,
        bridge: &BridgeEndpoint,
    ) -> Result<Self, HandoffError> {
        let args = fleetdeck_providers::coordinator::bridge_args(
            provider,
            &bridge.program,
            &bridge.socket,
        )
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

    /// The adapter form of the handoff: the arguments and the credential.
    #[must_use]
    pub fn into_launch(self) -> CoordinatorLaunch {
        CoordinatorLaunch {
            args: self.args,
            token: self.token,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_bridge_handoff_has_the_arguments_of_the_provider() {
        let bridge = BridgeEndpoint {
            program: "/station/bin/current".into(),
            socket: "/run/mcp.s".into(),
        };
        let claude = Handoff::prepare_bridge(ProviderKind::Claude, &bridge).unwrap();
        assert!(claude.args.iter().any(|a| a.contains("mcp-bridge")));
        assert!(claude.args.iter().all(|a| !a.contains("fdc1_")));
        // The version gate is in `tools_for` (DL-194 §9.10).
        let codex = Handoff::prepare_bridge(ProviderKind::Codex, &bridge).unwrap();
        assert!(codex.args.iter().any(|a| a.contains("env_vars")));
        assert!(matches!(
            Handoff::prepare_bridge(ProviderKind::Unknown, &bridge),
            Err(HandoffError::NoArgs)
        ));
    }
}
