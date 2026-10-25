//! The background poll of a shim-hosted Codex session (DL-024, DL-199).
//! Codex reports no background work on its own after a turn ends (trap 4),
//! so the host lists the background terminals like the direct driver does.

use std::sync::{Arc, Weak};
use std::time::Duration;

use fleetdeck_providers::adapter::ProviderError;
use fleetdeck_providers::codex::CodexCore;

use super::durable::DriverError;
use super::shim_codex::{DurableCodex, split};

/// The tick of the poll (DL-024).
pub const BACKGROUND_POLL: Duration = Duration::from_secs(5);

/// Starts the poll task of `driver`: one list at once (it also reconciles
/// background work after a restore), then a list on a tick when work is
/// open or a turn ended since the last tick. The task holds a [`Weak`]
/// and ends with the driver or with the provider (DL-024, DL-199).
pub fn spawn_background_poll(driver: &Arc<DurableCodex>, every: Duration) {
    tokio::spawn(poll(Arc::downgrade(driver), every));
}

/// True while the poll goes on.
async fn list(driver: &DurableCodex) -> bool {
    match driver.internal(|c| c.list_background().map(split)).await {
        Ok(()) => true,
        Err(DriverError::Provider(ProviderError::SessionEnded)) => false,
        Err(e) => {
            // The next tick tries again.
            tracing::debug!(error = %e, "the background list was not sent");
            true
        }
    }
}

async fn poll(weak: Weak<DurableCodex>, every: Duration) {
    let mut seen = {
        let Some(driver) = weak.upgrade() else {
            return;
        };
        let seen = driver.with_core(CodexCore::turns_ended).await;
        if !list(&driver).await {
            return;
        }
        seen
    };
    loop {
        tokio::time::sleep(every).await;
        // No strong reference is held while the task sleeps.
        let Some(driver) = weak.upgrade() else {
            return;
        };
        let (ended, open, turns) = driver
            .with_core(|c| {
                (
                    c.has_ended(),
                    !c.background().is_empty() || !c.background_tools().is_empty(),
                    c.turns_ended(),
                )
            })
            .await;
        if ended {
            return;
        }
        if (open || turns != seen) && !list(&driver).await {
            return;
        }
        seen = turns;
    }
}
