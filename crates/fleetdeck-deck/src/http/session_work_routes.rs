//! The work around a session (DL-191, `docs/v2/design/subagents-worktrees.md`
//! 2.6): the log of one subagent and the branches of the session's
//! repository.
//!
//! - `GET /api/sessions/{station}/{session}/subagents/{tool_use_id}` forwards
//!   `session.subagent` and returns a `SubagentLog`.
//! - `GET /api/sessions/{station}/{session}/branches` forwards
//!   `session.branches` and returns `RepoBranches`.
//! - `GET /api/sessions/{station}/{session}/branches/{head}/diff` forwards
//!   `files.branch_diff` and returns a `SessionDiff`.
//! - `GET /api/sessions/{station}/{session}/workflows?after_order=&after_index=`
//!   forwards `session.workflows` and returns a `WorkflowsPage` (B12, design
//!   `docs/v2/design/workflow-agents.md` 4.5).
//! - `GET /api/sessions/{station}/{session}/workflow-agents/{agent_id}`
//!   forwards `session.workflow_agent` and returns a `SubagentLog`.
//!
//! Only the owner's board login reads them: a member and a bearer token get
//! 403 `owner_only`. The exception is the workflow page: the owner and the
//! members of the session read it (it holds what the events of the session
//! hold). The deck stores and caches nothing. The text of the station never
//! goes to the answer.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::branches::{
    BranchDiffQuery, BranchesQuery, RepoBranches, code as branch_code,
};
use fleetdeck_proto::event::{WorkflowsCursor, WorkflowsPage, WorkflowsQuery};
use fleetdeck_proto::files::SessionDiff;
use fleetdeck_proto::history::code;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::subagent::{SubagentLog, SubagentQuery, WorkflowAgentQuery};
use fleetdeck_proto::version::feature;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Board;
use super::access::{self, session_of};
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, LinkFeature, MemberAction, NewAudit, Principal};

/// The longest the deck waits for the station's answer. The station's own
/// budget is 10 s.
const WORK_WAIT: Duration = Duration::from_secs(15);
/// The longest session id of a request, in characters.
const MAX_SESSION_CHARS: usize = 128;
/// The text of a station that does not offer the feature.
const OLDER_STATION: &str = "This station runs an older Fleet Deck. On that computer, install the new version, then run `fleetdeck service start`.";

/// What differs between the routes.
struct Route {
    /// The audit action.
    action: &'static str,
    /// The link method.
    method: &'static str,
    /// The link feature that the method needs.
    feature: &'static str,
    /// The 409 code of a station without the feature.
    unsupported: &'static str,
    /// What the owner's login does, for the `owner_only` text.
    what: &'static str,
    /// The text of a failure of the station.
    failed: &'static str,
}

const SUBAGENT: Route = Route {
    action: "sessions.subagent",
    method: "session.subagent",
    feature: feature::SUBAGENT,
    unsupported: "subagent_unsupported",
    what: "reads the work of a subagent",
    failed: "the station could not read the subagent log",
};

const BRANCHES: Route = Route {
    action: "sessions.branches",
    method: "session.branches",
    feature: feature::SESSION_BRANCHES,
    unsupported: "branches_unsupported",
    what: "reads the branches of a session",
    failed: "the station could not read the branches",
};

const BRANCH_DIFF: Route = Route {
    action: "sessions.branch_diff",
    method: "files.branch_diff",
    feature: feature::SESSION_BRANCHES,
    unsupported: "branches_unsupported",
    what: "reads the diff of a branch",
    failed: "the station could not read the branch diff",
};

const WORKFLOWS: Route = Route {
    action: "sessions.workflows",
    method: "session.workflows",
    feature: feature::WORKFLOWS,
    unsupported: "workflows_unsupported",
    what: "reads the workflows of a session",
    failed: "the station could not read the workflows",
};

const WORKFLOW_AGENT: Route = Route {
    action: "sessions.workflow_agent",
    method: "session.workflow_agent",
    feature: feature::WORKFLOWS,
    unsupported: "workflows_unsupported",
    what: "reads the work of a workflow agent",
    failed: "the station could not read the workflow agent",
};

/// The query of the workflow page: both cursor values or none.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowsParams {
    after_order: Option<u64>,
    after_index: Option<u32>,
}

fn bad(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", message)
}

fn unreachable_station(station: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

/// True when `session` is short and holds no control character.
fn plain_session(session: &str) -> bool {
    session.chars().count() <= MAX_SESSION_CHARS && !session.chars().any(char::is_control)
}

/// True for a tool use id: 1 to 128 of `A-Za-z0-9_-`.
fn tool_use_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// True for a workflow agent id: 1 to 64 of `a-z0-9`.
fn workflow_agent_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9'))
}

