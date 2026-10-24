//! The file system tasks of an installer (pre-PR review finding 2).
//!
//! A time limit drops the await of a `spawn_blocking` task, not the task: a
//! rename can still run after the limit. Each task holds the guard of
//! [`Mutations`] until it ends, so [`Mutations::settle`] tells when no task
//! of the installer runs any more. The attempt settles before its way back,
//! its clean-up and the release of the hold.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinError;

/// The file system tasks of one installer.
#[derive(Debug, Default)]
pub struct Mutations {
    lock: Arc<Mutex<()>>,
}

impl Mutations {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` in `spawn_blocking`. The task holds the guard until `f`
    /// returns, also when the caller stops waiting for it.
    ///
    /// # Errors
    /// The join error of the task.
    pub async fn run<T, F>(&self, f: F) -> Result<T, JoinError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let guard = Arc::clone(&self.lock).lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            f()
        })
        .await
    }

    /// Waits until no task of [`Mutations::run`] runs. `false` when a task
    /// still runs after `limit`.
    pub async fn settle(&self, limit: Duration) -> bool {
        tokio::time::timeout(limit, self.lock.lock()).await.is_ok()
    }
}

#[cfg(test)]
#[path = "mutations_tests.rs"]
mod tests;
