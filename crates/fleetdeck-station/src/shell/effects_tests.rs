//! DL-405: shell commands are durable effects. The receipt transaction does
//! no PTY I/O; a crash after the commit never runs the effect in a later
//! process; a slow reader cannot grow the station memory.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_proto::Mutation;
use fleetdeck_proto::command::ReceiptState;
use fleetdeck_proto::shell::{ShellChange, ShellEnvelope, ShellState};
use fleetdeck_proto::{CommandId, RejectCode};
use fleetdeck_testkit::Det;
use tokio::sync::mpsc::error::TryRecvError;

use super::tests::{env, open, policy, read_until};
use super::{SUBSCRIBER_QUEUE, ShellConfig, Shells, decide, dispatch};
use crate::manager::policy::Policy;
use crate::store::{Caller, Decided, Store, StoreConfig};

fn shells_with(program: &str, max_shells: u32) -> Arc<Shells> {
    Arc::new(Shells::with_env(
        ShellConfig {
            allow: true,
            max_shells,
            program: Some((program.into(), Vec::new())),
        },
        env(),
    ))
}

async fn file_store(det: &Det, path: &std::path::Path) -> Store {
    let s = Store::open(path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .expect("open");
    s.recover(Vec::new()).await.expect("recover");
    s
}

async fn command(
    store: &Store,
    shells: &Arc<Shells>,
    policy: &Policy,
    id: &str,
    change: ShellChange,
) -> Decided {
    let envelope = ShellEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: store.incarnation().clone(),
        retry_until: store.now_ms() + 60_000,
        change,
    };
    let (sh, pol) = (Arc::clone(shells), policy.clone());
    store
        .decide_mutation(
            Mutation::Shell(envelope.clone()),
            Caller::local(),
            move |d| decide(d, &sh, &pol, &envelope),
        )
        .await
        .expect("decide")
}

async fn state(store: &Store, id: &str) -> (String, Option<String>) {
    let s = store
        .shell_effect(CommandId::from(id))
        .await
        .expect("read")
        .expect("stored");
    (s.state, s.detail)
}

#[tokio::test]
async fn a_shell_command_committed_before_a_crash_never_runs_after_the_restart() {
    let det = Det::new();
    let dir = tempfile::tempdir().expect("dir");
    let root = tempfile::tempdir().expect("root");
    let db = dir.path().join("station.db");
    let p = policy(root.path());

    // Process 1 commits two opens and crashes: one before the claim, one
    // after the claim and before the PTY I/O.
    let first = file_store(&det, &db).await;
    let shells1 = shells_with("/bin/sh", 4);
    let d = command(&first, &shells1, &p, "sh-a", open("a", root.path())).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    // The receipt transaction did no PTY I/O.
    assert!(shells1.list().is_empty());
    assert_eq!(state(&first, "sh-a").await.0, "queued");
    let d = command(&first, &shells1, &p, "sh-b", open("b", root.path())).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    // A used id is refused while its open is still pending.
    let d = command(&first, &shells1, &p, "sh-a2", open("a", root.path())).await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected);
    let claimed = first
        .claim_shell_effect()
        .await
        .expect("claim")
        .expect("one");
    assert_eq!(claimed.command_id, CommandId::from("sh-a"));
    drop(first);
    drop(shells1);

    // Process 2: start recovery ends both effects; nothing runs.
    let second = file_store(&det, &db).await;
    let shells2 = shells_with("/bin/sh", 4);
    assert_eq!(
        state(&second, "sh-a").await,
        (
            "outcome_unknown".to_owned(),
            Some("station_restart".to_owned())
        )
    );
    assert_eq!(
        state(&second, "sh-b").await,
        (
            "retired_not_dispatched".to_owned(),
            Some("station_restart".to_owned())
        )
    );
    assert_eq!(dispatch(&second, &shells2, &p).await.expect("dispatch"), 0);
    assert!(shells2.list().is_empty());

    // A retry with the same command id gets the stored receipt and runs
    // nothing.
    let d = command(&second, &shells2, &p, "sh-b", open("b", root.path())).await;
    assert!(d.duplicate);
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert_eq!(dispatch(&second, &shells2, &p).await.expect("dispatch"), 0);
    assert!(shells2.list().is_empty());
    // A new command works in the new process.
    let d = command(&second, &shells2, &p, "sh-c", open("c", root.path())).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert_eq!(dispatch(&second, &shells2, &p).await.expect("dispatch"), 1);
    assert_eq!(state(&second, "sh-c").await.0, "confirmed");
    assert_eq!(shells2.list().len(), 1);
    shells2.close_all();
}

