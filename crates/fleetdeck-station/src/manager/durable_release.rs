//! The replay hold of a reattached shim (DL-194 §9.7): the bindings of the
//! shim answer "Fleet Deck is starting" until the driver has committed the
//! output that the shim had buffered at the attach.
//!
//! The start order gives the driver its held digests ([`DurableDriver::set_release`]).
//! The driver asks the shim for a `status` frame and releases the digests
//! when the host reported a `status_end` and the committed offset reached
//! it. The check runs after each commit and in a small poller. An exit
//! commit releases at once (its transition already revoked the bindings).
//! There is no timeout: a driver that never catches up keeps its bindings
//! held and logs once at `warn` after 60 s.

use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{DurableCore, DurableDriver};
use crate::coordinator::Bindings;

/// How often the poller checks.
const POLL: Duration = Duration::from_millis(25);
/// The wait after which the poller warns once.
const WARN_AFTER: Duration = Duration::from_secs(60);

/// The digests that a driver releases, and the bindings that hold them.
pub(super) struct Held {
    bindings: Bindings,
    digests: Vec<String>,
}

impl<C: DurableCore> DurableDriver<C> {
    /// Gives the driver the held digests of its shim. It releases them in
    /// `bindings` when it is caught up (see the module documentation).
    pub fn set_release(self: &Arc<Self>, bindings: Bindings, digests: Vec<String>) {
        if digests.is_empty() {
            return;
        }
        *self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Held { bindings, digests });
        if self.exit_committed.load(Ordering::SeqCst) {
            self.release_now();
            return;
        }
        self.release_if_caught_up();
        let weak = Arc::downgrade(self);
        tokio::spawn(poll(weak));
    }

    /// Records the offset of a commit, then releases when the commit was
    /// the exit or when the driver is caught up.
    pub(super) fn note_committed(&self, cursor: u64, exited: bool) {
        self.committed.fetch_max(cursor, Ordering::SeqCst);
        if exited {
            self.exit_committed.store(true, Ordering::SeqCst);
            self.release_now();
        } else {
            self.release_if_caught_up();
        }
    }

    fn released(&self) -> bool {
        self.release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
    }

    fn release_now(&self) {
        let held = self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(Held { bindings, digests }) = held {
            bindings.release(&digests);
        }
    }

    fn release_if_caught_up(&self) {
        if self.released() {
            return;
        }
        if let Some(end) = self.process.status_end()
            && self.committed.load(Ordering::SeqCst) >= end
        {
            self.release_now();
        }
    }
}

/// Asks the shim for its status once, then checks every [`POLL`] until the
/// digests are released or the driver is gone.
async fn poll<C: DurableCore>(weak: Weak<DurableDriver<C>>) {
    let asked = match weak.upgrade() {
        Some(driver) => driver.process.request_status().await,
        None => return,
    };
    if !asked {
        tracing::debug!("the shim status request was not sent");
    }
    let started = tokio::time::Instant::now();
    let mut warned = false;
    loop {
        {
            let Some(driver) = weak.upgrade() else {
                return;
            };
            driver.release_if_caught_up();
            if driver.released() {
                return;
            }
            if !warned && started.elapsed() >= WARN_AFTER {
                warned = true;
                tracing::warn!(shim = %driver.shim_id, "the replay of a reattached shim has not finished after 60 s");
            }
        }
        tokio::time::sleep(POLL).await;
    }
}
