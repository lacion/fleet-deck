//! The background poll task of the Codex driver (DL-024).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use super::Shared;
use crate::stdio::Write;

/// The background poll (DL-024): one `thread/backgroundTerminals/list`
/// after each wake, then one every `every` while work is open.
pub(super) async fn poll_loop(
    shared: Arc<Shared>,
    writes: mpsc::UnboundedSender<Write>,
    every: Duration,
) {
    loop {
        shared.poll.notified().await;
        loop {
            {
                let mut core = shared.core.lock().await;
                if core.has_ended() {
                    return;
                }
                let Ok(out) = core.list_background() else {
                    break;
                };
                if shared.dispatch(out, None, &writes).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(every).await;
            let core = shared.core.lock().await;
            if core.has_ended() {
                return;
            }
            if core.background().is_empty() && core.background_tools().is_empty() {
                break;
            }
        }
    }
}

// In this file for the module size rule (`driver.rs`); no behavior change.
impl Drop for super::CodexSession {
    /// The tasks hold the process control: abort them so that the last
    /// reference drops and the host kills the group.
    fn drop(&mut self) {
        for task in [self.reader.take(), self.writer.take(), self.poller.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}
