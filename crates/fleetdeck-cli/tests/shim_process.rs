//! Real-process tests of the shim (ADR-005 §9, slice 2, DL-100): the
//! `fleetdeck shim` process, its socket, launch journal, lock and exit
//! file, with `/bin/sh` fake providers. The test process plays the
//! station. Tests named exactly as in ADR-005 §9 cover the whole statement
//! at the shim; a name with `_at_the_shim` covers the shim half of a test
//! whose station half (`ShimHost`, the `shims` table, recovery) comes in a
//! later 7-1 slice.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use fleetdeck_providers::host::shim::client::{AttachResult, Launched};
use fleetdeck_providers::host::shim::journal::{Fence, LaunchEvidence, Record, prepare_dir};
use fleetdeck_providers::host::shim::launch::LaunchReply;
use fleetdeck_providers::host::shim::proof::AttachKey;
use fleetdeck_providers::host::shim::wire::{
    FromShim, GapReason, Refusal, StatusAnswer, ToShim, WriteState,
};

mod shim_rig;
use shim_rig::*;

#[tokio::test]
async fn output_written_while_the_station_was_down_is_replayed_from_the_committed_cursor_exactly_once()
 {
    let rig = Rig::new(
        "i=0; while [ $i -lt 40 ]; do echo line$i; i=$((i+1)); sleep 0.02; done; sleep 30",
    );
    let (launched, _) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    a.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    let first = read_until(&mut a, |s| s.text().contains("line5\n")).await;
    let cursor = first.line_cursor();
    a.send(
        &ToShim::Ack {
            generation: 1,
            cursor,
        },
        &[],
    )
    .await
    .unwrap();
    // The station goes down: the provider keeps writing.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(a);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (mut b, attached) = rig.attach(2).await;
    let FromShim::Attached { cursor_floor, .. } = attached else {
        panic!("{attached:?}");
    };
    assert_eq!(cursor_floor, cursor);
    b.send(
        &ToShim::Replay {
            generation: 2,
            from: cursor,
        },
        &[],
    )
    .await
    .unwrap();
    let second = read_until(&mut b, |s| s.text().contains("line39\n")).await;
    assert_eq!(second.start, Some(cursor));
    let committed = &first.bytes[..usize::try_from(cursor).unwrap()];
    let mut all = String::from_utf8_lossy(committed).into_owned();
    all.push_str(&second.text());
    let expected: String = (0..40).map(|i| format!("line{i}\n")).collect();
    assert_eq!(all, expected);
    drop(b);
    end(&rig, launched, 3).await;
}

