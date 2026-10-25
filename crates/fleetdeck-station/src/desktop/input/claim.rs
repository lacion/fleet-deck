//! The durable claim of an input operation on the target (R13 P7 §6.3, §11):
//! the `desktop_ops` row of the operation, written before the disclosure, the
//! focus and the injection. A restart or a reconnect never runs a claimed
//! operation again, and a changed payload under the same operation id has
//! another action hash and is refused.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use serde_json::{Value, json};

use super::{Claimed, Claimer};
use crate::desktop::coordinator::p6_fail;
use crate::desktop::{Fail, code};
use crate::store::Store;
use crate::store::desktop_ops::Claim;

/// The text of `failed` when the claim cannot be written.
const CLAIM_FAILED: &str = "The station could not record the desktop action.";

/// The claim of one operation in the station store.
pub struct StoreClaim<'a> {
    store: &'a Store,
    deck_fp: &'a str,
    operation_id: &'a str,
    action_hash: &'a str,
    kind: &'a str,
    ctx: &'a CoordinatorContext,
    created: AtomicBool,
}

impl<'a> StoreClaim<'a> {
    /// The claim of operation `operation_id` of kind `kind` (one of the three
    /// input claim kinds) for the deck `deck_fp`.
    #[must_use]
    pub fn new(
        store: &'a Store,
        deck_fp: &'a str,
        operation_id: &'a str,
        action_hash: &'a str,
        kind: &'a str,
        ctx: &'a CoordinatorContext,
    ) -> Self {
        Self {
            store,
            deck_fp,
            operation_id,
            action_hash,
            kind,
            ctx,
            created: AtomicBool::new(false),
        }
    }

    /// Whether this call wrote the claim. Only that call may end it: a row
    /// that was seen before belongs to an earlier run.
    #[must_use]
    pub fn created(&self) -> bool {
        self.created.load(Ordering::Acquire)
    }

    /// Ends the claim this call wrote: `done` with an empty result, or
    /// `failed` with the fixed code. A failure to write is logged and the
    /// row stays `claimed`, so a repeat gives `authority_unknown`.
    pub async fn finish(&self, outcome: Result<(), &str>) {
        if !self.created() {
            return;
        }
        let stored: Result<Option<Value>, &str> = outcome.map(|()| Some(json!({})));
        if let Err(e) = self
            .store
            .desktop_op_finish(self.deck_fp, self.operation_id, stored, self.store.now_ms())
            .await
        {
            tracing::warn!(error = %e, "an input claim cannot be finished");
        }
    }
}

#[async_trait]
impl Claimer for StoreClaim<'_> {
    async fn claim(&self) -> Result<Claimed, Fail> {
        let claimed = self
            .store
            .desktop_op_claim(
                self.deck_fp,
                self.operation_id,
                self.action_hash,
                self.kind,
                self.ctx,
                self.store.now_ms(),
            )
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "an input claim cannot be written");
                Fail::new(code::FAILED, CLAIM_FAILED)
            })?;
        match claimed {
            Claim::New => {
                self.created.store(true, Ordering::Release);
                Ok(Claimed::New)
            }
            Claim::Done(_) => Ok(Claimed::Replay),
            Claim::Seen => Err(p6_fail(p6_code::AUTHORITY_UNKNOWN)),
        }
    }
}

#[cfg(test)]
#[path = "claim_tests.rs"]
mod tests;
