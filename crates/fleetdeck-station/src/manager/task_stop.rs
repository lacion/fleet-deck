//! The bounded end of the station's background tasks at a shutdown.
use std::time::Duration;
use tokio::task::JoinHandle;

/// How long the shutdown waits for one background task.
pub(super) const TASK_STOP: Duration = Duration::from_secs(10);

/// Waits for each task at most [`TASK_STOP`], then aborts it: a task that
/// waits for a lock must not stop the shutdown.
pub(super) async fn join_or_abort(tasks: Vec<JoinHandle<()>>) {
    for mut task in tasks {
        if tokio::time::timeout(TASK_STOP, &mut task).await.is_err() {
            tracing::error!("a station task did not stop at the shutdown; aborted");
            task.abort();
        }
    }
}
