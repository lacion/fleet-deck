//! The adapter state gap of a durable driver (ADR-005 §4.2; review
//! sa-9c1141b8 MAJOR 2; DL-122): the shim no longer holds output above the
//! committed cursor. The gap is committed at once, while the provider still
//! runs, and before any further frame; the driver then reads and
//! acknowledges without parsing and dispatches nothing.

use std::sync::atomic::Ordering;

use fleetdeck_providers::host::shim::host::Attached;

use super::{DurableCore, DurableDriver};
use crate::store::{RequestTime, StoreError};

impl<C: DurableCore> DurableDriver<C> {
    /// True after a committed adapter state gap.
    pub fn has_adapter_gap(&self) -> bool {
        self.adapter_gap.load(Ordering::SeqCst)
    }

    /// Commits the `below_floor` gap of an attachment whose replay starts
    /// above the committed cursor. `true` when there was one.
    pub(super) async fn below_floor(&self, attached: &Attached) -> Result<bool, StoreError> {
        let (from, to) = (attached.record.cursor, attached.floor);
        if from >= to {
            return Ok(false);
        }
        self.commit_adapter_gap(from, to).await?;
        Ok(true)
    }

    /// The pump's form of [`Self::below_floor`]; `false` when the commit
    /// failed or was fenced (the pump stops).
    pub(super) async fn adapter_gap_at(&self, from: u64, to: u64) -> bool {
        self.commit_adapter_gap(from, to).await.unwrap_or(false)
    }

    async fn commit_adapter_gap(&self, from: u64, to: u64) -> Result<bool, StoreError> {
        // The core lock orders the gap before any command frame.
        let _core = self.core.lock().await;
        let committed = self
            .store
            .shim_adapter_gap(self.shim_id.clone(), self.process.generation(), from, to)
            .await?;
        if committed {
            self.adapter_gap.store(true, Ordering::SeqCst);
            self.changed.send_modify(|n| *n += 1);
        }
        Ok(committed)
    }

    /// Commits and acknowledges `cursor` without events (unparsed output
    /// after an adapter state gap). `false` when fenced or failed.
    pub(super) async fn skip_to(&self, cursor: u64) -> bool {
        let core = self.core.lock().await;
        let Ok(checkpoint) = core.save() else {
            return false;
        };
        drop(core);
        matches!(
            self.commit(cursor, Vec::new(), RequestTime::Now, checkpoint, Vec::new())
                .await,
            Ok(true)
        ) && self.process.ack(cursor).await
    }
}