#[tokio::test]
async fn a_takeover_closes_the_old_attachment_and_a_stale_generation_cannot_write_or_stop() {
    let rig = Rig::new(ECHO);
    let (launched, pid) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    let (mut b, _) = rig.attach(2).await;
    // The old attachment is closed.
    let closed = a.recv(WAIT).await;
    assert!(matches!(closed, Ok(None) | Err(_)), "{closed:?}");
    drop(a.send(&write(1, "old", 1), b"old\n").await);
    drop(
        a.send(
            &ToShim::Stop {
                generation: 1,
                grace_ms: 0,
                term_grace_ms: 0,
            },
            &[],
        )
        .await,
    );
    // A new attach with the old generation is a stale owner.
    assert_eq!(rig.refused(INC, 1, &rig.key).await, Refusal::StaleOwner);
    assert_eq!(rig.refused(INC, 2, &rig.key).await, Refusal::StaleOwner);
    // A stale frame on the current connection closes it and does nothing.
    b.send(&ToShim::Kill { generation: 1 }, &[]).await.unwrap();
    let closed = b.recv(WAIT).await;
    assert!(matches!(closed, Ok(None) | Err(_)), "{closed:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(alive(pid));
    let (mut c, _) = rig.attach(3).await;
    c.send(
        &ToShim::Replay {
            generation: 3,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    c.send(&write(3, "new", 2), b"new\n").await.unwrap();
    read_until(&mut c, |s| s.text().contains("got:new\n")).await;
    assert_eq!(rig.rec(), "new\n");
    drop(c);
    end(&rig, launched, 4).await;
}

#[tokio::test]
async fn an_attach_with_another_incarnation_or_a_bad_proof_is_refused() {
    let rig = Rig::new(ECHO);
    let (launched, _) = rig.ready().await;
    let other = AttachKey::generate().unwrap();
    assert_eq!(rig.refused(INC, 1, &other).await, Refusal::BadProof);
    assert_eq!(
        rig.refused("inc-2", 1, &rig.key).await,
        Refusal::ForeignStation
    );
    let conn = rig.connect().await;
    let refused = conn
        .attach_with_proof(INC, 1, "zz".to_owned(), WAIT)
        .await
        .unwrap();
    assert!(matches!(refused, AttachResult::Refused(Refusal::BadProof)));
    // A refused peer advanced nothing: generation 1 still attaches.
    end(&rig, launched, 1).await;
}

#[tokio::test]
async fn a_duplicate_write_id_is_written_once() {
    let rig = Rig::new(ECHO);
    let (launched, _) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    a.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    a.send(&write(1, "w-1", 1), b"hello\n").await.unwrap();
    let done = |s: &Seen| {
        s.frames
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    FromShim::WriteDone {
                        state: WriteState::Written,
                        ..
                    }
                )
            })
            .count()
    };
    read_until(&mut a, |s| done(s) == 1).await;
    // The same write again, then on a new attachment.
    a.send(&write(1, "w-1", 1), b"hello\n").await.unwrap();
    read_until(&mut a, |s| done(s) == 1).await;
    drop(a);
    let (mut b, _) = rig.attach(2).await;
    b.send(&write(2, "w-1", 1), b"hello\n").await.unwrap();
    read_until(&mut b, |s| done(s) == 1).await;
    b.send(
        &ToShim::WriteStatus {
            generation: 2,
            write_id: "w-1".to_owned(),
            write_seq: 1,
        },
        &[],
    )
    .await
    .unwrap();
    let status = read_until(&mut b, |s| {
        s.frames
            .iter()
            .any(|f| matches!(f, FromShim::WriteStatus { .. }))
    })
    .await;
    assert!(status.frames.iter().any(|f| matches!(
        f,
        FromShim::WriteStatus {
            answer: StatusAnswer::Known {
                state: WriteState::Written
            },
            ..
        }
    )));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.rec(), "hello\n");
    drop(b);
    end(&rig, launched, 3).await;
}

#[tokio::test]
async fn an_exit_while_the_station_was_down_is_reported_with_its_exit_state_after_reattach() {
    let rig = Rig::new("read l; echo bye; echo oops >&2; exit 7");
    let (mut launched, pid) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    a.send(&write(1, "w-1", 1), b"go\n").await.unwrap();
    drop(a);
    wait_gone(pid).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut b, attached) = rig.attach(2).await;
    let FromShim::Attached {
        exit: Some(exit), ..
    } = attached
    else {
        panic!("{attached:?}");
    };
    assert_eq!(exit.termination, "code:7");
    assert_eq!(exit.cause, "exited");
    assert!(exit.stderr_tail.contains("oops"), "{exit:?}");
    assert_eq!(exit.final_offset, 4);
    b.send(
        &ToShim::Replay {
            generation: 2,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut b, |s| s.text() == "bye\n").await;
    assert_eq!(seen.start, Some(0));
    b.send(&ToShim::AckExit { generation: 2 }, &[])
        .await
        .unwrap();
    let status = tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert!(!rig.spec.socket_path.exists());
    assert!(!rig.files.key().exists());
    assert!(!rig.files.lock().exists());
    assert!(rig.files.journal().exists());
    assert!(rig.files.read_exit().is_none());
}

