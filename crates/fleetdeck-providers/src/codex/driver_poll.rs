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
