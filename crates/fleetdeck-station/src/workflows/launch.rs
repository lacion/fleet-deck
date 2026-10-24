//! The launch facts of an adopted process (B12 §4.4, DL-191).

use fleetdeck_proto::SessionId;

use crate::history::subagent::LaunchFacts;
use crate::store::{ProcessIncarnation, Store};

/// The facts that the launch of `process` stored, for the DL-191 subagent
/// observer of an adopted session. `None` when the launch stored none (a
/// process of an older build) and on a store error (warn log).
pub async fn adopted(
    store: &Store,
    session: &SessionId,
    process: &ProcessIncarnation,
) -> Option<LaunchFacts> {
    match store
        .launch_facts_of(session.clone(), process.clone())
        .await
    {
        Ok(facts) => facts,
        Err(e) => {
            tracing::warn!(error = %e, session = %session, "the launch facts could not be read");
            None
        }
    }
}
