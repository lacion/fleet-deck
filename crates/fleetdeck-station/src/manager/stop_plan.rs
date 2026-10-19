//! The stop plan of a lost run and the facts of its command (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.2 and §5.3).
//!
//! `session.execution_stop_plan` is a read. It takes the lock of the
//! session, reads the shim row, makes a complete inventory, applies the
//! witness rules ([`super::stop_attribution`]), opens a pidfd for each
//! target, reads each target again and stores the plan for 30 seconds. A
//! refused or empty plan is answered and not stored.

use std::time::Instant;

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::execution::{ExecutionCheck, ProcessInfo};
use fleetdeck_proto::execution_stop::{ExecutionStopPlan, ExecutionStopPlanQuery, code};
use fleetdeck_providers::host::shim::stop_inventory::{
    Inventory, Member, StatNow, StopInventoryError, UidsNow, comm_of, inventory, owned, stat_now,
    uids_now,
};
use tokio::sync::OwnedMutexGuard;

use super::stop_attribution::{
    InventoryFailure, Refused, Station, attribute, check_row, inventory_failure, run_identity,
};
use super::stop_decide::StopFacts;
use super::stop_signal::{self, OpenError};
use super::stop_state::{PlanTarget, StopPlan};
use super::{SessionManager, shims};
use crate::store::{Result, ShimRow, StoreError};

/// Why no answer exists.
#[derive(Debug)]
pub enum StopPlanError {
    /// No such session (`unknown_session`).
    UnknownSession,
    /// No such shim, or the shim of another session (`invalid_params`).
    UnknownShim,
    /// The platform or the kernel has no pidfd (`unsupported`).
    Unsupported,
    Store(StoreError),
}

impl From<StoreError> for StopPlanError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

fn infos(pids: &[u32]) -> Vec<ProcessInfo> {
    pids.iter()
        .map(|&pid| ProcessInfo {
            pid,
            command: comm_of(pid),
        })
        .collect()
}

/// The inventory of `leader` on a blocking thread.
async fn blocking_inventory(leader: u32) -> Result<Inventory, StopInventoryError> {
    tokio::task::spawn_blocking(move || inventory(leader))
        .await
        .map_err(|e| StopInventoryError::Incomplete(format!("the inventory did not run: {e}")))?
}

impl SessionManager {
    /// The current boot id; `None` when it cannot be read or is empty.
    fn stop_boot(&self) -> Option<String> {
        (self.boot_reader())().ok().filter(|b| !b.is_empty())
    }

    /// Before the decision of a `Resume`, a `ReconcileExecution` or a
    /// `StopExecution`: the lock of the session (rule 1: these operations
    /// and the plan never run together) and, for a stop, its facts.
    pub(super) async fn stop_prepare(
        &self,
        command: &SessionCommand,
    ) -> (Option<OwnedMutexGuard<()>>, Option<StopFacts>) {
        let (session, stop) = match command {
            SessionCommand::Resume { session_id }
            | SessionCommand::ReconcileExecution { session_id, .. } => (session_id, None),
            SessionCommand::StopExecution {
                session_id,
                shim_id,
                plan_id,
            } => (session_id, Some((shim_id, plan_id))),
            _ => return (None, None),
        };
        let guard = self.target.stops().locks.hold(session).await;
        let facts = stop.map(|(shim_id, plan_id)| StopFacts {
            plan_valid: self
                .target
                .stops()
                .plans
                .valid(session, shim_id, plan_id, Instant::now()),
            boot: self.stop_boot(),
            incarnation: self.store.incarnation().as_str().to_owned(),
        });
        (Some(guard), facts)
    }

