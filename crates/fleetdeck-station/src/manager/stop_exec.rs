//! The signalling of a stop plan (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.3 rules 10 to 12). It runs after the
//! row, the boot and the plan were checked:
//!
//! 1. A fresh inventory: a live member of the domain that the plan did not
//!    list gives `processes_remain` before any signal. An incomplete
//!    inventory gives `inspection_incomplete`.
//! 2. Each target is read again (start time, state, group, session). A
//!    process that ended or reused the PID is not signalled. A process that
//!    left the domain is skipped and counted. Then the four UIDs of each
//!    live target are read, and the targets are read once more. A target
//!    that another user owns now fails the whole stop with
//!    `permission_denied` before any signal.
//! 3. SIGTERM to each live target through its own pidfd, a wait of five
//!    seconds on the monotonic clock (it ends when all are dead), then
//!    SIGKILL through the same pidfds to the targets that still live. The
//!    four UIDs are read again before each SIGKILL: a target that another
//!    user owns now gets no SIGKILL and is counted as denied.
//! 4. A fresh inventory. Members that remain: `processes_remain`.
//!
//! The whole run takes at most fifteen seconds. A run that does not finish
//! is `unknown`. A refused signal (`EPERM`) makes the result `failed` with
//! `permission_denied`, also when other members remain.

use std::time::Duration;

use fleetdeck_proto::execution_stop::{StopOutcome, code};
use fleetdeck_providers::host::shim::inspect::ProcStat;
use fleetdeck_providers::host::shim::stop_inventory::{
    Inventory, StatNow, StopInventoryError, UidsNow, inventory, owned, stat_now, uids_now,
};
use tokio::time::{Instant, sleep, timeout_at};

use super::stop_signal::{Sent, Which, send};
use super::stop_state::{PlanTarget, StopPlan};
use crate::store::execution_stops::StopResult;

/// The wait between SIGTERM and SIGKILL.
pub const TERM_WAIT: Duration = Duration::from_secs(5);
/// The longest wait for the SIGKILL to take effect.
pub const KILL_WAIT: Duration = Duration::from_secs(2);
/// The longest run of one stop.
pub const TOTAL: Duration = Duration::from_secs(15);
/// The pause between two reads while the station waits for exits.
const POLL: Duration = Duration::from_millis(50);

/// The signal sender; tests replace it.
pub type SendFn = fn(&std::os::fd::OwnedFd, Which) -> Sent;

/// The reader of the UIDs of a PID; tests replace it.
pub type UidsFn = fn(u32) -> UidsNow;

/// The owner of every target: the real UID of the station.
fn owner_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[derive(Debug, Default)]
struct Counts {
    term_sent: u32,
    kill_sent: u32,
    skipped: u32,
    denied: bool,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn in_domain(stat: &ProcStat, leader: u32) -> bool {
    stat.pgrp == leader || stat.session == leader
}

/// True when the target of the plan is dead: gone, a zombie, or another
/// process that has its PID now.
fn dead(t: &PlanTarget) -> bool {
    match stat_now(t.pid) {
        StatNow::Gone => true,
        StatNow::Alive(s) => s.start_time != t.start_time || s.is_zombie(),
        StatNow::Unreadable(_) => false,
    }
}

/// True when the target of the plan lives, is still the same process and is
/// still in the domain.
fn live_in_domain(t: &PlanTarget, leader: u32) -> bool {
    matches!(
        stat_now(t.pid),
        StatNow::Alive(s) if s.start_time == t.start_time && !s.is_zombie() && in_domain(&s, leader)
    )
}

async fn fresh_inventory(leader: u32) -> Result<Inventory, StopInventoryError> {
    tokio::task::spawn_blocking(move || inventory(leader))
        .await
        .map_err(|e| StopInventoryError::Incomplete(format!("the inventory did not run: {e}")))?
}

async fn wait_dead(targets: &[&PlanTarget], longest: Duration) {
    let until = Instant::now() + longest;
    while !targets.iter().all(|t| dead(t)) && Instant::now() < until {
        sleep(POLL).await;
    }
}

fn settled(counts: &Counts, gone: usize, remaining: bool) -> StopResult {
    let (state, code) = if counts.denied {
        (StopOutcome::Failed, Some(code::PERMISSION_DENIED))
    } else if remaining {
        (StopOutcome::Done, Some(code::PROCESSES_REMAIN))
    } else {
        (StopOutcome::Done, None)
    };
    StopResult {
        state,
        code,
        term_sent: counts.term_sent,
        kill_sent: counts.kill_sent,
        gone: count(gone),
        skipped: counts.skipped,
    }
}

/// Signals the targets of `plan` as described in the module documentation.
/// `send` is [`send`] outside tests.
pub async fn execute(plan: &StopPlan, send_signal: SendFn, uids: UidsFn) -> StopResult {
    let mut counts = Counts::default();
    match timeout_at(
        Instant::now() + TOTAL,
        run(plan, send_signal, uids, &mut counts),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(plan = %plan.plan_id, "the stop did not finish in time");
            let gone = plan.targets.iter().filter(|t| dead(t)).count();
            StopResult {
                code: Some(code::OUTCOME_UNKNOWN),
                state: StopOutcome::Unknown,
                ..settled(&counts, gone, true)
            }
        }
    }
}

