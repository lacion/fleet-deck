//! Which processes belong to a lost run (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.3 rules 2 to 7 and the witness rules).
//!
//! An equal PGID or SID alone never proves that a process belongs to the
//! run (rule 5). The station recorded the identity of the leader (PID,
//! start time, boot id) when the shim started the provider:
//!
//! - **(a) The leader is alive** (that identity, still the leader of its
//!   group and its session): every member of the domain that the station
//!   user owns is a target, and the station records every member as a
//!   witness.
//! - **(b) The leader is gone** (a zombie counts as gone): a member is a
//!   target only when the station recorded it as a witness (PID, start
//!   time, boot id) while the leader was alive, and it is still in the
//!   domain. A witness authorizes only itself.
//!
//! PID 1, the station, its ancestors and the shim are never in the domain
//! of a plan.

use fleetdeck_proto::execution_stop::code;
use fleetdeck_providers::host::shim::stop_inventory::{Inventory, Member, StopInventoryError};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;

use super::stop_state::MAX_TARGETS;
use crate::store::ShimRow;
use crate::store::execution_stops::Witness;
use crate::store::resumes::proves_end;

/// Why a plan or a stop is refused: a code of the board and a reason for
/// the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub why: String,
}

impl Refused {
    fn new(code: &'static str, why: impl Into<String>) -> Self {
        Self {
            code,
            why: why.into(),
        }
    }

    fn unproven(why: impl Into<String>) -> Self {
        Self::new(code::IDENTITY_UNPROVEN, why)
    }
}

/// What the station knows about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Station {
    pub pid: u32,
    /// The real UID of the station process.
    pub uid: u32,
}

impl Station {
    /// The facts of this process.
    #[must_use]
    pub fn current() -> Self {
        Self {
            pid: std::process::id(),
            uid: rustix::process::getuid().as_raw(),
        }
    }
}

/// The row rules of rule 2: the exact unresolved shim row of this database
/// incarnation, on the boot of its run, with the identity of its leader.
/// Returns the identity.
///
/// # Errors
/// `identity_unproven` with the reason.
pub fn check_row(
    row: &ShimRow,
    incarnation: &str,
    boot: Option<&str>,
) -> Result<ProviderIdentity, Refused> {
    if row.state.is_open() {
        return Err(Refused::unproven("the shim of the run is still managed"));
    }
    if proves_end(row) {
        return Err(Refused::unproven("the end of the run is already proven"));
    }
    run_identity(row, incarnation, boot)
}

/// The leader identity of `row` when its database incarnation, its boot and
/// its recorded identity agree. The state of the row is not checked: the
/// witnesses of a run are recorded in any state.
///
/// # Errors
/// `identity_unproven` with the reason.
pub fn run_identity(
    row: &ShimRow,
    incarnation: &str,
    boot: Option<&str>,
) -> Result<ProviderIdentity, Refused> {
    if row.incarnation != incarnation {
        return Err(Refused::unproven("the run belongs to another database"));
    }
    let Some(boot) = boot.filter(|b| !b.is_empty()) else {
        return Err(Refused::unproven("the boot id cannot be read"));
    };
    let Some(identity) = row.provider_identity.as_ref() else {
        return Err(Refused::unproven("no leader identity was recorded"));
    };
    if row.launch_boot_id.as_deref() != Some(boot) || identity.boot_id != boot {
        return Err(Refused::unproven("the run belongs to another boot"));
    }
    Ok(identity.clone())
}

/// What a failed inventory means for a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryFailure {
    /// The platform has no reader.
    Unsupported,
    /// A read failed or the two passes differ: `inspection_incomplete`.
    Refused(Refused),
}

/// The meaning of a failed inventory: an incomplete one (a failed read, two
/// passes that differ) refuses the plan with `inspection_incomplete`.
#[must_use]
pub fn inventory_failure(error: &StopInventoryError) -> InventoryFailure {
    match error {
        StopInventoryError::Unsupported => InventoryFailure::Unsupported,
        StopInventoryError::Incomplete(why) => {
            InventoryFailure::Refused(Refused::new(code::INSPECTION_INCOMPLETE, why.clone()))
        }
    }
}

/// The result of an attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// Rule (a) holds: the leader is alive, and `domain` is to be recorded
    /// as witnesses.
    pub rule_a: bool,
    /// The processes that a stop may signal, sorted by PID.
    pub targets: Vec<Member>,
    /// Every live member of the domain, as `(pid, start_time)`.
    pub domain: Vec<(u32, u64)>,
}

/// Applies the witness rules to a complete inventory.
///
/// # Errors
/// `identity_unproven` (a leader that is not the leader of both its group
/// and session, a protected member, members without a witness),
/// `permission_denied` (a candidate owned by another UID) and
/// `inspection_incomplete` (more than [`MAX_TARGETS`] targets).
pub fn attribute(
    row: &ShimRow,
    identity: &ProviderIdentity,
    inv: &Inventory,
    witnesses: &[Witness],
    station: Station,
) -> Result<Attribution, Refused> {
    let leader_alive = match &inv.leader_stat {
        Some(s) if s.start_time == identity.start_time && !s.is_zombie() => {
            if s.pgrp != identity.pid || s.session != identity.pid {
                return Err(Refused::unproven(
                    "the leader is not the leader of its group and session",
                ));
            }
            true
        }
        _ => false,
    };
    let mut protected = vec![1, station.pid];
    protected.extend(inv.ancestors(station.pid));
    protected.extend(row.shim_pid);
    if let Some(m) = inv.domain.iter().find(|m| protected.contains(&m.pid)) {
        return Err(Refused::unproven(format!(
            "the protected process {} is in the domain",
            m.pid
        )));
    }
    let is_witness = |m: &Member| {
        witnesses.iter().any(|w| {
            w.pid == m.pid && w.start_time == m.start_time && w.boot_id == identity.boot_id
        })
    };
    let targets: Vec<Member> = inv
        .domain
        .iter()
        .filter(|m| leader_alive || is_witness(m))
        .cloned()
        .collect();
    if let Some(m) = targets.iter().find(|m| m.uid != station.uid) {
        return Err(Refused::new(
            code::PERMISSION_DENIED,
            format!("process {} belongs to another user", m.pid),
        ));
    }
    if targets.is_empty() && !inv.domain.is_empty() {
        return Err(Refused::unproven(
            "the leader is gone and no member is a witness",
        ));
    }
    if targets.len() > MAX_TARGETS {
        return Err(Refused::new(
            code::INSPECTION_INCOMPLETE,
            format!("more than {MAX_TARGETS} targets"),
        ));
    }
    Ok(Attribution {
        rule_a: leader_alive,
        targets,
        domain: inv.domain.iter().map(|m| (m.pid, m.start_time)).collect(),
    })
}

#[cfg(test)]
#[path = "stop_attribution_tests.rs"]
mod tests;
