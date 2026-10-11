//! The effect worker (PLAN §6.4, ADR-001 §3; M2 P2-5).
//!
//! One worker generation runs per station. [`EffectWorker::register`] takes
//! a new fence token; claims of the generation before it become
//! `outcome_unknown` (their I/O may have happened). For each effect the
//! worker:
//!
//! 1. commits the claim with its fence token and the effect's target process
//!    incarnation (in the row since enqueue) — **before** any I/O;
//! 2. calls [`EffectTarget::dispatch`], the only place with provider I/O;
//! 3. stores the [`Dispatch`] result. `Sent` keeps the claim until provider
//!    evidence confirms it.
//!
//! A worker that a newer generation replaced gets
//! [`StoreError::Fenced`] at its next claim or settle and stops. A crash
//! anywhere in 1–3 never sends an effect twice: a claimed effect never goes
//! back to `queued`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::watch;

use crate::store::{Dispatch, Effect, Result, Store, StoreError};

/// The provider side of the worker: the session manager (P2-6), or a fake in
/// tests.
#[async_trait]
pub trait EffectTarget: Send + Sync {
    /// Does the I/O of one claimed effect. It must check the effect's
    /// `process_incarnation` against the live process and return
    /// [`Dispatch::NotDispatched`] when they differ (the process that the
    /// command was for is gone).
    async fn dispatch(&self, effect: &Effect) -> Dispatch;
}

/// One worker generation.
pub struct EffectWorker {
    store: Store,
    target: Arc<dyn EffectTarget>,
    fence: u64,
}

impl std::fmt::Debug for EffectWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectWorker")
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

impl EffectWorker {
    /// Starts a new worker generation.
    ///
    /// # Errors
    /// A database error.
    pub async fn register(store: Store, target: Arc<dyn EffectTarget>) -> Result<Self> {
        let fence = store.register_worker().await?;
        Ok(Self {
            store,
            target,
            fence,
        })
    }

    #[must_use]
    pub fn fence(&self) -> u64 {
        self.fence
    }

    /// Claims, dispatches and settles one effect. `None` when the outbox is
    /// empty.
    ///
    /// # Errors
    /// [`StoreError::Fenced`] or a database error.
    pub async fn run_once(&self) -> Result<Option<Effect>> {
        let Some(effect) = self.store.claim_next(self.fence).await? else {
            return Ok(None);
        };
        let kind = effect.payload.kind();
        crate::fault::hit(crate::fault::FaultPoint::AfterClaim, kind);
        let dispatch = self.target.dispatch(&effect).await;
        crate::fault::hit(crate::fault::FaultPoint::AfterSend, kind);
        let settled = self
            .store
            .settle(effect.effect_id.clone(), self.fence, dispatch)
            .await?;
        Ok(Some(settled.unwrap_or(effect)))
    }

    /// Runs effects until the outbox is empty; returns how many ran.
    ///
    /// # Errors
    /// See [`Self::run_once`].
    pub async fn drain(&self) -> Result<usize> {
        let mut n = 0;
        while self.run_once().await?.is_some() {
            n += 1;
        }
        Ok(n)
    }

    /// Runs until `shutdown` turns true, the store closes, or a newer worker
    /// replaces this one (then it returns `Ok`).
    ///
    /// # Errors
    /// A database error.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut wake = self.store.subscribe_outbox();
        loop {
            wake.mark_unchanged();
            match self.drain().await {
                Ok(_) => {}
                Err(StoreError::Fenced { fence, current }) => {
                    tracing::info!(fence, current, "a newer effect worker took over");
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                changed = wake.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
