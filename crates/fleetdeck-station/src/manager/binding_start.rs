//! The start order of the shim bindings (DL-194 §2.3 "Startup", §9.1, §9.7,
//! §9.8).
//!
//! 1. [`prepare`]: the fence, then the binding rows into memory. A binding
//!    named by the fence (or any binding, when the fence is unreadable)
//!    loads `Revoked` and its row becomes `suspended`; a row of an expired
//!    grant loads `Revoked` (expiry is terminal, DL-194) and ends; every
//!    other open row loads with its state and is held until its shim
//!    replayed its output.
//! 2. The binding sink is registered on the store.
//! 3. The shim recovery (`recover_station`).
//! 4. [`finish`]: the rows of shims that were not reattached are deleted,
//!    the drivers of the reattached ones get their held digests, and the
//!    fence entries that the rows settled are removed.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use super::shim_station::Restored;
use crate::coordinator::fence::{Fence, FenceState};
use crate::coordinator::{Binding, BindingState, Bindings};
use crate::store::shim_bindings::{BindingRow, MemorySink, RowState};
use crate::store::{Store, StoreError};

/// The held digests of each reattachable shim.
pub type Held = HashMap<String, Vec<String>>;

/// The binding that a row with a damaged column gets: no class, refused.
fn damaged(row: &BindingRow) -> Binding {
    Binding {
        binding_id: row.binding_id.clone(),
        grant_id: row.grant_id.clone(),
        session_id: row.session_id.clone(),
        provider_instance: row.provider_instance.clone(),
        native_id: row.native_id.clone(),
        generation: row.generation,
        classes: BTreeSet::new(),
        scope: fleetdeck_proto::command::GrantScope::Lineage,
        state: BindingState::Revoked,
        control_skill: String::new(),
    }
}

/// Steps 1 and 2: creates the fence in `run_dir`, loads the rows into
/// `bindings` and registers the binding sink on `store`. Returns the held
/// digests by shim id.
///
/// # Errors
/// A store error.
pub async fn prepare(
    store: &Store,
    bindings: &Bindings,
    run_dir: PathBuf,
) -> Result<Held, StoreError> {
    let fence = Arc::new(Fence::new(run_dir));
    fence.clean_tmp();
    bindings.set_fence(Arc::clone(&fence));
    let held = load(store, bindings, &fence, store.now_ms()).await?;
    store.set_binding_sink(Arc::new(MemorySink(bindings.clone())));
    Ok(held)
}

async fn load(
    store: &Store,
    bindings: &Bindings,
    fence: &Fence,
    now_ms: i64,
) -> Result<Held, StoreError> {
    let rows = store.shim_bindings_load().await?;
    let mut state = fence.read();
    match (&state, rows.is_empty()) {
        (FenceState::Missing, true) => {
            if let Err(error) = fence.ensure() {
                tracing::error!(%error, "the binding fence was not created; revokes will fail until it is");
            }
        }
        // A missing fence with rows counts as unreadable (§2.3).
        (FenceState::Missing, false) => state = FenceState::Unreadable,
        _ => {}
    }
    let unreadable = state == FenceState::Unreadable;
    let named = match state {
        FenceState::Entries(entries) => entries,
        _ => BTreeSet::new(),
    };
    let mut suspend_failed = false;
    let mut held = Held::new();
    for row in rows {
        if row.state == RowState::Ended {
            continue;
        }
        let digest = row.digest.clone();
        let fenced = unreadable || named.contains(&(row.shim_id.clone(), digest.clone()));
        let parsed = row.to_binding();
        if row.state == RowState::Suspended || fenced || parsed.is_err() {
            let mut binding = parsed.unwrap_or_else(|column| {
                tracing::warn!(binding = %row.binding_id, column, "a binding row does not parse; it is refused");
                damaged(&row)
            });
            binding.state = BindingState::Revoked;
            bindings.insert(digest.clone(), binding);
            if row.state != RowState::Suspended {
                match store
                    .shim_binding_set_state(digest, RowState::Suspended)
                    .await
                {
                    Ok(_) => {}
                    Err(error) => {
                        suspend_failed = true;
                        tracing::error!(binding = %row.binding_id, %error, "a binding row was not suspended");
                    }
                }
            }
            continue;
        }
        let Ok(mut binding) = parsed else {
            continue;
        };
        if row.expires_at.is_some_and(|t| t <= now_ms) {
            // Expiry is terminal: the grant never renews (DL-194).
            binding.state = BindingState::Revoked;
            bindings.insert(digest.clone(), binding);
            if let Err(error) = store.shim_binding_expire(digest).await {
                tracing::warn!(binding = %row.binding_id, %error, "an expired binding row was not ended");
            }
            continue;
        }
        bindings.insert(digest.clone(), binding);
        bindings.hold([digest.clone()]);
        held.entry(row.shim_id).or_default().push(digest);
    }
    if unreadable && !suspend_failed {
        // Every row is suspended: a rewrite of the fence loses nothing.
        if let Err(error) = fence.reset() {
            tracing::error!(%error, "the binding fence was not rewritten");
        }
    }
    Ok(held)
}

/// Step 4: see the module documentation. `restored` are the reattached
/// shims; `held` is the result of [`prepare`].
///
/// # Errors
/// A store error.
pub async fn finish(
    store: &Store,
    bindings: &Bindings,
    restored: &[Restored],
    mut held: Held,
) -> Result<(), StoreError> {
    let keep: Vec<String> = restored.iter().map(|r| r.shim_id.clone()).collect();
    drop_unattached(store, bindings, keep).await?;
    for r in restored {
        if let Some(digests) = held.remove(&r.shim_id) {
            r.driver.set_release(bindings.clone(), digests);
        }
    }
    if let Some(fence) = bindings.fence() {
        fence.prune(store).await;
    }
    Ok(())
}

/// Deletes the rows of the shims that are not in `keep`, ends their open
/// bindings in memory and releases their held digests.
///
/// # Errors
/// A store error; nothing changes.
pub async fn drop_unattached(
    store: &Store,
    bindings: &Bindings,
    keep: Vec<String>,
) -> Result<(), StoreError> {
    let dropped = store.shim_bindings_drop_others(keep).await?;
    for row in &dropped {
        bindings.update_digest(&row.digest, |b| b.state = BindingState::Revoked);
    }
    bindings.release(dropped.iter().map(|r| &r.digest));
    Ok(())
}

#[cfg(test)]
#[path = "binding_start_tests.rs"]
mod tests;
