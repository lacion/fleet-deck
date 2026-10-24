//! The binding projection of a snapshot, moved out of `bindings.rs`; no
//! behaviour change.

use std::collections::HashSet;

use super::BindingState;
use crate::store::session_grants::{GrantRecord, RecordState};

impl super::Bindings {
    /// The binding projection of a coordinator-capable snapshot (ADR-006
    /// §6.1): every binding with its fields and lifecycle state, sorted by
    /// binding id. No secret.
    #[must_use]
    pub fn projection(&self) -> Vec<fleetdeck_proto::CoordinatorBindingState> {
        self.projection_with(&[])
    }

    /// The projection that follows the records of the sessions (MCP P4b §5):
    /// a dead credential of an `Attached` record is left out, and an `Ended`
    /// record that memory does not hold gives a synthetic `Revoked` entry
    /// (`Expired` for the reason `expired`). A grant without a record is
    /// projected as by [`Self::projection`].
    #[must_use]
    pub fn projection_with(
        &self,
        records: &[GrantRecord],
    ) -> Vec<fleetdeck_proto::CoordinatorBindingState> {
        use fleetdeck_proto::CoordinatorBindingPhase as Phase;
        use fleetdeck_proto::CoordinatorBindingState as Row;
        let id = |r: &GrantRecord| format!("{}:{}", r.session_id, r.grant_id);
        let attached: HashSet<String> = records
            .iter()
            .filter(|r| r.state == RecordState::Attached)
            .map(id)
            .collect();
        let map = self.lock();
        let mut out: Vec<Row> = map
            .values()
            .map(|e| &e.binding)
            .filter(|b| !b.is_fleet_only())
            .filter(|b| {
                !(attached.contains(&b.binding_id)
                    && matches!(b.state, BindingState::Revoked | BindingState::Expired))
            })
            .map(|b| Row {
                binding_id: b.binding_id.clone(),
                grant_id: b.grant_id.clone(),
                session_id: b.session_id.clone(),
                generation: b.generation,
                provider_instance: b.provider_instance.clone(),
                native_id: b.native_id.clone(),
                state: match b.state {
                    BindingState::Pending => Phase::Pending,
                    BindingState::Active => Phase::Active,
                    BindingState::Expired => Phase::Expired,
                    BindingState::Revoked => Phase::Revoked,
                },
            })
            .collect();
        for r in records.iter().filter(|r| r.state == RecordState::Ended) {
            if map.values().any(|e| e.binding.binding_id == id(r)) {
                continue;
            }
            out.push(Row {
                binding_id: id(r),
                grant_id: r.grant_id.clone(),
                session_id: r.session_id.clone(),
                generation: r.generation,
                provider_instance: String::new(),
                native_id: None,
                state: if r.ended_reason.as_deref() == Some("expired") {
                    Phase::Expired
                } else {
                    Phase::Revoked
                },
            });
        }
        out.sort_by(|a, b| a.binding_id.cmp(&b.binding_id));
        out
    }
}
