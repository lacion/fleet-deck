//! What a station needs to run a session under a durable host (ADR-005;
//! DL-113): the process to launch under a shim and the configuration of the
//! sans-IO core that the station drives itself.
//!
//! [`crate::ProviderAdapter::durable_launch`] builds it with the same
//! workspace, capability, mode and argument rules as the adapter's direct
//! start, but starts nothing. The station launches the process under a shim
//! and drives the core with its durable driver; a restart restores the core
//! from the stored checkpoint with the same configuration.

use crate::host::{ProcessSpec, StopPolicy};
use crate::{claude, codex};

/// The core configuration of a durable session.
#[derive(Debug, Clone, PartialEq)]
pub enum DurableConfig {
    Claude(claude::CoreConfig),
    /// The core configuration and the thread that the handshake opens.
    Codex {
        config: codex::CoreConfig,
        thread: codex::SessionSpec,
    },
}

/// A durable launch: the process, the core configuration and the stop
/// policy of a graceful stop.
#[derive(Debug, Clone)]
pub struct DurableLaunch {
    pub process: ProcessSpec,
    pub core: DurableConfig,
    pub stop: StopPolicy,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use async_trait::async_trait;
    use fleetdeck_proto::Capabilities;
    use fleetdeck_proto::command::SessionSpec;
    use fleetdeck_proto::event::ProviderKind;

    use crate::adapter::{
        EventSink, LiveSession, NativeSessionId, ProviderAdapter, ProviderError, ProviderProbe,
        Result,
    };

    /// An adapter that overrides nothing optional.
    struct Plain;

    #[async_trait]
    impl ProviderAdapter for Plain {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Claude
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::minimal()
        }
        async fn probe(&self) -> Result<ProviderProbe> {
            Err(ProviderError::SessionEnded)
        }
        async fn start(&self, _: SessionSpec, _: EventSink) -> Result<Box<dyn LiveSession>> {
            Err(ProviderError::SessionEnded)
        }
        async fn resume(
            &self,
            _: &NativeSessionId,
            _: SessionSpec,
            _: EventSink,
        ) -> Result<Box<dyn LiveSession>> {
            Err(ProviderError::SessionEnded)
        }
    }

    #[test]
    fn the_trait_default_of_a_durable_resume_is_unsupported() {
        let spec = SessionSpec {
            provider_instance: "x".into(),
            cwd: "/w".to_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
        };
        let err = Plain
            .durable_resume_launch(&spec, &NativeSessionId("n".to_owned()))
            .unwrap_err();
        assert!(matches!(err, ProviderError::Unsupported(_)), "{err:?}");
        // The launch of a new session is still refused as before.
        assert!(Plain.durable_launch(&spec).is_err());
    }
}