/// The page cursor of the query: both values, or none. A value that does not
/// parse, an unknown key, or one value alone is a 400.
fn workflow_cursor(
    query: Result<Query<WorkflowsParams>, QueryRejection>,
) -> Result<Option<WorkflowsCursor>, ApiError> {
    let Query(p) = query.map_err(|_| bad("the page cursor is not valid"))?;
    match (p.after_order, p.after_index) {
        (Some(run_order), Some(agent_index)) => Ok(Some(WorkflowsCursor {
            run_order,
            agent_index,
        })),
        (None, None) => Ok(None),
        _ => Err(bad("the page cursor is not valid")),
    }
}

/// True for a full hex object id (SHA-1 or SHA-256).
fn head_ok(head: &str) -> bool {
    matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Maps a station error code to a fixed HTTP error. The text of the station
/// never goes to the answer.
fn station_error(route: &Route, code_text: &str, station: &str, session: &str) -> ApiError {
    match code_text {
        code::UNKNOWN_SESSION => access::unknown_session(station, session),
        code::INVALID_PARAMS => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_params",
            "the station refused the query",
        ),
        branch_code::GIT_TOO_OLD => ApiError::new(
            StatusCode::CONFLICT,
            "git_too_old",
            "Update Git on this station (2.45 or newer) to see the branches.",
        ),
        branch_code::NOT_A_REPOSITORY => ApiError::new(
            StatusCode::CONFLICT,
            "not_a_repository",
            "This session folder is not a Git repository.",
        ),
        branch_code::NO_COMMON_HISTORY => ApiError::new(
            StatusCode::CONFLICT,
            "no_common_history",
            "This branch has no history in common with the session.",
        ),
        branch_code::PLATFORM_UNSUPPORTED => ApiError::new(
            StatusCode::CONFLICT,
            "platform_unsupported",
            "The branch view needs a Linux station for now.",
        ),
        code::BUSY => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "The station is busy. Try again in a moment.",
        ),
        _ => ApiError::new(StatusCode::BAD_GATEWAY, "station_error", route.failed),
    }
}

/// The checked read: the guards of the route, then the forward. `params` is
/// built after the session is known.
async fn read<T: DeserializeOwned>(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    route: &Route,
    params: impl FnOnce() -> Result<serde_json::Value, ApiError>,
) -> Result<T, ApiError> {
    let station_id = session_of(
        board,
        principal,
        station,
        session,
        MemberAction::Read,
        route.what,
    )
    .await?;
    match board.store.link_feature(&station_id, route.feature) {
        LinkFeature::Down => return Err(unreachable_station(station)),
        LinkFeature::Without => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                route.unsupported,
                OLDER_STATION,
            ));
        }
        LinkFeature::Negotiated => {}
    }
    let control = board
        .link
        .control_for(&station_id)
        .ok_or_else(|| unreachable_station(station))?;
    let outcome = tokio::time::timeout(WORK_WAIT, control.request(route.method, None, params()?))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable_station(station))?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| {
            // The decode error can quote the answer: it is not logged.
            tracing::warn!(
                method = route.method,
                "the station sent an answer that does not parse"
            );
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station sent an answer that the deck cannot read",
            )
        }),
        Outcome::Err(e) => {
            if !matches!(
                e.code.as_str(),
                code::UNKNOWN_SESSION | code::INVALID_PARAMS | code::BUSY
            ) {
                tracing::warn!(code = %e.code, method = route.method, "the station failed a read");
            }
            Err(station_error(route, &e.code, station, session))
        }
    }
}

