//! The checks of the workflow routes that need no harness (B12): the page
//! cursor and the agent id.

use axum::extract::Query;
use axum::http::Uri;
use fleetdeck_proto::event::WorkflowsCursor;

use super::{WorkflowsParams, workflow_agent_id_ok, workflow_cursor};

fn cursor(query: &str) -> Result<Option<WorkflowsCursor>, String> {
    let uri: Uri = format!("/x{query}").parse().unwrap();
    workflow_cursor(Query::<WorkflowsParams>::try_from_uri(&uri)).map_err(|e| e.message)
}

#[test]
fn workflow_cursor_needs_both_values_or_none() {
    assert_eq!(cursor(""), Ok(None));
    assert_eq!(
        cursor("?after_order=7&after_index=3"),
        Ok(Some(WorkflowsCursor {
            run_order: 7,
            agent_index: 3
        }))
    );
    let invalid = Err("the page cursor is not valid".to_owned());
    assert_eq!(cursor("?after_order=7"), invalid.clone());
    assert_eq!(cursor("?after_index=3"), invalid.clone());
    assert_eq!(cursor("?after_order=x&after_index=3"), invalid.clone());
    assert_eq!(cursor("?after_order=7&after_index=-1"), invalid.clone());
    assert_eq!(
        cursor("?after_order=7&after_index=4294967296"),
        invalid.clone()
    );
    assert_eq!(cursor("?after_order=7&after_index=3&other=1"), invalid);
}

#[test]
fn workflow_agent_id_is_a_z0_9_1_to_64() {
    assert!(workflow_agent_id_ok("a"));
    assert!(workflow_agent_id_ok("a1b2c3"));
    assert!(workflow_agent_id_ok(&"a".repeat(64)));
    assert!(!workflow_agent_id_ok(""));
    assert!(!workflow_agent_id_ok(&"a".repeat(65)));
    assert!(!workflow_agent_id_ok("A"));
    assert!(!workflow_agent_id_ok("a-b"));
    assert!(!workflow_agent_id_ok("a_b"));
    assert!(!workflow_agent_id_ok("a.b"));
    assert!(!workflow_agent_id_ok("a/b"));
}
