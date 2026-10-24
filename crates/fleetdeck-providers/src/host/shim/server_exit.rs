//! The exit path of the shim (DL-431): the exit sweep with a pinned leader,
//! then the leader reap, the exit report and its publication.
//!
//! The leader is a zombie from `LeaderWait::Ended` until the reap, so its
//! pid number cannot be reused: every process with `pgrp == L` or
//! `session == L` belongs to the run, and `kill(-L)` hits only the run. After
//! the reap no code of the shim sends a numeric signal to `L` or `-L`.

use std::time::{Duration, Instant};

use rustix::process::Signal;

use super::*;
use crate::host::shim::inspect::{ProcStat, exit_census, parse_stat};

/// The members of a run may end by themselves for this long.
const MEMBER_GRACE: Duration = Duration::from_secs(2);
/// After SIGTERM they get this long before SIGKILL.
const MEMBER_TERM_GRACE: Duration = Duration::from_secs(2);
/// The most SIGKILL rounds of the sweep.
const KILL_ROUNDS: u32 = 3;
/// How often the sweep counts the members while it waits.
const SWEEP_POLL: Duration = Duration::from_millis(50);
/// The pause after one SIGKILL round.
const KILL_PAUSE: Duration = Duration::from_millis(100);

/// The live members of a census; `None` when the census was incomplete.
type Members = Option<Vec<ProcStat>>;

/// True when a complete census found members. An incomplete census (`None`)
/// is not a reason to wait or to signal: the final census reports it.
fn members_remain(members: &Members) -> bool {
    matches!(members, Some(m) if !m.is_empty())
}

impl Shim {
    /// The provider ended (`waited`). Stops what is left of the run, reaps
    /// the leader, then publishes the exit report.
    pub(super) async fn on_exit(
        self: &Arc<Self>,
        child: &mut Child,
        waited: LeaderWait,
        eof: watch::Receiver<bool>,
    ) {
        let leader = self.pid();
        let (status, descendants_remaining) = match waited {
            LeaderWait::Ended => {
                // E1: the leader is a zombie; `stop()` ends its waits.
                self.leader.set(Lifecycle::Ended);
                self.exited.send_replace(true);
                // E2: the sweep runs on a blocking thread (it sleeps).
                let killed = self.lock().cause == Some("killed");
                let escalate = self.leader.escalated();
                let remaining =
                    tokio::task::spawn_blocking(move || exit_sweep(leader, killed, &escalate))
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "the exit sweep did not run");
                            None
                        });
                // E3: the leader is still pinned.
                if let Err(e) = kill_group_only(leader, Signal::KILL)
                    && e != rustix::io::Errno::SRCH
                {
                    tracing::warn!(error = %e, "cannot signal the provider group");
                }
                // E4: no numeric signal can run from here on.
                self.leader.set(Lifecycle::Reaped);
                let status = child.wait().await;
                // E5
                reap_orphans();
                (status, remaining)
            }
            LeaderWait::Failed(errno) => {
                // U1: fail closed; no numeric signal, no sweep.
                tracing::warn!(error = %errno, "the provider wait failed; no exit sweep");
                self.leader.set(Lifecycle::Unpinned);
                // U2
                let status = child.wait().await;
                self.leader.set(Lifecycle::Reaped);
                self.exited.send_replace(true);
                // U3, U4
                let shim = Arc::clone(self);
                let remaining = tokio::task::spawn_blocking(move || {
                    unpinned_count(&shim.leader, || {
                        reap_orphans();
                        group_members(leader)
                    })
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "the process inspection did not run");
                    None
                });
                (status, remaining)
            }
        };
        if descendants_remaining.is_none() {
            tracing::warn!("the process inspection was incomplete; the exit is unverified");
        }
        let termination = termination_of(&status);
        // E6 / U5
        self.wait_stdout_end(eof).await;
        reap_orphans();
        let exit = {
            let mut state = self.lock();
            let stderr_tail =
                String::from_utf8_lossy(state.stderr_tail.make_contiguous()).into_owned();
            let exit = ExitState {
                termination,
                cause: state.cause.unwrap_or("exited").to_owned(),
                stderr_tail,
                descendants_remaining,
                final_offset: state.buffer.end(),
                inspection: INSPECTION,
            };
            state.exit = Some(exit.clone());
            exit
        };
        self.notify_exit(&exit);
        tokio::spawn(Arc::clone(self).publish_exit(exit));
    }
}

