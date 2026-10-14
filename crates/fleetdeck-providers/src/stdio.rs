//! The stdin write queue that the Claude and Codex drivers share.
//!
//! A host command queues its frames on an unbounded channel; one writer
//! task writes them in order. [`flush`] returns when every frame queued
//! before it is in the stdin pipe. The frames that a driver writes on its
//! own (the replies to the process) are counted, and the driver stops the
//! session when too many wait (NV-19).
//!
//! It also holds the bounded stdout line reader (NV-20).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};

use crate::adapter::{ProviderError, Result};
use crate::host::HostError;

/// The longest stdout line of a provider (NV-20). It is the size of the
/// shim output buffer, so a direct host and a shim host accept the same line.
pub(crate) const MAX_LINE_BYTES: usize = crate::host::shim::output::DEFAULT_CAPACITY;

/// The most frames that wait in the stdin queue (NV-19).
pub(crate) const MAX_QUEUED_WRITES: usize = 4096;

/// The text of the error when the stdin queue is full.
const QUEUE_FULL: &str = "the process does not read stdin";

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

/// Counts one frame into the stdin queue. Call it before the send. Fails
/// when [`MAX_QUEUED_WRITES`] frames wait: the process does not read stdin.
pub(crate) fn reserve_write(queued: &AtomicUsize) -> Result<()> {
    queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_QUEUED_WRITES).then_some(n + 1)
        })
        .map(|_| ())
        .map_err(|_| write_failed(QUEUE_FULL))
}

/// Takes one frame out of the count: the send failed, or the writer took the
/// frame.
pub(crate) fn release_write(queued: &AtomicUsize) {
    let _ = queued.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        Some(n.saturating_sub(1))
    });
}

/// True for the error of [`reserve_write`].
pub(crate) fn is_queue_full(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::Host(HostError::Write(e))
            if e.get_ref().is_some_and(|inner| inner.to_string() == QUEUE_FULL)
    )
}

/// What a driver learns from one stdout line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fed {
    /// The line is handled.
    Open,
    /// The station dropped the session's events: nobody owns the session.
    SinkClosed,
    /// The stdin queue is full: the process does not read stdin (NV-19).
    QueueFull,
}

/// The result of [`read_line_bounded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineRead {
    /// One line is in the buffer, without its line end.
    Line,
    /// The input ended before any byte of a new line.
    Eof,
    /// The line is longer than the limit.
    TooLong,
}