    /// The `session.execution_stop_plan` read.
    ///
    /// # Errors
    /// [`StopPlanError`].
    pub async fn execution_stop_plan(
        &self,
        query: &ExecutionStopPlanQuery,
    ) -> std::result::Result<ExecutionStopPlan, StopPlanError> {
        if self
            .store
            .summary(query.session_id.clone())
            .await?
            .is_none()
        {
            return Err(StopPlanError::UnknownSession);
        }
        if !cfg!(target_os = "linux") {
            return Err(StopPlanError::Unsupported);
        }
        let _serial = self.target.stops().locks.hold(&query.session_id).await;
        let row = self
            .store
            .shim(query.shim_id.clone())
            .await?
            .filter(|row| row.session_id == query.session_id)
            .ok_or(StopPlanError::UnknownShim)?;
        let boot = self.boot_reader();
        let check = {
            let row = row.clone();
            let boot = std::sync::Arc::clone(&boot);
            tokio::task::spawn_blocking(move || shims::check_row(&boot, &row))
                .await
                .map_err(|e| StoreError::Corrupt(format!("the check did not run: {e}")))?
        };
        let check = self.add_last_stop(check).await;
        match self.build_plan(&row, &check).await? {
            Ok(plan) => Ok(plan),
            Err(refusal) => Ok(refusal.answer(check)),
        }
    }

    /// The plan of `row`, or the reason that it has none.
    async fn build_plan(
        &self,
        row: &ShimRow,
        check: &ExecutionCheck,
    ) -> std::result::Result<std::result::Result<ExecutionStopPlan, NoPlan>, StopPlanError> {
        let incarnation = self.store.incarnation().as_str().to_owned();
        let boot = self.stop_boot();
        let identity = match check_row(row, &incarnation, boot.as_deref()) {
            Ok(identity) => identity,
            Err(refused) => return Ok(Err(NoPlan::refused(&refused))),
        };
        let inv = match blocking_inventory(identity.pid).await {
            Ok(inv) => inv,
            Err(e) => {
                return match inventory_failure(&e) {
                    InventoryFailure::Unsupported => Err(StopPlanError::Unsupported),
                    InventoryFailure::Refused(refused) => Ok(Err(NoPlan::refused(&refused))),
                };
            }
        };
        let witnesses = self.store.execution_witnesses(row.shim_id.clone()).await?;
        let attribution = match attribute(row, &identity, &inv, &witnesses, Station::current()) {
            Ok(a) => a,
            Err(refused) => return Ok(Err(NoPlan::refused(&refused))),
        };
        if attribution.rule_a {
            self.store
                .execution_witnesses_record(
                    row.shim_id.clone(),
                    identity.boot_id.clone(),
                    attribution.domain.clone(),
                )
                .await?;
        }
        if attribution.targets.is_empty() {
            return Ok(Err(NoPlan::empty()));
        }
        let mut targets = Vec::new();
        for member in &attribution.targets {
            let fd = match stop_signal::open(member.pid) {
                Ok(fd) => fd,
                Err(OpenError::Gone) => return Ok(Err(NoPlan::code(code::PLAN_CHANGED))),
                Err(OpenError::Unsupported) => return Err(StopPlanError::Unsupported),
                Err(OpenError::Other(why)) => {
                    tracing::warn!(pid = member.pid, %why, "a pidfd cannot be opened");
                    return Ok(Err(NoPlan::code(code::INSPECTION_INCOMPLETE)));
                }
            };
            // The pidfd now names the process that had this PID when it
            // was opened. The read after it proves that this process is
            // the one of the inventory, and that it is still in the domain.
            if let Some(no_plan) = same_process(member, identity.pid) {
                return Ok(Err(no_plan));
            }
            // The owner is read after the open too (rule 8): the process
            // may have changed its UIDs since the inventory.
            match uids_now(member.pid) {
                UidsNow::Uids(u) if owned(&u, Station::current().uid) => {}
                UidsNow::Uids(_) => return Ok(Err(NoPlan::code(code::PERMISSION_DENIED))),
                UidsNow::Gone => return Ok(Err(NoPlan::code(code::PLAN_CHANGED))),
                UidsNow::Unreadable(why) => {
                    tracing::warn!(pid = member.pid, %why, "the owner of a target cannot be read");
                    return Ok(Err(NoPlan::code(code::INSPECTION_INCOMPLETE)));
                }
            }
            // The PID could be reused between the two reads.
            if let Some(no_plan) = same_process(member, identity.pid) {
                return Ok(Err(no_plan));
            }
            targets.push(PlanTarget {
                pid: member.pid,
                start_time: member.start_time,
                fd,
            });
        }
        let listed: Vec<ProcessInfo> = infos(&targets.iter().map(|t| t.pid).collect::<Vec<_>>());
        let plan_id: String = self.new_id();
        let now_ms = self.store.now_ms();
        let plan = StopPlan::new(
            plan_id.clone(),
            (row.session_id.clone(), row.shim_id.clone(), identity.pid),
            targets,
            attribution.domain,
            (Instant::now(), now_ms),
        );
        let expires_at = plan.expires_at;
        if self.target.stops().plans.put(plan, Instant::now()).is_err() {
            return Ok(Err(NoPlan::code(code::INSPECTION_INCOMPLETE)));
        }
        Ok(Ok(ExecutionStopPlan {
            plan_id,
            expires_at,
            eligible: true,
            code: None,
            targets: listed,
            escaped: check.escaped.clone(),
            check: check.clone(),
        }))
    }

