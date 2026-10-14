//! The tests of the read loop and the dispatch of the Codex driver.

use std::io::Cursor;
use std::sync::atomic::Ordering;

use serde_json::json;

use super::*;
use crate::codex::tests::config;
use crate::stdio::test_support::FakeControl;
use crate::stdio::{MAX_QUEUED_WRITES, is_queue_full};

fn shared(queued: usize) -> Arc<Shared> {
    let (sink, _events) = EventSink::channel(64);
    Arc::new(Shared {
        core: Mutex::new(CodexCore::new(config("/w"))),
        sink,
        raw: std::sync::Mutex::new(None),
        poll: Notify::new(),
        queued: AtomicUsize::new(queued),
    })
}

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
    let shared = shared(0);
    let mut stdout = vec![b'x'; 40];
    stdout.extend_from_slice(b"\n{\"method\":\"brand/new\"}\n");
    let (control, seen) = run_read_loop(&shared, stdout, 16).await;
    assert!(control.was_killed());
    assert_eq!(seen, 0);
    assert!(shared.core.lock().await.has_ended());
}

#[tokio::test]
async fn a_stdout_line_at_the_limit_is_fed_to_the_core() {
    let shared = shared(0);
    let line = "{\"method\":\"brand/new\"}";
    let stdout = format!("{line}\n{line}\n").into_bytes();
    let (control, seen) = run_read_loop(&shared, stdout, line.len()).await;
    assert!(!control.was_killed());
    assert_eq!(seen, 2);
}

#[tokio::test]
async fn a_dispatch_with_a_full_stdin_queue_writes_nothing() {
    // NV-19.
    let shared = shared(MAX_QUEUED_WRITES);
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 1}))], None, &tx)
        .await;
    assert!(is_queue_full(&r.unwrap_err()));
    assert!(rx.try_recv().is_err());
    assert_eq!(shared.queued.load(Ordering::Acquire), MAX_QUEUED_WRITES);
}

#[tokio::test]
async fn a_dispatch_counts_the_queued_frames_and_a_closed_writer_leaves_the_count() {
    let shared = shared(0);
    let (tx, rx) = mpsc::unbounded_channel::<Write>();
    shared
        .dispatch(vec![Output::Write(json!({"n": 1}))], None, &tx)
        .await
        .unwrap();
    assert_eq!(shared.queued.load(Ordering::Acquire), 1);
    drop(rx);
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 2}))], None, &tx)
        .await;
    assert!(matches!(r, Err(ProviderError::SessionEnded)), "{r:?}");
    assert_eq!(shared.queued.load(Ordering::Acquire), 1);
}
