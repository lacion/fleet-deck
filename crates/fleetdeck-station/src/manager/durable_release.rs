//! The replay hold of a reattached shim (DL-194 §9.7): the bindings of the
//! shim answer "Fleet Deck is starting" until the driver has committed the
//! output that the shim had buffered at the attach.
//!
//! The start order gives the driver its held digests ([`DurableDriver::set_release`]).
//! The driver asks the shim for a `status` frame and releases the digests
//! when the host reported a `status_end` and the driver received that far;
//! it then fixes the replay boundary, the end of the last complete line
//! (the driver commits whole lines only, so a partial line at the end of
//! the buffer is not waited for), and releases when the committed offset
//! reaches the boundary. The check runs after each commit and in a small
//! poller. An exit commit releases at once (its transition already revoked
//! the bindings).
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
    /// The replay boundary, fixed once when the received output reached
    /// the status end; `None` before (DL-194).
    boundary: Option<u64>,
}

/// The replay boundary check (DL-194 §9.7). It fixes `boundary` once,
/// when the status end is known and the received output reached it, then
/// is true when `committed` reached the boundary.
fn caught_up(
    boundary: &mut Option<u64>,
    status_end: Option<u64>,
    delivered: u64,
    line_end: impl FnOnce() -> u64,
    committed: u64,
) -> bool {
    if boundary.is_none()
        && let Some(end) = status_end
        && delivered >= end
    {
        *boundary = Some(line_end());
    }
    boundary.is_some_and(|b| committed >= b)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Held {
            bindings,
            digests,
            boundary: None,
        });
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
        if let Some(Held {
            bindings, digests, ..
        }) = held
        {
            bindings.release(&digests);
        }
    }

    fn release_if_caught_up(&self) {
        let due = {
            let mut guard = self
                .release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(held) = guard.as_mut() else {
                return;
            };
            caught_up(
                &mut held.boundary,
                self.process.status_end(),
                self.process.delivered(),
                || self.process.line_end(),
                self.committed.load(Ordering::SeqCst),
            )
        };
        if due {
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

#[cfg(test)]
mod tests {
    use super::caught_up;

    #[test]
    fn nothing_is_released_before_the_status_end_and_the_receipt_of_it() {
        let mut b = None;
        assert!(!caught_up(&mut b, None, 100, || 100, 100));
        assert!(!caught_up(&mut b, Some(30), 20, || 12, 100));
        assert_eq!(b, None);
    }

    #[test]
    fn a_partial_line_at_the_end_of_the_buffer_is_not_waited_for() {
        // "line1\n" "line2\n" "par": the buffer ends at 15, the last
        // complete line at 12.
        let mut b = None;
        assert!(!caught_up(&mut b, Some(15), 15, || 12, 6));
        assert_eq!(b, Some(12));
        assert!(caught_up(&mut b, Some(15), 15, || 12, 12));
    }

    #[test]
    fn the_boundary_is_fixed_once() {
        let mut b = None;
        assert!(!caught_up(&mut b, Some(15), 15, || 12, 0));
        // More output arrives later: the boundary does not move.
        assert!(!caught_up(&mut b, Some(15), 40, || 36, 11));
        assert_eq!(b, Some(12));
        assert!(caught_up(&mut b, Some(15), 40, || 36, 12));
    }
}
