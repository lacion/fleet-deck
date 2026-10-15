//! The rotation of a binding by a resume (DL-194 §2.3, §9.12): the old
//! credentials stop working before the resume starts a provider with a new
//! one. The order is the one of a revoke: memory gate, fence entry, row
//! change with its event, fence entry removal.

use crate::coordinator::Bindings;
use crate::store::Store;

/// The text of a refused rotation.
const NOT_SAVED: &str = "The station cannot save the session credentials. Try again.";

/// Ends the `pending` and `active` rows of `binding_id` (reason
/// `rotated`) and refuses their credentials at once. No such row: nothing
/// to do.
///
/// # Errors
/// [`NOT_SAVED`] when the rows cannot be read, the fence cannot take the
/// entries (or the station has none), or the row change failed. The memory
/// gate stays; a fence entry that was written stays too (the next start
/// suspends the rows).
#[allow(dead_code, reason = "T4a calls it from the resume (DL-194)")]
pub(crate) async fn rotate_out(
    store: &Store,
    bindings: &Bindings,
    binding_id: &str,
) -> Result<(), String> {
    let rows = store
        .shim_bindings_by_binding(binding_id.to_owned())
        .await
        .map_err(|error| {
            tracing::error!(%error, "the binding rows cannot be read for a rotation");
            NOT_SAVED.to_owned()
        })?;
    if rows.is_empty() {
        return Ok(());
    }
    for row in &rows {
        bindings.update_digest(&row.digest, |b| {
            b.state = crate::coordinator::BindingState::Revoked;
        });
    }
    let entries: Vec<_> = rows
        .iter()
        .map(|r| (r.shim_id.clone(), r.digest.clone()))
        .collect();
    let Some(fence) = bindings.fence() else {
        tracing::error!("a rotation needs the binding fence and the station has none");
        return Err(NOT_SAVED.to_owned());
    };
    fence.add(&entries, "rotate").map_err(|error| {
        tracing::error!(%error, "the binding fence did not take the rotation");
        NOT_SAVED.to_owned()
    })?;
    store
        .shim_bindings_rotate_end(entries.iter().map(|(_, d)| d.clone()).collect())
        .await
        .map_err(|error| {
            tracing::error!(%error, "the rotated binding rows were not ended");
            NOT_SAVED.to_owned()
        })?;
    if let Err(error) = fence.remove(&entries) {
        tracing::warn!(%error, "the rotation fence entry was not removed");
    }
    Ok(())
}

#[cfg(test)]
#[path = "binding_rotate_tests.rs"]
mod tests;
