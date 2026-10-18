//! The audit rows of the coordinator desktop tools (R13 P6 §12, decision
//! O12-9 item 24). Actor `coordinator`; action `desktop.<tool>`; target the
//! operation ID (the grant ID for `desktop_info` and the status tool, and for
//! a refusal before an operation exists); detail a compact JSON with fixed
//! keys. A row names only deck-made identifiers and fixed codes: never a
//! credential, an authority, a raw request ID, a URL, a title, a body or a
//! path.
//!
//! The attempt row is written right after the admission; the outcome row only
//! when `desktop_op_finish` made the first transition, so a stage is written
//! once for one operation. A refusal after the authorization passed has its own
//! row (result `refused`).

use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use serde_json::{Map, Value, json};

use crate::store::coordinator_desktop::Charges;
use crate::store::{AuditResult, DeckStore, NewAudit};

/// The stage of the row written when an operation is accepted.
pub(super) const ATTEMPT: &str = "attempt";
/// The stage of the row written when an operation ends, and of the single row
/// of a query.
pub(super) const OUTCOME: &str = "outcome";
/// The stage of the row of a refusal.
pub(super) const REFUSED: &str = "refused";

/// The facts of one row.
pub(super) struct Facts<'a> {
    pub action: &'static str,
    pub ctx: &'a CoordinatorContext,
    pub target: &'a StationId,
    /// `None` before an operation exists, and for a query.
    pub operation_id: Option<&'a str>,
    /// The validated window handle of a close.
    pub window_id: Option<&'a str>,
    /// The validated monitor of a preview.
    pub monitor: Option<&'a str>,
}

impl Facts<'_> {
    /// The audit row for `stage`: the target is the operation ID, else the
    /// grant ID.
    pub(super) fn row(
        &self,
        stage: &'static str,
        result: AuditResult,
        code: Option<&str>,
        charged: Option<Charges>,
    ) -> NewAudit {
        let mut detail = Map::new();
        detail.insert("grant_id".into(), json!(self.ctx.grant_id));
        detail.insert("generation".into(), json!(self.ctx.generation));
        detail.insert("caller_station".into(), json!(self.ctx.caller_station));
        detail.insert("caller_session".into(), json!(self.ctx.caller_session));
        detail.insert("target_station".into(), json!(self.target));
        detail.insert("operation_id".into(), json!(self.operation_id));
        if let Some(window_id) = self.window_id {
            detail.insert("window_id".into(), json!(window_id));
        }
        if let Some(monitor) = self.monitor {
            detail.insert("monitor".into(), json!(monitor));
        }
        detail.insert("stage".into(), json!(stage));
        detail.insert("code".into(), json!(code));
        if let Some(charged) = charged {
            detail.insert(
                "charged".into(),
                json!({"desktop": charged.desktop, "capture": charged.capture}),
            );
        }
        let target = self
            .operation_id
            .map_or_else(|| self.ctx.grant_id.clone(), str::to_owned);
        NewAudit::new("coordinator", self.action, result)
            .target(target)
            .detail(Value::Object(detail).to_string())
    }
}

/// Writes a row whose loss does not change the answer: a failure is logged.
pub(super) async fn write_or_log(store: &DeckStore, row: NewAudit) {
    store.audit_or_log(row).await;
}

/// Writes the row that an operation needs before it starts (§12: no
/// operation starts without its decision record).
///
/// # Errors
/// The store error; the caller must not send.
pub(super) async fn write_required(
    store: &DeckStore,
    row: NewAudit,
) -> Result<(), crate::store::DeckError> {
    store.audit(row).await
}
