//! `session.execution_reconciled` and its optional `by` (DL-432).

use super::*;
use serde_json::json;

#[test]
fn execution_reconciled_reads_without_by_and_with_an_unknown_by() {
    let read = |v: serde_json::Value| serde_json::from_value::<EventBody>(v).unwrap();
    let kind = "session.execution_reconciled";
    let old = read(json!({"kind": kind, "shim_id": "s"}));
    assert!(matches!(
        old.known(),
        Some(Event::ExecutionReconciled { by: None, .. })
    ));
    let new = read(json!({"kind": kind, "shim_id": "s", "by": "robot"}));
    assert!(matches!(
        new.known(),
        Some(Event::ExecutionReconciled {
            by: Some(ReconciledBy::Unknown),
            ..
        })
    ));
    let owner = Event::ExecutionReconciled {
        shim_id: "s".into(),
        by: None,
    };
    let owner = serde_json::to_value(owner).unwrap();
    assert_eq!(owner, json!({"kind": kind, "shim_id": "s"}));
}