/// Records the answer of one signal; returns true when it was sent.
fn record(counts: &mut Counts, pid: u32, which: Which, sent: Sent) -> bool {
    match sent {
        Sent::Delivered => {
            match which {
                Which::Term => counts.term_sent += 1,
                Which::Kill => counts.kill_sent += 1,
            }
            true
        }
        // The process ended; the wait and the final read count it as gone.
        Sent::Gone => false,
        Sent::Denied => {
            counts.denied = true;
            false
        }
        Sent::Failed(why) => {
            tracing::warn!(pid, %why, "a signal of the stop failed");
            false
        }
    }
}

/// The checks before any SIGTERM: the owner of each live target, then the
/// target itself once more. `Err` is the result of the stop.
fn checked<'a>(
    live: Vec<&'a PlanTarget>,
    leader: u32,
    uids: UidsFn,
    counts: &mut Counts,
) -> Result<Vec<&'a PlanTarget>, StopResult> {
    let owner = owner_uid();
    let mut owned_now: Vec<&PlanTarget> = Vec::new();
    for t in live {
        match uids(t.pid) {
            UidsNow::Uids(u) if owned(&u, owner) => owned_now.push(t),
            UidsNow::Uids(_) => return Err(StopResult::failed(code::PERMISSION_DENIED)),
            UidsNow::Gone => {}
            UidsNow::Unreadable(why) => {
                tracing::warn!(pid = t.pid, %why, "the owner of a target cannot be read");
                return Err(StopResult::failed(code::INSPECTION_INCOMPLETE));
            }
        }
    }
    let mut kept: Vec<&PlanTarget> = Vec::new();
    for t in owned_now {
        match stat_now(t.pid) {
            StatNow::Alive(s) if s.start_time == t.start_time && !s.is_zombie() => {
                if in_domain(&s, leader) {
                    kept.push(t);
                } else {
                    counts.skipped += 1;
                }
            }
            StatNow::Alive(_) | StatNow::Gone => {}
            StatNow::Unreadable(why) => {
                tracing::warn!(pid = t.pid, %why, "a target cannot be read");
                return Err(StopResult::failed(code::INSPECTION_INCOMPLETE));
            }
        }
    }
    Ok(kept)
}

async fn run(
    plan: &StopPlan,
    send_signal: SendFn,
    uids: UidsFn,
    counts: &mut Counts,
) -> StopResult {
    let leader = plan.leader;
    let before = match fresh_inventory(leader).await {
        Ok(inv) => inv,
        Err(e) => {
            tracing::warn!(error = %e, "the inventory before the stop is incomplete");
            return StopResult::failed(code::INSPECTION_INCOMPLETE);
        }
    };
    if before
        .domain
        .iter()
        .any(|m| !plan.domain.contains(&(m.pid, m.start_time)))
    {
        return StopResult::failed(code::PROCESSES_REMAIN);
    }
    let mut live: Vec<&PlanTarget> = Vec::new();
    for t in &plan.targets {
        match stat_now(t.pid) {
            StatNow::Alive(s) if s.start_time == t.start_time && !s.is_zombie() => {
                if in_domain(&s, leader) {
                    live.push(t);
                } else {
                    counts.skipped += 1;
                }
            }
            // It ended, or another process reused the PID: the pidfd is
            // never used for it.
            StatNow::Alive(_) | StatNow::Gone => {}
            StatNow::Unreadable(why) => {
                tracing::warn!(pid = t.pid, %why, "a target cannot be read");
                return StopResult::failed(code::INSPECTION_INCOMPLETE);
            }
        }
    }
    let live = match checked(live, leader, uids, counts) {
        Ok(live) => live,
        Err(result) => return result,
    };
    let mut waiting: Vec<&PlanTarget> = Vec::new();
    for t in &live {
        let sent = send_signal(&t.fd, Which::Term);
        if record(counts, t.pid, Which::Term, sent) {
            waiting.push(t);
        }
    }
    wait_dead(&waiting, TERM_WAIT).await;
    let mut waiting: Vec<&PlanTarget> = Vec::new();
    for t in &live {
        if !live_in_domain(t, leader) {
            continue;
        }
        // The owner is read again: a process that changed its UIDs since
        // the SIGTERM gets no SIGKILL.
        match uids(t.pid) {
            UidsNow::Uids(u) if owned(&u, owner_uid()) => {
                let sent = send_signal(&t.fd, Which::Kill);
                if record(counts, t.pid, Which::Kill, sent) {
                    waiting.push(t);
                }
            }
            UidsNow::Uids(_) => {
                counts.denied = true;
                counts.skipped += 1;
            }
            UidsNow::Gone => {}
            UidsNow::Unreadable(why) => {
                tracing::warn!(pid = t.pid, %why, "the owner of a target cannot be read");
                counts.skipped += 1;
            }
        }
    }
    wait_dead(&waiting, KILL_WAIT).await;
    let gone = plan.targets.iter().filter(|t| dead(t)).count();
    match fresh_inventory(leader).await {
        Ok(after) => settled(counts, gone, !after.domain.is_empty()),
        Err(e) => {
            tracing::warn!(error = %e, "the inventory after the stop is incomplete");
            StopResult {
                code: Some(code::OUTCOME_UNKNOWN),
                state: StopOutcome::Unknown,
                ..settled(counts, gone, true)
            }
        }
    }
}

/// The real signal sender.
#[must_use]
pub fn real_send() -> SendFn {
    send
}

/// The real reader of the UIDs, over `/proc`.
#[must_use]
pub fn real_uids() -> UidsFn {
    uids_now
}

#[cfg(test)]
#[path = "stop_exec_tests.rs"]
mod tests;
