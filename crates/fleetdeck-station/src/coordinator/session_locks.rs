//! The lock of one session that orders a process start with a live grant
//! change (MCP P4b PRE-PR R1): the start holds it from its record read until
//! its binding is installed; `session.coordinator_set` and
//! `session.coordinator_renew` hold it from before the decision until after
//! its publish.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fleetdeck_proto::SessionId;
use tokio::sync::OwnedMutexGuard;

/// One async lock for each session. An entry is created on first use and is
/// kept.
#[derive(Clone, Default)]
pub(crate) struct SessionLocks(Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>);

impl SessionLocks {
    /// Waits for the lock of `session` and holds it until the guard drops.
    pub(crate) async fn lock(&self, session: &SessionId) -> OwnedMutexGuard<()> {
        let entry = {
            let mut map = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(map.entry(session.clone()).or_default())
        };
        entry.lock_owned().await
    }
}
