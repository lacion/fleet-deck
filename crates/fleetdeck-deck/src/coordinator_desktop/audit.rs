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

use crate::store::coordinator_desktop::{Charges, DesktopOp, OpTool};
use crate::store::{AuditResult, DeckStore, NewAudit};

/// The stage of the row written when an operation is accepted.
pub(super) const ATTEMPT: &str = "attempt";
/// The stage of the row written when an operation ends, and of the single row
/// of a query.
pub(super) const OUTCOME: &str = "outcome";
/// The stage of the row of a refusal.
pub(super) const REFUSED: &str = "refused";

/// The audit action of a coordinator screenshot (§12).
pub(super) const CAPTURE: &str = "desktop.capture";

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
        self.row_with(stage, result, code, charged, &[])
    }

    /// As [`Self::row`], with more fixed keys (`extra`: a key and a value).
    /// A screenshot row (`desktop.capture`) also names the capture: the
    /// capture ID is the operation ID.
    pub(super) fn row_with(
        &self,
        stage: &'static str,
        result: AuditResult,
        code: Option<&str>,
        charged: Option<Charges>,
        extra: &[(&'static str, Value)],
    ) -> NewAudit {
        let mut detail = Map::new();
        detail.insert("grant_id".into(), json!(self.ctx.grant_id));
        detail.insert("generation".into(), json!(self.ctx.generation));
        detail.insert("caller_station".into(), json!(self.ctx.caller_station));
        detail.insert("caller_session".into(), json!(self.ctx.caller_session));
        detail.insert("target_station".into(), json!(self.target));
        detail.insert("operation_id".into(), json!(self.operation_id));
        if self.action == CAPTURE {
            detail.insert("capture_id".into(), json!(self.operation_id));
        }
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
        for (key, value) in extra {
            detail.insert((*key).into(), value.clone());
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

/// The extra keys of a recording row (R13 P5b-2 §15): the kind, the
/// requested seconds and the counters that the acceptance charged.
pub(super) fn recording_extras(op: &DesktopOp) -> Vec<(&'static str, Value)> {
    let seconds = op.seconds.unwrap_or(0);
    vec![
        ("kind", json!("recording")),
        ("seconds", json!(seconds)),
        (
            "charged",
            json!({
                "desktop": op.charged.desktop,
                "capture": op.charged.capture,
                "recording": u32::from(op.tool == OpTool::RecordMonitor),
                "recording_seconds": seconds,
            }),
        ),
    ]
}

/// What the recording module writes for a coordinator recording (the
/// started, outcome and indexing rows of `desktop_recording`).
pub(crate) struct RecordingStage<'a> {
    pub capture_id: &'a str,
    pub monitor: &'a str,
    pub seconds: Option<u32>,
    /// `started`, `outcome` or `indexing_failed`.
    pub stage: &'static str,
    /// `ok` or a failure code.
    pub code: &'a str,
    pub stop_reason: Option<&'a str>,
    pub artifact: Option<&'a str>,
}

/// Writes one `desktop.capture` row of kind `recording` of a coordinator
/// recording: actor `coordinator`, the caller and the grant of the stored
/// `coordinator` context. A context that does not parse writes no row and
/// logs the failure.
pub(crate) async fn recording_stage(
    store: &DeckStore,
    (coordinator, target): (&str, &StationId),
    result: AuditResult,
    f: &RecordingStage<'_>,
) {
    let Ok(ctx) = serde_json::from_str::<CoordinatorContext>(coordinator) else {
        tracing::warn!("coordinator recording audit: the stored context does not parse");
        return;
    };
    let facts = Facts {
        action: CAPTURE,
        ctx: &ctx,
        target,
        operation_id: Some(f.capture_id),
        window_id: None,
        monitor: Some(f.monitor),
    };
    let mut extra: Vec<(&'static str, Value)> = vec![("kind", json!("recording"))];
    if let Some(seconds) = f.seconds {
        extra.push(("seconds", json!(seconds)));
    }
    if let Some(reason) = f.stop_reason {
        extra.push(("stop_reason", json!(reason)));
    }
    if let Some(artifact) = f.artifact {
        extra.push(("artifact_id", json!(artifact)));
    }
    let code = (f.code != "ok").then_some(f.code);
    let row = facts.row_with(f.stage, result, code, None, &extra);
    write_or_log(store, row).await;
}