/// Writes the audit row of a failure. The owner's successful read writes no
/// row. The detail is the error code, never a text of the station.
async fn audit(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    route: &Route,
    error: Option<&ApiError>,
) {
    let Some(e) = error else { return };
    let kind = if matches!(
        e.status,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ) {
        AuditResult::Refused
    } else {
        AuditResult::Failed
    };
    let row = NewAudit::new(principal.actor(), route.action, kind).detail(e.code);
    // The names come from the path: keep them out of the log when they are
    // not plain.
    let target = if plain_session(station) && plain_session(session) {
        format!("{station}/{session}")
    } else {
        String::new()
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// `GET /api/sessions/{station}/{session}/subagents/{tool_use_id}`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_session`; 409 `station_unreachable`, `subagent_unsupported`; 502
/// `station_error`; 503 `busy`; 504 `timeout`.
pub async fn subagent(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, tool_use_id)): Path<(String, String, String)>,
) -> Result<Json<SubagentLog>, ApiError> {
    let result = async {
        owner_only(&principal, SUBAGENT.what)?;
        if !plain_session(&session) {
            return Err(bad("the session id is not valid"));
        }
        if !tool_use_id_ok(&tool_use_id) {
            return Err(bad("the tool use id is not valid"));
        }
        read(&board, &principal, &station, &session, &SUBAGENT, || {
            serde_json::to_value(SubagentQuery {
                session_id: SessionId::from(session.as_str()),
                tool_use_id: tool_use_id.clone(),
            })
            .map_err(ApiError::internal)
        })
        .await
    }
    .await;
    audit(
        &board,
        &principal,
        &station,
        &session,
        &SUBAGENT,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}

/// `GET /api/sessions/{station}/{session}/branches`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_session`; 409 `station_unreachable`, `branches_unsupported`,
/// `git_too_old`, `platform_unsupported`; 502 `station_error`; 503 `busy`; 504
/// `timeout`.
pub async fn branches(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<RepoBranches>, ApiError> {
    let result = async {
        owner_only(&principal, BRANCHES.what)?;
        if !plain_session(&session) {
            return Err(bad("the session id is not valid"));
        }
        read(&board, &principal, &station, &session, &BRANCHES, || {
            serde_json::to_value(BranchesQuery {
                session_id: SessionId::from(session.as_str()),
            })
            .map_err(ApiError::internal)
        })
        .await
    }
    .await;
    audit(
        &board,
        &principal,
        &station,
        &session,
        &BRANCHES,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}

/// `GET /api/sessions/{station}/{session}/branches/{head}/diff`.
///
/// # Errors
/// As [`branches`].
pub async fn branch_diff(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, head)): Path<(String, String, String)>,
) -> Result<Json<SessionDiff>, ApiError> {
    let result = async {
        owner_only(&principal, BRANCH_DIFF.what)?;
        if !plain_session(&session) {
            return Err(bad("the session id is not valid"));
        }
        if !head_ok(&head) {
            return Err(bad("the head is not a full object id"));
        }
        read(&board, &principal, &station, &session, &BRANCH_DIFF, || {
            serde_json::to_value(BranchDiffQuery {
                session_id: SessionId::from(session.as_str()),
                head: head.clone(),
            })
            .map_err(ApiError::internal)
        })
        .await
    }
    .await;
    audit(
        &board,
        &principal,
        &station,
        &session,
        &BRANCH_DIFF,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}

/// `GET /api/sessions/{station}/{session}/workflows`.
///
/// The owner and the members of the session read the page.
///
/// # Errors
/// 400 `invalid_params`; 404 `unknown_station`, `unknown_session`; 409
/// `station_unreachable`, `workflows_unsupported`; 502 `station_error`; 503
/// `busy`; 504 `timeout`.
pub async fn workflows(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    query: Result<Query<WorkflowsParams>, QueryRejection>,
) -> Result<Json<WorkflowsPage>, ApiError> {
    let result = async {
        if !plain_session(&session) {
            return Err(bad("the session id is not valid"));
        }
        let after = workflow_cursor(query)?;
        read(&board, &principal, &station, &session, &WORKFLOWS, || {
            serde_json::to_value(WorkflowsQuery {
                session_id: SessionId::from(session.as_str()),
                after,
            })
            .map_err(ApiError::internal)
        })
        .await
    }
    .await;
    audit(
        &board,
        &principal,
        &station,
        &session,
        &WORKFLOWS,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}

/// `GET /api/sessions/{station}/{session}/workflow-agents/{agent_id}`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_session`; 409 `station_unreachable`, `workflows_unsupported`; 502
/// `station_error`; 503 `busy`; 504 `timeout`.
pub async fn workflow_agent(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, agent_id)): Path<(String, String, String)>,
) -> Result<Json<SubagentLog>, ApiError> {
    let result = async {
        owner_only(&principal, WORKFLOW_AGENT.what)?;
        if !plain_session(&session) {
            return Err(bad("the session id is not valid"));
        }
        if !workflow_agent_id_ok(&agent_id) {
            return Err(bad("the agent id is not valid"));
        }
        read(
            &board,
            &principal,
            &station,
            &session,
            &WORKFLOW_AGENT,
            || {
                serde_json::to_value(WorkflowAgentQuery {
                    session_id: SessionId::from(session.as_str()),
                    agent_id: agent_id.clone(),
                })
                .map_err(ApiError::internal)
            },
        )
        .await
    }
    .await;
    audit(
        &board,
        &principal,
        &station,
        &session,
        &WORKFLOW_AGENT,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}

#[cfg(test)]
#[path = "session_work_workflow_tests.rs"]
mod workflow_tests;
