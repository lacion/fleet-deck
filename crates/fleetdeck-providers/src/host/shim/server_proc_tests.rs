//! Tests of the reaping of orphans (a Linux subreaper test process).
//!
//! Every test sets its own process as child subreaper, like the shim; each
//! test runs in its own process (nextest). The checks count only the
//! children of the process under test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

/// True under nextest, which runs each test in its own process. These tests
/// reap every zombie child of the test process, so under `cargo test` (one
/// process, many test threads) they could take the exit status of another
/// test's child: they do nothing there.
fn isolated() -> bool {
    std::env::var_os("NEXTEST").is_some()
}

fn become_subreaper() {
    rustix::process::set_child_subreaper(Some(rustix::process::getpid()))
        .expect("set the child subreaper");
}

/// A shell that starts a background `sleep` and ends at once: the sleep is
/// an orphan that comes back to the subreaper.
fn start_orphan() {
    let status = Command::new("sh")
        .args(["-c", "sleep 0.1 &"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("start the shell");
    assert!(status.success());
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

#[test]
fn reap_orphans_except_spares_the_kept_child_and_reaps_the_others() {
    if !isolated() {
        return;
    }
    become_subreaper();
    let mut kept = Command::new("sh")
        .args(["-c", "exit 3"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the kept child");
    let kept_pid = kept.id();
    start_orphan();
    // The kept child ended and stays a zombie; the orphan ends later.
    assert!(
        wait_until(Duration::from_secs(6), || {
            reap_orphans_except(Some(kept_pid));
            child_pids() == vec![kept_pid]
        }),
        "an orphan was not reaped: {:?}",
        child_pids()
    );
    // The kept child was not reaped: its owner still takes its status.
    let status = kept.wait().expect("the kept child is still waitable");
    assert_eq!(status.code(), Some(3));
}

#[test]
fn reap_orphans_except_none_reaps_every_zombie_child() {
    if !isolated() {
        return;
    }
    become_subreaper();
    start_orphan();
    assert!(
        wait_until(Duration::from_secs(6), || {
            reap_orphans_except(None);
            child_pids().is_empty()
        }),
        "an orphan was not reaped: {:?}",
        child_pids()
    );
}

#[tokio::test]
async fn the_periodic_reaper_reaps_orphans_while_the_provider_runs_and_spares_it() {
    if !isolated() {
        return;
    }
    become_subreaper();
    // The provider starts an orphan, keeps running, then ends with code 3.
    let mut provider = tokio::process::Command::new("sh")
        .args(["-c", "sh -c 'sleep 0.1 &'; sleep 4; exit 3"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start the provider");
    let provider_pid = provider.id().expect("the provider has a pid");
    let (exited_tx, exited_rx) = watch::channel(false);
    let reaper = tokio::spawn(reap_while_running(
        provider_pid,
        Duration::from_secs(1),
        exited_rx,
    ));
    let others = || {
        child_pids()
            .into_iter()
            .filter(|p| *p != provider_pid)
            .collect::<Vec<_>>()
    };
    // Before the first pass the ended orphan is a zombie of this process.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        others()
            .iter()
            .any(|p| matches!(stat_state_ppid(*p), Some(('Z', _)))),
        "the orphan did not become a zombie: {:?}",
        others()
    );
    // Within the limit the reaper reaped it, and the provider still runs.
    let end = Instant::now() + Duration::from_secs(2);
    while !others().is_empty() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(others().is_empty(), "a zombie stayed: {:?}", others());
    assert!(
        provider.try_wait().expect("poll the provider").is_none(),
        "the provider ended too early"
    );
    // The provider's own status is still reported.
    let status = provider.wait().await.expect("the provider status");
    assert_eq!(status.code(), Some(3));
    exited_tx.send_replace(true);
    reaper.await.expect("the reaper ends");
}
