//! The watch task of the MCP bridge (MCP P4b §8.3).
//!
//! After the bridge wrote the first successful answer to `server/discover`
//! or `initialize`, one task keeps a `WATCH` connection to the station
//! socket. Each line that is exactly the `notifications/tools/list_changed`
//! notification becomes at most one pending notification, which a writer
//! task writes to stdout through `emit`. The station's first line after
//! every `OK watch.v1` is the refresh, so the bridge never writes a
//! notification on its own.
//!
//! `ERR unauthorized` and `ERR bad_request` (an older station) stop the task
//! for good. `ERR starting`, `ERR limited`, a timeout and a connection error
//! retry with a backoff of 1 s that doubles to 30 s, plus a jitter of 0 to
//! 25 %. The token goes only to the socket.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::bridge::emit;
use super::socket::{Line, read_bounded};
use super::socket_watch::{LIST_CHANGED_LINE, OK_WATCH};

/// The longest line that the bridge takes from a watch connection.
const LINE_MAX: usize = 256;
/// The wait for the first line of the station.
const FIRST_LINE: Duration = Duration::from_secs(10);
/// The first and the longest wait between two attempts.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A connection that was established this long resets the backoff.
const STABLE: Duration = Duration::from_secs(30);

/// The watch of one bridge: it starts once and stops with the bridge.
#[derive(Default)]
pub(super) struct Watch {
    started: AtomicBool,
    task: StdMutex<Option<JoinHandle<()>>>,
}

impl Watch {
    /// Called after the bridge wrote `answer` for `msg`: starts the task for
    /// the first successful answer to `server/discover` or `initialize`.
    pub(super) fn after_answer<W>(
        &self,
        msg: &Value,
        answer: &[u8],
        socket: &Arc<Path>,
        token: &Arc<str>,
        out: &Arc<Mutex<W>>,
    ) where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let method = msg.get("method").and_then(Value::as_str);
        if !matches!(method, Some("server/discover" | "initialize")) {
            return;
        }
        let answered = serde_json::from_slice::<Value>(answer)
            .ok()
            .is_some_and(|value| value.get("result").is_some());
        if !answered || self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let task = tokio::spawn(run(socket.clone(), token.clone(), out.clone()));
        *self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }

    /// Stops the task (stdin closed). A later answer starts nothing.
    pub(super) fn stop(&self) {
        self.started.store(true, Ordering::SeqCst);
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

/// Aborts its task when dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How one connection attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// Stop for good.
    Stop,
    /// Retry with the backoff; the connection was never established.
    Failed,
    /// Retry with the backoff; the connection was established for this long.
    Ended(Duration),
}

/// The task: connect, watch, retry, until stopped for good or aborted.
async fn run<W>(socket: Arc<Path>, token: Arc<str>, out: Arc<Mutex<W>>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let pending = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(Notify::new());
    // The writer: at most one notification is pending; a slow stdout does
    // not stop the reading of the watch.
    let writer = {
        let (pending, wake) = (pending.clone(), wake.clone());
        tokio::spawn(async move {
            loop {
                wake.notified().await;
                if pending.swap(false, Ordering::SeqCst) {
                    emit(&out, LIST_CHANGED_LINE.as_bytes()).await;
                }
            }
        })
    };
    let _writer = AbortOnDrop(writer);
    let mut backoff = Backoff::default();
    loop {
        let ended = attempt(&socket, &token, &pending, &wake).await;
        if ended == Attempt::Stop {
            return;
        }
        tokio::time::sleep(with_jitter(backoff.after(ended))).await;
    }
}

/// The wait between two attempts: 1 s, doubling to 30 s.
struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: BACKOFF_MIN }
    }
}

impl Backoff {
    /// The wait (without jitter) after an attempt that ended like `ended`.
    /// A connection that was established for at least [`STABLE`] resets the
    /// backoff to 1 s first.
    fn after(&mut self, ended: Attempt) -> Duration {
        if matches!(ended, Attempt::Ended(lasted) if lasted >= STABLE) {
            self.next = BACKOFF_MIN;
        }
        let wait = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        wait
    }
}

/// `base` plus a jitter of 0 to 25 %, from the nanoseconds of the clock.
fn with_jitter(base: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    base + base * (nanos % 251) / 1000
}

/// One connection: `WATCH`, the first line, then the lines of the station.
async fn attempt(socket: &Path, token: &str, pending: &AtomicBool, wake: &Notify) -> Attempt {
    let stream = match UnixStream::connect(socket).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(kind = ?error.kind(), "the bridge watch could not connect");
            return Attempt::Failed;
        }
    };
    let (read, mut write) = stream.into_split();
    let mut request = Vec::with_capacity(token.len() + 8);
    request.extend_from_slice(b"WATCH ");
    request.extend_from_slice(token.as_bytes());
    request.push(b'\n');
    if let Err(error) = write.write_all(&request).await {
        tracing::debug!(kind = ?error.kind(), "the bridge watch could not write");
        return Attempt::Failed;
    }
    let mut reader = BufReader::new(read);
    let first = tokio::time::timeout(FIRST_LINE, read_bounded(&mut reader, LINE_MAX)).await;
    match first {
        Err(_) | Ok(Err(_)) | Ok(Ok(Line::Eof | Line::Partial(_))) => return Attempt::Failed,
        Ok(Ok(Line::TooLong)) => return Attempt::Stop,
        Ok(Ok(Line::Text(text))) => match text.as_slice() {
            line if line == OK_WATCH.as_bytes() => {}
            b"ERR starting" | b"ERR limited" => return Attempt::Failed,
            // `ERR unauthorized`, `ERR bad_request` (an older station) and
            // any other first line.
            _ => return Attempt::Stop,
        },
    }
    let since = Instant::now();
    loop {
        match read_bounded(&mut reader, LINE_MAX).await {
            Ok(Line::Text(text)) if text == LIST_CHANGED_LINE.as_bytes() => {
                pending.store(true, Ordering::SeqCst);
                wake.notify_one();
            }
            // A foreign or an oversize line, the end of the stream or an error
            // closes the connection.
            Ok(Line::Text(_) | Line::TooLong | Line::Eof | Line::Partial(_)) | Err(_) => break,
        }
    }
    Attempt::Ended(since.elapsed())
}

#[cfg(test)]
#[path = "bridge_watch_tests.rs"]
mod tests;