#[tokio::test]
async fn an_unacknowledged_exit_is_published_atomically_to_the_exit_file_and_read_at_recovery() {
    let rig = Rig::with("echo done; exit 3", |s| {
        s.exit_publish_after_ms = 200;
    });
    let (mut launched, _) = rig.ready().await;
    let status = tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    let exit = rig.files.read_exit().expect("the exit file");
    assert_eq!(exit.termination, "code:3");
    assert_eq!(exit.final_offset, 5);
    let mode = std::fs::metadata(rig.files.exit())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(!rig.spec.socket_path.exists());
    // No temporary file is left behind.
    let names: Vec<_> = std::fs::read_dir(&rig.spec.shims_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
}

#[tokio::test]
async fn a_socket_directory_with_another_owner_or_open_mode_is_refused() {
    // An open socket directory.
    let rig = Rig::new(ECHO);
    let run = rig.spec.socket_path.parent().unwrap().to_owned();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut launched = rig.launch().await;
    assert!(
        matches!(
            &launched.reply,
            LaunchReply::Failed {
                spawn_attempted: false,
                ..
            }
        ),
        "{:?}",
        launched.reply
    );
    launched.child.wait().await.unwrap();
    assert!(!rig.spec.socket_path.exists());
    assert_eq!(
        rig.files.records().unwrap(),
        vec![Record::Created {
            shim_id: rig.spec.shim_id.clone(),
            incarnation: INC.to_owned(),
            boot_id: None,
        }]
    );
    // An open shims directory.
    let rig = Rig::new(ECHO);
    std::fs::set_permissions(&rig.spec.shims_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
    let mut launched = rig.launch().await;
    assert!(matches!(
        &launched.reply,
        LaunchReply::Failed {
            spawn_attempted: false,
            ..
        }
    ));
    launched.child.wait().await.unwrap();
    // A socket directory that is a symlink to a private directory.
    let rig = Rig::new(ECHO);
    let real = rig.tmp.path().join("real");
    prepare_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, rig.spec.socket_path.parent().unwrap()).unwrap();
    let mut launched = rig.launch().await;
    assert!(matches!(
        &launched.reply,
        LaunchReply::Failed {
            spawn_attempted: false,
            ..
        }
    ));
    launched.child.wait().await.unwrap();
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}

#[tokio::test]
async fn dropping_every_station_handle_never_stops_the_provider() {
    let rig = Rig::new(ECHO);
    let (launched, pid) = rig.ready().await;
    let (a, _) = rig.attach(1).await;
    let Launched { child, .. } = launched;
    let shim_pid = child.id().unwrap();
    drop(a);
    drop(child);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(alive(pid));
    assert!(alive(shim_pid));
    let (mut b, _) = rig.attach(2).await;
    b.send(
        &ToShim::Replay {
            generation: 2,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    b.send(&write(2, "w-1", 1), b"still\n").await.unwrap();
    read_until(&mut b, |s| s.text().contains("got:still\n")).await;
    b.send(&ToShim::Kill { generation: 2 }, &[]).await.unwrap();
    read_until(&mut b, |s| exited(s).is_some()).await;
    b.send(&ToShim::AckExit { generation: 2 }, &[])
        .await
        .unwrap();
    wait_gone(shim_pid).await;
}

#[tokio::test]
async fn a_complete_frame_of_the_old_generation_after_a_takeover_is_dropped_and_never_written() {
    let rig = Rig::new(ECHO);
    let (launched, _) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    let (mut b, _) = rig.attach(2).await;
    // A complete old-generation frame arrives after the takeover.
    drop(a.send(&write(1, "w-old", 1), b"old\n").await);
    b.send(
        &ToShim::Replay {
            generation: 2,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    b.send(&write(2, "w-new", 2), b"new\n").await.unwrap();
    read_until(&mut b, |s| s.text().contains("got:new\n")).await;
    b.send(
        &ToShim::WriteStatus {
            generation: 2,
            write_id: "w-old".to_owned(),
            write_seq: 1,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut b, |s| {
        s.frames
            .iter()
            .any(|f| matches!(f, FromShim::WriteStatus { .. }))
    })
    .await;
    assert!(
        seen.frames.iter().any(|f| matches!(
            f,
            FromShim::WriteStatus {
                answer: StatusAnswer::NotReceived,
                ..
            }
        )),
        "{:?}",
        seen.frames
    );
    // The tombstone refuses the identity for good.
    b.send(&write(2, "w-old", 1), b"old\n").await.unwrap();
    let seen = read_until(&mut b, |s| {
        s.frames
            .iter()
            .any(|f| matches!(f, FromShim::WriteDone { write_id, .. } if write_id == "w-old"))
    })
    .await;
    assert!(
        seen.frames.iter().any(|f| matches!(
            f,
            FromShim::WriteDone {
                state: WriteState::ClosedUnwritten,
                ..
            }
        )),
        "{:?}",
        seen.frames
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.rec(), "new\n");
    drop((a, b));
    end(&rig, launched, 3).await;
}

#[tokio::test]
async fn a_line_of_the_buffer_size_without_a_newline_fails_the_session_with_an_explicit_gap_and_stops_the_provider_at_the_shim()
 {
    let rig = Rig::new("echo first; head -c 200000 /dev/zero | tr '\\0' x; sleep 30");
    let (mut launched, pid) = rig.ready().await;
    // Give the provider time to fill the buffer before the attach.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (mut a, _) = rig.attach(1).await;
    a.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    // The station commits and acknowledges the complete first line; the
    // rest of the buffer then fills with one line without a newline.
    let first = read_until(&mut a, |s| s.text().contains("first\n")).await;
    assert_eq!(first.line_cursor(), 6);
    a.send(
        &ToShim::Ack {
            generation: 1,
            cursor: 6,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut a, |s| exited(s).is_some()).await;
    let gap = seen.frames.iter().find_map(|f| match f {
        FromShim::Gap {
            from,
            to,
            reason: GapReason::OutputFrameTooLarge,
        } => Some((*from, *to)),
        _ => None,
    });
    let (from, to) = gap.expect("an output_frame_too_large gap");
    assert_eq!(to - from, 64 * 1024, "{from}..{to}");
    let exit = exited(&seen).unwrap();
    assert_eq!(exit.cause, "stop_forced");
    assert!(!alive(pid));
    a.send(&ToShim::AckExit { generation: 1 }, &[])
        .await
        .unwrap();
    tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_blocked_stdin_write_does_not_block_kill_takeover_or_status() {
    let rig = Rig::new("sleep 60");
    let (mut launched, pid) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    // 900 KiB never fit in the pipe of a provider that does not read.
    a.send(&write(1, "w-big", 1), &vec![b'x'; 900 * 1024])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    a.send(&ToShim::Status { generation: 1 }, &[])
        .await
        .unwrap();
    let seen = read_until(&mut a, |s| {
        s.frames
            .iter()
            .any(|f| matches!(f, FromShim::Status { .. }))
    })
    .await;
    assert!(
        seen.frames
            .iter()
            .any(|f| matches!(f, FromShim::Status { pending_writes, .. } if *pending_writes >= 1)),
        "{:?}",
        seen.frames
    );
    a.send(
        &ToShim::WriteStatus {
            generation: 1,
            write_id: "w-big".to_owned(),
            write_seq: 1,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut a, |s| {
        s.frames
            .iter()
            .any(|f| matches!(f, FromShim::WriteStatus { .. }))
    })
    .await;
    assert!(
        seen.frames.iter().any(|f| matches!(
            f,
            FromShim::WriteStatus {
                answer: StatusAnswer::Known {
                    state: WriteState::Writing { .. }
                },
                ..
            }
        )),
        "{:?}",
        seen.frames
    );
    let (mut b, _) = tokio::time::timeout(Duration::from_secs(2), rig.attach(2))
        .await
        .unwrap();
    b.send(&ToShim::Kill { generation: 2 }, &[]).await.unwrap();
    let seen = tokio::time::timeout(
        Duration::from_secs(3),
        read_until(&mut b, |s| exited(s).is_some()),
    )
    .await
    .unwrap();
    assert_eq!(exited(&seen).unwrap().cause, "killed");
    assert!(!alive(pid));
    b.send(&ToShim::AckExit { generation: 2 }, &[])
        .await
        .unwrap();
    tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_station_crash_during_a_launch_is_fenced_or_resolved_by_the_launch_lock_before_classification_at_the_shim()
 {
    let rig = Rig::new(ECHO);
    let (launched, _) = rig.ready().await;
    // A second start of the same shim finds the lock held.
    let mut second = rig.launch().await;
    assert_eq!(second.reply, LaunchReply::Locked);
    second.child.wait().await.unwrap();
    // Recovery cannot take the lock of a live shim, so it does not fence.
    assert_eq!(rig.files.fence(INC).unwrap(), Fence::Held);
    assert!(!rig.files.is_fenced());
    end(&rig, launched, 1).await;
}

#[tokio::test]
async fn a_launch_interrupted_after_the_provider_spawn_and_before_the_identity_commit_is_never_retired_as_not_dispatched_at_the_shim()
 {
    let rig = Rig::new(ECHO);
    let (launched, pid) = rig.ready().await;
    let shim_pid = launched.child.id().unwrap();
    // The shim dies (SIGKILL); its provider follows (parent-death signal).
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(shim_pid).unwrap()).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    let mut child = launched.child;
    child.wait().await.unwrap();
    wait_gone(pid).await;
    // The volatile runtime directory is gone (a reboot model).
    std::fs::remove_dir_all(rig.spec.socket_path.parent().unwrap()).unwrap();
    assert_eq!(
        rig.files.fence(INC).unwrap(),
        Fence::Fenced(LaunchEvidence::Unknown)
    );
    assert_eq!(rig.files.evidence(INC), LaunchEvidence::Unknown);
    // A truncated or missing journal proves nothing either.
    let text = std::fs::read_to_string(rig.files.journal()).unwrap();
    std::fs::write(rig.files.journal(), &text[..text.len() - 3]).unwrap();
    assert_eq!(rig.files.evidence(INC), LaunchEvidence::Unknown);
    std::fs::remove_file(rig.files.journal()).unwrap();
    assert_eq!(rig.files.evidence(INC), LaunchEvidence::Unknown);
}

#[tokio::test]
async fn only_an_intact_journal_without_spawn_attempted_retires_a_launch_as_shim_never_started() {
    let rig = Rig::new(ECHO);
    // The station crashed before the shim started: recovery fences.
    assert_eq!(
        rig.files.fence(INC).unwrap(),
        Fence::Fenced(LaunchEvidence::NeverStarted)
    );
    // The late shim exits without a socket and without a provider.
    let mut late = rig.launch().await;
    assert_eq!(late.reply, LaunchReply::Fenced);
    let status = late.child.wait().await.unwrap();
    assert!(status.success());
    assert!(!rig.spec.socket_path.exists());
    assert!(rig.rec().is_empty());
    assert_eq!(
        rig.files.records().unwrap(),
        vec![
            Record::Created {
                shim_id: rig.spec.shim_id.clone(),
                incarnation: INC.to_owned(),
                boot_id: None,
            },
            Record::Fenced
        ]
    );
    assert_eq!(rig.files.evidence(INC), LaunchEvidence::NeverStarted);
    // Another incarnation's view of the same journal proves nothing.
    assert_eq!(rig.files.evidence("inc-2"), LaunchEvidence::Unknown);
}

#[tokio::test]
async fn the_provider_runs_in_its_own_session_and_group_with_a_cleared_environment() {
    let rig = Rig::new("echo \"env:$(env | sort | tr '\\n' ' ')\"; sleep 30");
    let (launched, pid) = rig.ready().await;
    let (mut a, _) = rig.attach(1).await;
    a.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut a, |s| s.text().ends_with('\n')).await;
    let text = seen.text();
    assert!(text.contains("PATH=/usr/bin:/bin"), "{text}");
    assert!(!text.contains("HOME="), "{text}");
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &stat[stat.rfind(')').unwrap() + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // Fields after the name: state ppid pgrp session.
    assert_eq!(fields[2], pid.to_string(), "process group");
    assert_eq!(fields[3], pid.to_string(), "session");
    assert_eq!(
        fields[1],
        launched.child.id().unwrap().to_string(),
        "the shim is the parent"
    );
    drop(a);
    end(&rig, launched, 2).await;
}