#[tokio::test]
async fn shell_effects_run_after_the_commit_in_order_and_a_pty_failure_is_recorded() {
    let det = Det::new();
    let dir = tempfile::tempdir().expect("dir");
    let root = tempfile::tempdir().expect("root");
    let p = policy(root.path());
    let store = file_store(&det, &dir.path().join("station.db")).await;
    let shells = shells_with("/bin/sh", 1);

    // A resize and a close of a shell whose open is pending are accepted;
    // the cap counts the pending open.
    command(&store, &shells, &p, "sh-1", open("a", root.path())).await;
    let resize = ShellChange::Resize {
        shell_id: "a".to_owned(),
        cols: 100,
        rows: 30,
    };
    let d = command(&store, &shells, &p, "sh-2", resize).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    let d = command(&store, &shells, &p, "sh-3", open("b", root.path())).await;
    let reject = d.receipt.reject.expect("rejected");
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(reject.message.contains("max_shells"), "{reject:?}");
    assert_eq!(dispatch(&store, &shells, &p).await.expect("dispatch"), 2);
    assert_eq!(state(&store, "sh-1").await.0, "confirmed");
    assert_eq!(state(&store, "sh-2").await.0, "confirmed");
    let info = &shells.list()[0];
    assert_eq!(
        (info.cols, info.rows, info.state),
        (100, 30, ShellState::Running)
    );

    let close = ShellChange::Close {
        shell_id: "a".to_owned(),
    };
    command(&store, &shells, &p, "sh-4", close).await;
    assert_eq!(dispatch(&store, &shells, &p).await.expect("dispatch"), 1);
    assert_eq!(state(&store, "sh-4").await.0, "confirmed");
    assert!(shells.list().is_empty());

    // The PTY cannot start the program: the effect is `failed` with the
    // reason, and no shell is listed.
    let broken = shells_with("/nonexistent/fleetdeck-shell", 1);
    command(&store, &broken, &p, "sh-5", open("x", root.path())).await;
    assert_eq!(dispatch(&store, &broken, &p).await.expect("dispatch"), 1);
    let (st, detail) = state(&store, "sh-5").await;
    assert_eq!(st, "failed");
    assert!(
        detail.as_deref().unwrap_or("").contains("start the shell"),
        "{detail:?}"
    );
    assert!(broken.list().is_empty());
}

#[test]
fn a_slow_reader_is_cut_off_and_the_shell_keeps_running_with_its_scrollback() {
    let root = tempfile::tempdir().expect("root");
    let p = policy(root.path());
    let s = shells_with("/bin/sh", 1);
    s.apply(&open("sh", root.path()), &p, 1).expect("open");
    // This reader never reads while the shell writes about 20 MB.
    let (_, mut slow) = s.attach("sh").expect("attach");
    assert!(s.input(
        "sh",
        b"head -c 20000000 /dev/zero | tr '\\\\0' y; echo DONE-$((40+2))\n"
    ));
    let t0 = Instant::now();
    loop {
        let (backlog, _) = s.attach("sh").expect("attach");
        if String::from_utf8_lossy(&backlog).contains("DONE-42") {
            assert!(backlog.len() <= fleetdeck_proto::shell::SCROLLBACK_BYTES);
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "no DONE-42");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The slow queue holds at most its bound, then it ends.
    let mut queued = 0;
    loop {
        match slow.try_recv() {
            Ok(_) => queued += 1,
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => panic!("the slow reader was not cut off"),
        }
    }
    assert!(queued <= SUBSCRIBER_QUEUE, "{queued}");
    // The shell still runs, and a new reader gets the live output.
    assert_eq!(s.list()[0].state, ShellState::Running);
    let (_, mut fresh) = s.attach("sh").expect("attach");
    assert!(s.input("sh", b"echo AGAIN-$((40+3))\n"));
    let mut seen = String::new();
    read_until(&mut seen, &mut fresh, "AGAIN-43");
    s.close_all();
}
