//! The advance of the folder generation after a shell settlement, and the
//! start-up settlement of the shell rows (B8, DL-196).
//!
//! A settled row keeps `advance_pending = 1` until the advance ran under the
//! folder fence ([`super::turns::Fences`]). So no turn start checkpoint is
//! taken at the old generation after a settlement. [`task`] wakes on a
//! notify and at a slow tick; [`start_up`] runs once before the shell
//! admission opens.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, watch};

use super::turns::Fences;
use crate::store::{Result, Store};

/// The tick that covers a settlement made inside a store transaction (a
/// retirement), which has no notify.
const TICK: Duration = Duration::from_secs(1);

/// Advances the generation of every settled row, under the fence of its
/// root. Returns how many advanced.
///
/// # Errors
/// A database error.
pub async fn advance_pending(store: &Store, fences: &Fences) -> Result<usize> {
    let pending = store.shell_pending_advances().await?;
    let mut done = 0;
    for (effect_id, root) in pending {
        let fence = fences.for_worktree(Path::new(&root));
        let _guard = fence.lock().await;
        store.shell_advance(effect_id, root, false).await?;
        done += 1;
    }
    Ok(done)
}

/// The station start, before any command is admitted: a row whose effect
/// was retired or never claimed settles `cancelled`; every pending advance
/// runs; then the shell admission opens. A row whose effect was claimed
/// stays as it is (the execution task reclassifies it later).
///
/// # Errors
/// A database error.
pub async fn start_up(store: &Store, fences: &Fences) -> Result<()> {
    let cancelled = store.shell_cancel_unclaimed().await?;
    if cancelled > 0 {
        tracing::info!(cancelled, "settled shell commands that never started");
    }
    advance_pending(store, fences).await?;
    store.shell_open_admission().await
}

/// The advance task: runs at the start, on each notify and at each tick,
/// until the shutdown.
pub async fn task(
    store: Store,
    fences: Fences,
    notify: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if let Err(e) = advance_pending(&store, &fences).await {
            tracing::error!(error = %e, "shell advance failed");
        }
        tokio::select! {
            () = notify.notified() => {}
            () = tokio::time::sleep(TICK) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}
