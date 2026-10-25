//! Real-shim regressions of ShimHost CODE REVIEW part A sa-1d231480
//! (DL-121): the original read time survives a replay, a takeover is never
//! blocked by a superseded attachment that stops reading, and the exit
//! report never carries a final cursor before the provider's stdout was
//! drained.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod shim_rig;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleetdeck_providers::host::shim::client::Attachment;
use fleetdeck_providers::host::shim::wire::{ExitState, FromShim, ToShim};
use shim_rig::{Rig, WAIT, end};

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

async fn replay(att: &mut Attachment, generation: u64, from: u64) {
    att.send(&ToShim::Replay { generation, from }, &[])
        .await
        .unwrap();
}

/// The next output frame: `(offset, read_at, bytes)`.
async fn output(att: &mut Attachment) -> (u64, u64, Vec<u8>) {
    loop {
        match att.recv(WAIT).await.unwrap() {
            Some((FromShim::Output { offset, read_at }, bytes)) => return (offset, read_at, bytes),
            Some(_) => {}
            None => panic!("the shim closed"),
        }
    }
}

#[tokio::test]
async fn a_replay_after_a_reattach_keeps_the_original_read_time_of_the_output() {
    let rig = Rig::new("echo one; sleep 30");
    let (launched, _) = rig.ready().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let before_attach = now_ms();
    let (mut a, _) = rig.attach(1).await;
    replay(&mut a, 1, 0).await;
    let (offset, first, bytes) = output(&mut a).await;
    assert_eq!((offset, bytes.as_slice()), (0, &b"one\n"[..]));
    assert!(
        first + 1000 <= before_attach,
        "the read time is the pipe read, not the send: {first} vs {before_attach}"
    );
    drop(a);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let (mut b, _) = rig.attach(2).await;
    replay(&mut b, 2, 0).await;
    let (offset, again, bytes) = output(&mut b).await;
    assert_eq!((offset, bytes.as_slice()), (0, &b"one\n"[..]));
    assert_eq!(again, first, "a replay never replaces the read time");
    drop(b);
    end(&rig, launched, 3).await;
}

#[tokio::test]
async fn a_takeover_replays_to_the_new_owner_while_the_old_output_socket_is_saturated_and_unread() {
    // More than the attachment queue (256 frames of 64 KiB) and the socket
    // buffers together, so a streamer that waits on the old owner blocks.
    const SIZE: usize = 20 * 1024 * 1024;
    let rig = Rig::with(
        &format!("head -c {SIZE} /dev/zero | tr '\\0' '\\n'; sleep 30"),
        |spec| spec.buffer_bytes = SIZE,
    );
    let (launched, _) = rig.ready().await;
    // The old owner asks for the output and never reads it.
    let (mut old, _) = rig.attach(1).await;
    replay(&mut old, 1, 0).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (mut new, _) = rig.attach(2).await;
    replay(&mut new, 2, 0).await;
    let mut received = 0usize;
    let deadline = tokio::time::Instant::now() + WAIT;
    while received < SIZE {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the new owner got {received} of {SIZE} bytes"
        );
        let (offset, _, bytes) = output(&mut new).await;
        assert_eq!(offset, received as u64, "contiguous replay");
        received += bytes.len();
        // The new owner commits and acks (every byte is a newline).
        new.send(
            &ToShim::Ack {
                generation: 2,
                cursor: received as u64,
            },
            &[],
        )
        .await
        .unwrap();
    }
    assert_eq!(received, SIZE);
    drop(old);
    drop(new);
    end(&rig, launched, 3).await;
}

#[tokio::test]
async fn an_exit_with_a_full_buffer_reports_the_final_cursor_only_after_the_output_is_drained() {
    const BUFFER: usize = 1024 * 1024;
    const SIZE: u64 = 1_052_672;
    let rig = Rig::with(
        &format!("head -c {SIZE} /dev/zero | tr '\\0' '\\n'"),
        |spec| spec.buffer_bytes = BUFFER,
    );
    let (mut launched, _) = rig.ready().await;
    // Longer than the shim's stdout wait: the provider exited long ago and
    // its last bytes are still in the pipe behind the full buffer.
    tokio::time::sleep(Duration::from_millis(6500)).await;
    let (mut a, attached) = rig.attach(1).await;
    let FromShim::Attached { exit, .. } = attached else {
        panic!("{attached:?}");
    };
    if let Some(exit) = &exit {
        assert_eq!(exit.final_offset, SIZE, "{exit:?}");
    }
    replay(&mut a, 1, 0).await;
    let mut received = 0u64;
    let mut report: Option<ExitState> = exit;
    let deadline = tokio::time::Instant::now() + WAIT;
    while received < SIZE || report.is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "received {received} of {SIZE}, exit {report:?}"
        );
        match a.recv(WAIT).await.unwrap() {
            Some((FromShim::Output { offset, .. }, bytes)) => {
                assert_eq!(offset, received, "no byte is lost");
                received += bytes.len() as u64;
                // Every byte is a newline: the station commits and acks it.
                a.send(
                    &ToShim::Ack {
                        generation: 1,
                        cursor: received,
                    },
                    &[],
                )
                .await
                .unwrap();
            }
            Some((FromShim::Exited { exit }, _)) => {
                assert_eq!(exit.final_offset, SIZE, "{exit:?}");
                report = Some(exit);
            }
            Some((FromShim::Gap { from, to, reason }, _)) => {
                panic!("no gap expected: {from}..{to} {reason:?}")
            }
            Some(_) => {}
            None => panic!("the shim closed after {received} bytes"),
        }
    }
    assert_eq!(received, SIZE);
    let exit = report.unwrap();
    assert_eq!(exit.termination, "code:0");
    assert_eq!(exit.final_offset, SIZE);
    a.send(&ToShim::AckExit { generation: 1 }, &[])
        .await
        .unwrap();
    let status = tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status:?}");
}
