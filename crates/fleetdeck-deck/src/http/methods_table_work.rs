//! The rows of the work-around-a-session routes (split from
//! `methods_table_rest.rs` for the module size rule): the subagent log, the
//! branches and the workflows.

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, m};

/// The work rows; [`super::api`] joins them with the others.
pub(super) const API_WORK: &[ApiMethod] = &[
    // The work around a session (DL-191): the owner only, never a member.
    m(
        "sessions.subagent",
        Method::GET,
        "/api/sessions/{station}/{session}/subagents/{tool_use_id}",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.branches",
        Method::GET,
        "/api/sessions/{station}/{session}/branches",
        Some(Scope::Read),
        false,
    ),
    m(
        "sessions.branch_diff",
        Method::GET,
        "/api/sessions/{station}/{session}/branches/{head}/diff",
        Some(Scope::Read),
        false,
    ),
    // The workflows of a session (B12): the page holds what the events of the
    // session hold, so a member of the session reads it; the chat of one
    // workflow agent is the owner only.
    m(
        "sessions.workflows",
        Method::GET,
        "/api/sessions/{station}/{session}/workflows",
        Some(Scope::Read),
        false,
    )
    .member(),
    m(
        "sessions.workflow_agent",
        Method::GET,
        "/api/sessions/{station}/{session}/workflow-agents/{agent_id}",
        Some(Scope::Read),
        false,
    ),
];