fn termination_of(status: &std::io::Result<std::process::ExitStatus>) -> String {
    use std::os::unix::process::ExitStatusExt as _;
    match status {
        Ok(s) => match (s.code(), s.signal()) {
            (Some(code), _) => format!("code:{code}"),
            (None, Some(sig)) => format!("signal:{sig}"),
            (None, None) => "unknown".to_owned(),
        },
        Err(_) => "unknown".to_owned(),
    }
}

/// The count of the unpinned path (steps U2-U4, after the reap): the state
/// is `Reaped`, the census runs and nothing is sent. A state that is not
/// `Reaped` is a bug: the count is then incomplete (fail closed).
pub(super) fn unpinned_count(leader: &Leader, census: impl FnOnce() -> Option<u32>) -> Option<u32> {
    if leader.lifecycle() != Lifecycle::Reaped {
        debug_assert!(false, "the unpinned census runs after the reap");
        tracing::error!("the unpinned census ran before the reap");
        return None;
    }
    census()
}

/// Waits while members remain, for at most `grace`, counting every
/// [`SWEEP_POLL`]. It ends at once when `escalate` is true (a Kill frame).
/// Returns the last census.
fn wait_for_end(
    leader: u32,
    grace: Duration,
    escalate: &watch::Receiver<bool>,
    mut last: Members,
) -> Members {
    let end = Instant::now() + grace;
    loop {
        if !members_remain(&last) || *escalate.borrow() {
            return last;
        }
        let now = Instant::now();
        if now >= end {
            return last;
        }
        std::thread::sleep(SWEEP_POLL.min(end - now));
        reap_orphans_except(Some(leader));
        last = pinned_members(leader);
    }
}

/// Signals the group of the run and each live session member outside it.
fn signal_all(leader: u32, members: &Members, sig: Signal) {
    if let Err(e) = kill_group_only(leader, sig)
        && e != rustix::io::Errno::SRCH
    {
        tracing::warn!(?sig, error = %e, "cannot signal the provider group");
    }
    for member in members.iter().flatten().filter(|m| m.pgrp != leader) {
        signal_member(leader, member, sig);
    }
}

/// Signals a session member in another group through a pidfd, only when a
/// fresh read shows the same session and start time as the census read.
fn signal_member(leader: u32, member: &ProcStat, sig: Signal) {
    let Some(pid) = i32::try_from(member.pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let fd = match super::leader::pidfd_open(pid) {
        Ok(fd) => fd,
        // The member is gone.
        Err(rustix::io::Errno::SRCH) => return,
        Err(e) => {
            tracing::warn!(pid = member.pid, error = %e, "cannot open a pidfd of a member; it is not signalled");
            return;
        }
    };
    let fresh = std::fs::read(format!("/proc/{}/stat", member.pid))
        .ok()
        .and_then(|text| parse_stat(member.pid, &text));
    let Some(fresh) = fresh else {
        return;
    };
    if fresh.session != leader || fresh.start_time != member.start_time {
        return;
    }
    match super::leader::pidfd_send_signal(&fd, sig) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(e) => tracing::warn!(pid = member.pid, ?sig, error = %e, "cannot signal a member"),
    }
}

/// E2: ends every live member of the run's group and session while the
/// leader is a pinned zombie, and counts again. `killed` skips the graces
/// (a Kill stop); `escalate` becoming true skips them too. The result is
/// the final census (`None`: incomplete).
pub(super) fn exit_sweep(
    leader: u32,
    killed: bool,
    escalate: &watch::Receiver<bool>,
) -> Option<u32> {
    let (grace, term_grace) = if killed {
        (Duration::ZERO, Duration::ZERO)
    } else {
        (MEMBER_GRACE, MEMBER_TERM_GRACE)
    };
    // 1, 2
    reap_orphans_except(Some(leader));
    let mut last = pinned_members(leader);
    if members_remain(&last) {
        // 3: the members may end by themselves.
        last = wait_for_end(leader, grace, escalate, last);
    }
    if members_remain(&last) {
        // 4: SIGTERM, then a grace.
        signal_all(leader, &last, Signal::TERM);
        last = wait_for_end(leader, term_grace, escalate, last);
    }
    if members_remain(&last) {
        // 5: SIGKILL to every member of a fresh census.
        for _ in 0..KILL_ROUNDS {
            reap_orphans_except(Some(leader));
            let fresh = pinned_members(leader);
            if !members_remain(&fresh) {
                break;
            }
            signal_all(leader, &fresh, Signal::KILL);
            std::thread::sleep(KILL_PAUSE);
        }
    }
    // 6: the final census gives `descendants_remaining`.
    reap_orphans_except(Some(leader));
    exit_census(leader, true, &mut || reap_orphans_except(Some(leader)))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "server_exit_tests.rs"]
mod tests;