    /// Fills `last_stop` of `check`: the newest settled stop of its shim.
    pub async fn add_last_stop(&self, mut check: ExecutionCheck) -> ExecutionCheck {
        if let Some(shim_id) = check.shim_id.clone() {
            match self.store.execution_stop_last(shim_id).await {
                Ok(row) => check.last_stop = row.and_then(|r| r.outcome()),
                Err(e) => tracing::warn!(error = %e, "the last stop cannot be read"),
            }
        }
        check
    }

    /// Records the witnesses of the newest unresolved run of `row`'s
    /// session when rule (a) holds (the leader is alive). It changes
    /// nothing else and never fails the caller.
    pub(super) async fn record_witnesses(&self, row: &ShimRow) {
        let incarnation = self.store.incarnation().as_str().to_owned();
        let Ok(identity) = run_identity(row, &incarnation, self.stop_boot().as_deref()) else {
            return;
        };
        let inv = match blocking_inventory(identity.pid).await {
            Ok(inv) => inv,
            Err(e) => {
                tracing::debug!(error = %e, "no witnesses: the inventory is incomplete");
                return;
            }
        };
        let Ok(attribution) = attribute(row, &identity, &inv, &[], Station::current()) else {
            return;
        };
        if !attribution.rule_a {
            return;
        }
        if let Err(e) = self
            .store
            .execution_witnesses_record(row.shim_id.clone(), identity.boot_id, attribution.domain)
            .await
        {
            tracing::warn!(error = %e, "the witnesses are not stored");
        }
    }
}

/// Reads `stat` of the target now: `None` when it is the process of the
/// inventory (same start time), live, and still in the domain of `leader`.
fn same_process(member: &Member, leader: u32) -> Option<NoPlan> {
    match stat_now(member.pid) {
        StatNow::Alive(s)
            if s.start_time == member.start_time
                && !s.is_zombie()
                && (s.pgrp == leader || s.session == leader) =>
        {
            None
        }
        StatNow::Unreadable(why) => {
            tracing::warn!(pid = member.pid, %why, "a target cannot be read");
            Some(NoPlan::code(code::INSPECTION_INCOMPLETE))
        }
        _ => Some(NoPlan::code(code::PLAN_CHANGED)),
    }
}

/// A plan that is not stored: a code, or nothing to stop.
struct NoPlan {
    code: Option<&'static str>,
}

impl NoPlan {
    fn code(code: &'static str) -> Self {
        Self { code: Some(code) }
    }

    fn empty() -> Self {
        Self { code: None }
    }

    fn refused(refused: &Refused) -> Self {
        tracing::info!(code = refused.code, why = %refused.why, "a stop plan is refused");
        Self::code(refused.code)
    }

    /// The answer: no plan id, no targets, the escaped list and the check.
    fn answer(self, check: ExecutionCheck) -> ExecutionStopPlan {
        ExecutionStopPlan {
            plan_id: String::new(),
            expires_at: 0,
            eligible: false,
            code: self.code.map(str::to_owned),
            targets: Vec::new(),
            escaped: check.escaped.clone(),
            check,
        }
    }
}

#[cfg(test)]
#[path = "stop_command_tests.rs"]
mod command_tests;