/// Reads one line into `buf` (cleared first). The `\n` and a `\r` before it
/// are not in `buf`, like `lines()`. The limit counts the bytes before the
/// `\n`. It returns [`LineRead::TooLong`] as soon as the line is over `max`
/// bytes, and it does not read the rest (NV-20). A last line without `\n` is
/// a [`LineRead::Line`].
pub(crate) async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<LineRead> {
    buf.clear();
    let mut any = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if !any {
                return Ok(LineRead::Eof);
            }
            break;
        }
        any = true;
        let end = chunk.iter().position(|b| *b == b'\n');
        let take = end.unwrap_or(chunk.len());
        if buf.len() + take > max {
            return Ok(LineRead::TooLong);
        }
        buf.extend_from_slice(&chunk[..take]);
        reader.consume(end.map_or(take, |i| i + 1));
        if end.is_some() {
            break;
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(LineRead::Line)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read(input: &[u8], max: usize) -> (LineRead, Vec<u8>) {
        let mut reader = tokio::io::BufReader::with_capacity(4, input);
        let mut buf = Vec::new();
        let r = read_line_bounded(&mut reader, &mut buf, max).await.unwrap();
        (r, buf)
    }

    #[tokio::test]
    async fn a_line_with_a_newline_is_read_without_it() {
        assert_eq!(read(b"a\n", 16).await, (LineRead::Line, b"a".to_vec()));
    }

    #[tokio::test]
    async fn a_line_with_a_carriage_return_and_a_newline_is_read_without_both() {
        assert_eq!(read(b"a\r\n", 16).await, (LineRead::Line, b"a".to_vec()));
    }

    #[tokio::test]
    async fn a_last_line_without_a_newline_is_a_line() {
        assert_eq!(read(b"abc", 16).await, (LineRead::Line, b"abc".to_vec()));
    }

    #[tokio::test]
    async fn an_empty_input_is_the_end() {
        assert_eq!(read(b"", 16).await, (LineRead::Eof, Vec::new()));
    }

    #[tokio::test]
    async fn an_empty_line_is_a_line_not_the_end() {
        assert_eq!(read(b"\nx\n", 16).await, (LineRead::Line, Vec::new()));
    }

    #[tokio::test]
    async fn a_line_over_the_limit_without_a_newline_is_too_long() {
        let input = vec![b'x'; 9];
        assert_eq!(read(&input, 8).await.0, LineRead::TooLong);
    }

    #[tokio::test]
    async fn a_line_over_the_limit_with_a_newline_is_too_long() {
        let mut input = vec![b'x'; 9];
        input.push(b'\n');
        assert_eq!(read(&input, 8).await.0, LineRead::TooLong);
    }

    #[tokio::test]
    async fn a_line_of_exactly_the_limit_is_a_line() {
        let mut input = vec![b'x'; 8];
        assert_eq!(read(&input, 8).await, (LineRead::Line, vec![b'x'; 8]));
        input.push(b'\n');
        assert_eq!(read(&input, 8).await, (LineRead::Line, vec![b'x'; 8]));
    }

    #[tokio::test]
    async fn the_lines_after_a_line_are_read_in_order() {
        let mut reader = tokio::io::BufReader::with_capacity(4, &b"one\r\ntwo\nthree"[..]);
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        while read_line_bounded(&mut reader, &mut buf, 16).await.unwrap() == LineRead::Line {
            lines.push(buf.clone());
        }
        assert_eq!(lines, [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn the_queue_counter_stops_at_the_limit_and_frees_a_slot() {
        let queued = AtomicUsize::new(MAX_QUEUED_WRITES - 1);
        reserve_write(&queued).unwrap();
        let err = reserve_write(&queued).unwrap_err();
        assert!(is_queue_full(&err), "{err:?}");
        assert_eq!(queued.load(Ordering::Acquire), MAX_QUEUED_WRITES);
        release_write(&queued);
        reserve_write(&queued).unwrap();
    }

    #[test]
    fn a_release_on_an_empty_counter_stays_at_zero() {
        let queued = AtomicUsize::new(0);
        release_write(&queued);
        assert_eq!(queued.load(Ordering::Acquire), 0);
    }

    #[test]
    fn another_write_error_is_not_a_full_queue() {
        assert!(!is_queue_full(&write_failed("the stdin writer stopped")));
    }
}

/// A process control for the driver tests: no process, only a kill flag.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;

    use crate::host::{EndCause, ExitReport, HostError, ProcessControl, StopPolicy, Termination};

    #[derive(Debug, Default)]
    pub(crate) struct FakeControl {
        pub(crate) killed: AtomicBool,
    }

    impl FakeControl {
        fn report(&self) -> ExitReport {
            ExitReport {
                termination: Termination::Signal(9),
                cause: EndCause::Killed,
                stderr_tail: String::new(),
            }
        }

        pub(crate) fn was_killed(&self) -> bool {
            self.killed.load(Ordering::Acquire)
        }
    }

    #[async_trait]
    impl ProcessControl for FakeControl {
        fn pid(&self) -> u32 {
            0
        }

        async fn write(&self, _bytes: &[u8]) -> Result<(), HostError> {
            Ok(())
        }

        async fn close_stdin(&self) {}

        async fn stop(&self, _policy: StopPolicy) -> ExitReport {
            self.report()
        }

        async fn kill(&self) -> ExitReport {
            self.killed.store(true, Ordering::Release);
            self.report()
        }

        async fn wait(&self) -> ExitReport {
            self.report()
        }

        fn exit(&self) -> Option<ExitReport> {
            None
        }
    }
}
