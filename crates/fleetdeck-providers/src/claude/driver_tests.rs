//! The write acknowledgement of a host command (PLAN §6.4: `sent` only
//! after the I/O; a write that can be partial is an unknown outcome).

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use super::{Write, flush};
use crate::adapter::ProviderError;
use crate::host::HostError;

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
