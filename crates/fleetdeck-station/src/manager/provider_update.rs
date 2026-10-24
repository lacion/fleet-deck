//! The update hold and the durable quarantine of provider CLIs (B28, design
//! `cli-update.md` §3.7.1 and §3.8.1).
//!
//! While an update of one provider kind holds, a queued start or resume of
//! every provider waits (the effect worker does not claim it), and the
//! restart admission is refused. The hold is short: the download and the
//! staging run before it.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;

use super::SessionManager;
use super::restart_gate::RestartState;
use super::target::SessionTarget;
use crate::store::{Result, Store};

/// The longest wait of [`SessionManager::begin_provider_update`] for the
/// starts in flight.
const SETTLE: Duration = Duration::from_secs(10);

/// The hold of one update. The drop ends it and wakes the effect worker.
pub struct UpdateHold {
    restart: Arc<RestartState>,
    store: Store,
}

impl Drop for UpdateHold {
    fn drop(&mut self) {
        self.restart.end_update();
        self.store.nudge_worker();
    }
}

impl SessionManager {
    /// The sessions with a live process whose provider is of `kind`. A live
    /// session whose provider cannot be read is listed (the safe side).
    pub async fn live_sessions_of(&self, kind: ProviderKind) -> Vec<SessionId> {
        let mut out = Vec::new();
        for (id, _) in self.target.live_pids() {
            let provider = match self.store.summary(id.clone()).await {
                Ok(Some(summary)) => summary.provider.or_else(|| {
                    let instance = summary.provider_instance?;
                    self.policy.current().instance(&instance)
                }),
                _ => None,
            };
            if provider.is_none_or(|p| p == kind) {
                out.push(id);
            }
        }
        out
    }

    /// Starts the hold of an update of `kind` (§3.7.1), then waits at most
    /// ten seconds for the starts in flight.
    ///
    /// # Errors
    /// The reason: `station_restarting`, `another_update` or
    /// `starts_in_flight`. Nothing stays held after an error.
    pub async fn begin_provider_update(
        &self,
        kind: ProviderKind,
    ) -> std::result::Result<UpdateHold, &'static str> {
        self.begin_provider_update_within(kind, SETTLE).await
    }

    pub(super) async fn begin_provider_update_within(
        &self,
        kind: ProviderKind,
        settle: Duration,
    ) -> std::result::Result<UpdateHold, &'static str> {
        if !self.restart.begin_update(kind) {
            return Err(if self.restart.restarting() {
                "station_restarting"
            } else {
                "another_update"
            });
        }
        let hold = UpdateHold {
            restart: Arc::clone(&self.restart),
            store: self.store.clone(),
        };
        if !self.settle_starts(settle).await {
            return Err("starts_in_flight");
        }
        Ok(hold)
    }
}

/// Marks every provider with a quarantine row as quarantined, with the text
/// of the row. It runs before the effect worker registers, so no queued
/// start can dispatch first (§3.8.1).
///
/// # Errors
/// A store error.
pub(super) async fn load_quarantine(store: &Store, target: &SessionTarget) -> Result<()> {
    for row in store.cli_update_quarantines().await? {
        target.quarantine(row.provider, &row.text);
    }
    Ok(())
}

#[cfg(test)]
#[path = "provider_update_tests.rs"]
mod tests;
