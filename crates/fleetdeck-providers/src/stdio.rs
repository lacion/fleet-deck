//! The stdin write queue that the Claude and Codex drivers share.
//!
//! A host command queues its frames on an unbounded channel; one writer
//! task writes them in order. [`flush`] returns when every frame queued
//! before it is in the stdin pipe.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::adapter::{ProviderError, Result};
use crate::host::HostError;

/// One item of the write queue.
#[derive(Debug)]
pub(crate) enum Write {
    Frame(Value),
    /// Close stdin after the frames before it (a graceful stop).
    Close,
    /// Acknowledges that every frame before it is written to stdin.
    Flush(oneshot::Sender<()>),
}

/// Milliseconds since the Unix epoch (raw-frame log time stamps).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Returns when every frame queued before it is in the stdin pipe, so a
/// host command returns after its write, not before it (PLAN §6.4: the
/// station records `sent` only after the I/O). Call it only after frames
/// were queued: every failure then can follow a partial write, so it is a
/// host write error (`outcome_unknown`), never "nothing was sent".
pub(crate) async fn flush(writes: &mpsc::UnboundedSender<Write>, limit: Duration) -> Result<()> {
    let (ack, done) = oneshot::channel();
    if writes.send(Write::Flush(ack)).is_err() {
        // The writer ended after it took (some of) the frames.
        return Err(write_failed("the stdin writer stopped"));
    }
    match tokio::time::timeout(limit, done).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(write_failed("the stdin writer stopped")),
        Err(_) => Err(write_failed("the process does not read stdin")),
    }
}

/// A host write error: the outcome of the command is unknown.
pub(crate) fn write_failed(why: &str) -> ProviderError {
    ProviderError::Host(HostError::Write(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        why,
    )))
}
