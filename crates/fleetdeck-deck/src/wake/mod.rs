//! The wake rule of the wall (R13 §6.6, §6D items 3 to 5 and 9, §5E item 5
//! and 6, §5F item 4, §5G item 2).
//!
//! STEP 1 STUB. [`spawn`] is the task that `serve.rs` starts. It only waits
//! for the shutdown signal. The P3b wake task replaces the body: request
//! detection on `watch_board()` with the baseline of
//! [`crate::store::DeckStore::snapshot_mark`], `wake_reserve` before every
//! wake (only `Reserved` wakes), quiet hours, the cooldown of 30 s for each
//! bound monitor, and at most four workers with a bounded queue that call
//! [`crate::desktop_relay::act`] with
//! [`crate::desktop_relay::Actor::Wake`]. The signature stays.

use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::link::DeckLink;
use crate::live::LiveCell;
use crate::store::DeckStore;

/// Starts the wake task. It stops when `shutdown` turns true.
pub fn spawn(
    _store: DeckStore,
    _link: Arc<DeckLink>,
    _live: LiveCell,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while !*shutdown.borrow() {
            if shutdown.changed().await.is_err() {
                return;
            }
        }
    })
}
