//! The target side of a move (FG-9, design §12.3). This step has the per-move
//! lock; the launch admission, the commit, the discard and the undo handlers
//! come in later steps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// The per-move locks of the target (design §12.3, "The per-move lock and
/// the tombstone", revision 4, B4): one `tokio::sync::Mutex<()>` for each
/// `move_id`, created at its first use. Three parties take it: the launch
/// admission of the `MoveIn` effect, the commit transaction of the effect, and
/// the discard handler.
// (FG-9: T4.2c and T4.2d use it)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Default)]
pub struct MoveLocks {
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl MoveLocks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The lock of `move_id`; the same lock for the same id.
    fn of(&self, move_id: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(move_id.to_owned()).or_default())
    }

    /// Waits for the lock of `move_id` and holds it until the guard drops.
    pub async fn lock(&self, move_id: &str) -> OwnedMutexGuard<()> {
        self.of(move_id).lock_owned().await
    }

    /// The lock of `move_id` when it is free now; `None` while a party holds
    /// it.
    #[must_use]
    pub fn try_lock(&self, move_id: &str) -> Option<OwnedMutexGuard<()>> {
        self.of(move_id).try_lock_owned().ok()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::MoveLocks;

    #[tokio::test]
    async fn one_move_id_has_one_lock_and_other_ids_are_independent() {
        let locks = Arc::new(MoveLocks::new());
        let held = locks.lock("m1").await;
        assert!(locks.try_lock("m1").is_none(), "the same id is held");
        assert!(locks.try_lock("m2").is_some(), "another id is free");
        // A waiter on the same id gets the lock only after the holder drops.
        let waiter = {
            let locks = Arc::clone(&locks);
            tokio::spawn(async move {
                let _guard = locks.lock("m1").await;
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "the waiter waits for the holder");
        drop(held);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the waiter takes the lock after the drop")
            .unwrap();
        assert!(locks.try_lock("m1").is_some());
    }
}
