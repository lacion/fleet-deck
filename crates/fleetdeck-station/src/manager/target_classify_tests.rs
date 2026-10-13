//! The classification of a provider error as a dispatch state.

use std::io;

use fleetdeck_providers::ProviderError;
use fleetdeck_providers::host::HostError;

use super::classify;
use crate::store::Dispatch;

#[test]
fn a_write_that_can_be_partial_is_unknown_and_an_ended_session_is_not_dispatched() {
    let partial = ProviderError::Host(HostError::Write(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "the stdin writer stopped",
    )));
    assert!(matches!(classify(&partial), Dispatch::Unknown(_)));
    assert!(matches!(
        classify(&ProviderError::SessionEnded),
        Dispatch::NotDispatched(_)
    ));
}
