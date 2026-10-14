//! The write acknowledgement of a host command (PLAN §6.4: `sent` only
//! after the I/O; a write that can be partial is an unknown outcome).

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Mutex, mpsc, watch};

use super::{ClaudeCore, Output, Shared, Write, flush, read_loop};
use crate::adapter::{EventSink, ProviderError};
use crate::claude::tests::config;
use crate::host::HostError;
use crate::stdio::test_support::FakeControl;
use crate::stdio::{MAX_QUEUED_WRITES, is_queue_full};

const LIMIT: Duration = Duration::from_secs(5);

fn is_write_error(r: &Result<(), ProviderError>) -> bool {
    matches!(r, Err(ProviderError::Host(HostError::Write(_))))
}

#[tokio::test]
async fn the_flush_returns_after_the_writer_took_every_frame_before_it() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    let writer = tokio::spawn(async move {
        let mut frames = 0;
        while let Some(item) = rx.recv().await {
            match item {
                Write::Frame(_) => frames += 1,
                Write::Flush(ack) => {
                    ack.send(()).unwrap();
                    return frames;
                }
                Write::Close => return frames,
            }
        }
        frames
    });
    flush(&tx, LIMIT).await.unwrap();
    assert_eq!(writer.await.unwrap(), 1);
}

#[tokio::test]
async fn a_writer_that_ends_after_it_took_the_frames_is_a_partial_write() {
    // The frames were queued; then the writer ended (for example, a write
    // error after a part of a line) before the acknowledgement could be
    // queued. Some bytes may be in the pipe: the outcome is unknown, never
    // "nothing was sent".
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    assert!(matches!(rx.recv().await, Some(Write::Frame(_))));
    drop(rx);
    let r = flush(&tx, LIMIT).await;
    assert!(is_write_error(&r), "{r:?}");
}

#[tokio::test]
async fn a_writer_that_drops_the_acknowledgement_is_a_partial_write() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    let writer = tokio::spawn(async move {
        // Takes the frame, fails on it, and ends: the pending
        // acknowledgement is dropped with the queue.
        let _ = rx.recv().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(rx);
    });
    let r = flush(&tx, LIMIT).await;
    assert!(is_write_error(&r), "{r:?}");
    writer.await.unwrap();
}

#[tokio::test]
async fn a_process_that_does_not_read_stdin_makes_the_flush_time_out() {
    let (tx, _rx) = mpsc::unbounded_channel::<Write>();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    let r = flush(&tx, Duration::from_millis(50)).await;
    assert!(is_write_error(&r), "{r:?}");
}

fn shared(queued: usize) -> (Arc<Shared>, mpsc::Receiver<crate::adapter::Emitted>) {
    let (sink, events) = EventSink::channel(64);
    let shared = Arc::new(Shared {
        core: Mutex::new(ClaudeCore::new(config(false))),
        sink,
        raw: std::sync::Mutex::new(None),
        queued: AtomicUsize::new(queued),
    });
    (shared, events)
}

/// Runs the read loop on `stdout` with the line limit `max_line`. Returns the
/// control and the number of lines that the loop counted.
async fn run_read_loop(
    shared: &Arc<Shared>,
    stdout: Vec<u8>,
    max_line: usize,
) -> (Arc<FakeControl>, u64) {
    let control = Arc::new(FakeControl::default());
    let (writes, _rx) = mpsc::unbounded_channel::<Write>();
    let (frames_tx, frames_rx) = watch::channel(0);
    tokio::time::timeout(
        Duration::from_secs(5),
        read_loop(
            Arc::clone(shared),
            control.clone(),
            Box::new(Cursor::new(stdout)),
            writes,
            frames_tx,
            max_line,
        ),
    )
    .await
    .unwrap();
    let seen = *frames_rx.borrow();
    (control, seen)
}

#[tokio::test]
async fn a_stdout_line_over_the_limit_stops_the_session() {
    // NV-20: the line is over the limit; the next line is never read.
    let (shared, _events) = shared(0);
    let mut stdout = vec![b'x'; 40];
    stdout.extend_from_slice(b"\n{\"type\":\"unknown\"}\n");
    let (control, seen) = run_read_loop(&shared, stdout, 16).await;
    assert!(control.was_killed());
    assert_eq!(seen, 0);
    assert!(shared.core.lock().await.has_ended());
}

#[tokio::test]
async fn a_stdout_line_at_the_limit_is_fed_to_the_core() {
    let (shared, _events) = shared(0);
    let line = "{\"type\":\"unknown\"}";
    let stdout = format!("{line}\n{line}\n").into_bytes();
    let (control, seen) = run_read_loop(&shared, stdout, line.len()).await;
    assert!(!control.was_killed());
    assert_eq!(seen, 2);
}

#[tokio::test]
async fn a_dispatch_with_a_full_stdin_queue_writes_nothing() {
    // NV-19.
    let (shared, _events) = shared(MAX_QUEUED_WRITES);
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 1}))], None, &tx)
        .await;
    assert!(is_write_error(&r), "{r:?}");
    assert!(is_queue_full(&r.unwrap_err()));
    assert!(rx.try_recv().is_err());
    assert_eq!(shared.queued.load(Ordering::Acquire), MAX_QUEUED_WRITES);
}

#[tokio::test]
async fn a_dispatch_counts_the_queued_frames_and_the_writer_frees_them() {
    let (shared, _events) = shared(0);
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    shared
        .dispatch(
            vec![
                Output::Write(json!({"n": 1})),
                Output::Write(json!({"n": 2})),
            ],
            None,
            &tx,
        )
        .await
        .unwrap();
    assert_eq!(shared.queued.load(Ordering::Acquire), 2);
    drop(rx.try_recv().unwrap());
    // A closed writer: the frame is not counted.
    drop(rx);
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 3}))], None, &tx)
        .await;
    assert!(matches!(r, Err(ProviderError::SessionEnded)), "{r:?}");
    assert_eq!(shared.queued.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn a_process_that_does_not_read_stdin_is_killed_when_the_queue_is_full() {
    // NV-19: a refusal reply has no room in the queue.
    let (shared, _events) = shared(MAX_QUEUED_WRITES);
    let frame = json!({"type":"control_request","request_id":"r1",
                       "request":{"subtype":"hook_callback","callback_id":"c","input":{}}});
    let stdout = format!("{frame}\n{frame}\n").into_bytes();
    let (control, seen) = run_read_loop(&shared, stdout, 1 << 20).await;
    assert!(control.was_killed());
    assert_eq!(seen, 1, "the loop stops after the first failed line");
}
