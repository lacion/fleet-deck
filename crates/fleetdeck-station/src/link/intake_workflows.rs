//! `session.workflows` and `session.workflow_agent` (B12): the workflow
//! projection of a session in pages, and the transcript of one workflow
//! agent. Error codes of `session.workflows`: `invalid_params`,
//! `unknown_session`, `internal`; of `session.workflow_agent`:
//! `invalid_params`, `unknown_session`, `busy`, `internal`.

use fleetdeck_proto::event::WorkflowsQuery;
use fleetdeck_proto::history::code;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::subagent::WorkflowAgentQuery;

use super::StationLink;
use super::{bad_params, ok};
use crate::workflows::read;

/// `session.workflows`.
pub(super) async fn workflows_method(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let query: WorkflowsQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    match ctx
        .manager
        .store()
        .workflow_page(query.session_id, query.after)
        .await
    {
        Ok(Some(page)) => ok(page),
        Ok(None) => Outcome::Err(WireError::new(code::UNKNOWN_SESSION, "no such session")),
        Err(e) => Outcome::Err(WireError::new("internal", e.to_string())),
    }
}

/// `session.workflow_agent`.
pub(super) async fn workflow_agent_method(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let query: WorkflowAgentQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let store = ctx.manager.store();
    let private = ctx.manager.private_paths();
    match read::answer(store, store.history(), &private, query).await {
        Ok(answer) => ok(answer),
        Err(e) => Outcome::Err(WireError::new(e.code, e.message)),
    }
}
