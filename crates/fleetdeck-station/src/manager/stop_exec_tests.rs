//! The signalling of a stop plan over real process trees that the test
//! starts itself, each in its own session. Nothing else is signalled.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use std::os::fd::OwnedFd;
use std::time::Instant;

use fleetdeck_proto::execution_stop::{StopOutcome, code};

use super::*;
use crate::manager::stop_tree::{Tree, fifo, kill_process_of};

fn denied(_: &OwnedFd, _: Which) -> Sent {
    Sent::Denied
}

#[tokio::test]
async fn a_tree_gets_sigterm_through_its_pidfds_and_nothing_more() {
    let mut tree = Tree::simple();
    let plan = tree.plan("s", "shim");
    let result = execute(&plan, real_send()).await;
    assert_eq!(result.state, StopOutcome::Done, "{result:?}");
    assert_eq!(result.code, None);
    assert_eq!(
        (
            result.term_sent,
            result.kill_sent,
            result.gone,
            result.skipped
        ),
        (3, 0, 3, 0)
    );
    assert!(tree.members().is_empty());
    tree.end();
}

#[tokio::test]
async fn a_child_that_ignores_sigterm_gets_sigkill_after_five_seconds() {
    let mut tree = Tree::spawn("(trap '' TERM; exec sleep 600) & sleep 600 & wait");
    tree.wait_members(3);
    let plan = tree.plan("s", "shim");
    let started = Instant::now();
    let result = execute(&plan, real_send()).await;
    let took = started.elapsed();
    assert!(took >= TERM_WAIT, "SIGKILL came after {took:?}");
    assert!(took < TOTAL, "{took:?}");
    assert_eq!(result.state, StopOutcome::Done, "{result:?}");
    assert_eq!(result.code, None, "{result:?}");
    assert_eq!(result.term_sent, 3, "{result:?}");
    assert_eq!(
        result.kill_sent, 1,
        "only the survivor is killed: {result:?}"
    );
    assert_eq!((result.gone, result.skipped), (3, 0));
    assert!(tree.members().is_empty());
    tree.end();
}

#[tokio::test]
async fn a_member_that_the_plan_does_not_list_gives_processes_remain_and_no_signal() {
    let mut tree = Tree::simple();
    let mut plan = tree.plan("s", "shim");
    // The plan was made before one member existed.
    let new = plan.domain.pop().unwrap();
    let removed = plan.targets.pop().unwrap();
    assert_eq!(removed.pid, new.0);
    let result = execute(&plan, real_send()).await;
    assert_eq!(result.state, StopOutcome::Failed, "{result:?}");
    assert_eq!(result.code, Some(code::PROCESSES_REMAIN));
    assert_eq!((result.term_sent, result.kill_sent), (0, 0));
    assert_eq!(tree.members().len(), 3, "nothing was signalled");
    tree.end();
}

#[tokio::test]
async fn a_target_with_another_start_time_is_not_signalled() {
    let mut tree = Tree::simple();
    let mut plan = tree.plan("s", "shim");
    // The PID of this target now belongs to a later process, as far as the
    // plan can tell: the start time differs.
    let reused = plan.targets.last_mut().unwrap();
    let (pid, real) = (reused.pid, reused.start_time);
    reused.start_time += 1;
    let result = execute(&plan, real_send()).await;
    assert_eq!(result.term_sent, 2, "{result:?}");
    assert!(Tree::alive(pid), "the process of the reused PID lives");
    assert_eq!(result.code, Some(code::PROCESSES_REMAIN), "{result:?}");
    kill_process_of(pid, real);
    tree.end();
}

#[tokio::test]
async fn a_target_that_left_the_domain_is_skipped_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let go = fifo(dir.path());
    // The first child moves into a session of its own when the FIFO is
    // written: the same process (same PID, same start time) leaves the
    // domain after the plan.
    let script = format!(
        "(read x < {go}; exec setsid sleep 600) & sleep 600 & wait",
        go = go.display()
    );
    let mut tree = Tree::spawn(&script);
    tree.wait_members(3);
    let plan = tree.plan("s", "shim");
    Tree::release(&go);
    // Wait until a process of the plan has left the domain.
    let until = Instant::now() + std::time::Duration::from_secs(15);
    let left = loop {
        let now: Vec<u32> = tree.members().iter().map(|m| m.pid).collect();
        if let Some(t) = plan.targets.iter().find(|t| !now.contains(&t.pid)) {
            break (t.pid, t.start_time);
        }
        assert!(Instant::now() < until, "no process left the domain");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let result = execute(&plan, real_send()).await;
    assert_eq!(result.skipped, 1, "{result:?}");
    assert_eq!(result.term_sent, 2, "{result:?}");
    assert!(Tree::alive(left.0), "the process outside the domain lives");
    assert_eq!(result.code, None, "{result:?}");
    kill_process_of(left.0, left.1);
    tree.end();
}

#[tokio::test]
async fn a_refused_signal_fails_the_stop_with_permission_denied_and_the_rest_stays_visible() {
    let mut tree = Tree::simple();
    let plan = tree.plan("s", "shim");
    let result = execute(&plan, denied).await;
    assert_eq!(result.state, StopOutcome::Failed, "{result:?}");
    assert_eq!(result.code, Some(code::PERMISSION_DENIED));
    assert_eq!((result.term_sent, result.kill_sent, result.gone), (0, 0, 0));
    assert_eq!(tree.members().len(), 3);
    tree.end();
}
