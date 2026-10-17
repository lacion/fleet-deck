//! Settles the turn checkpoint rows that a crash left open (moved from
//! `manager/mod.rs` for the module size rule, T4.2c; no change in behavior).

use crate::store::{Result, Store};

/// Settles the turn checkpoint rows that a crash left open (DL-082).
pub(super) async fn settle_turn_rows(store: &Store) -> Result<()> {
    let settled =
        crate::workspace::settle_unsettled(store, crate::workspace::GitOptions::isolated()).await?;
    if !settled.finalized.is_empty() || !settled.abandoned.is_empty() {
        tracing::info!(
            finalized = settled.finalized.len(),
            abandoned = settled.abandoned.len(),
            "settled turn checkpoints after restart"
        );
    }
    Ok(())
}
