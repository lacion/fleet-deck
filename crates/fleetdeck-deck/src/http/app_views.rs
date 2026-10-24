//! The view, list and release routes of MCP Apps (B15, design `mcp-apps.md`
//! §4.7). The call route is in `app_routes`, the document route in
//! `app_public`.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::{
    AppCallsQuery, AppCallsReply, AppMeta, AppMetaQuery, AppReleaseParams, AppReleaseReply,
    AppViewReply,
};

use super::Board;
use super::app_csp;
use super::app_public::{AppTokens, VIEW_TOKEN_MS, ViewEntry, new_token, token_key};
use super::app_routes::{
    audit_result_of, bad, checked, forward, is_ulid, plain, ready, release_detail, write_audit,
};
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, Principal};

/// The longest wait for `session.app_meta`, `session.app_calls` and
/// `session.app_release`.
const READ_WAIT: Duration = Duration::from_secs(20);

/// `GET /api/sessions/{station}/{session}/apps/{app_id}`: mints a view token
/// and answers the view of the app.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_session`, `unknown_app`; 409 `apps_unsupported`,
/// `station_unreachable`; 410 `app_gone`; 502 `station_error`; 504 `timeout`.
pub async fn view(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Extension(tokens): Extension<Arc<AppTokens>>,
    Path((station, session, app_id)): Path<(String, String, String)>,
) -> Result<Json<AppViewReply>, ApiError> {
    const WHAT: &str = "opens an app";
    let station_id = checked(&board, &principal, &station, &session, WHAT).await?;
    if !plain(&app_id) {
        return Err(bad("the app id is not valid"));
    }
    let params = serde_json::to_value(AppMetaQuery {
        session_id: SessionId::from(session.as_str()),
        app_id: app_id.clone(),
    })
    .map_err(ApiError::internal)?;
    let meta: AppMeta = forward(
        &board,
        &station,
        &station_id,
        "session.app_meta",
        params,
        READ_WAIT,
    )
    .await?;
    let csp = app_csp::sanitize(&meta.csp);
    let token = new_token()?;
    let now = board.store.now_ms();
    let expires_at = now + VIEW_TOKEN_MS;
    let login = match &principal {
        Principal::Owner(s) => s.session_hash.clone(),
        _ => String::new(),
    };
    tokens.insert(
        token_key(&token),
        ViewEntry {
            station_id,
            session_id: session,
            app_id,
            csp: csp.clone(),
            doc_sha256: meta.doc_sha256,
            login_session_hash: login,
            expires_at,
        },
        now,
    );
    Ok(Json(AppViewReply {
        url: format!("/app/{token}"),
        expires_at,
        server: meta.server,
        tool: meta.tool,
        resource_uri: meta.resource_uri,
        csp,
        prefers_border: meta.prefers_border,
        tool_input: meta.tool_input,
        tool_result: meta.tool_result,
    }))
}

/// `GET /api/sessions/{station}/{session}/app-calls`: the open app calls of
/// the session and of its folder. It needs no app id.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_session`; 409 `apps_unsupported`, `station_unreachable`; 502
/// `station_error`; 504 `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<AppCallsReply>, ApiError> {
    let station_id = checked(&board, &principal, &station, &session, "lists app actions").await?;
    let params = serde_json::to_value(AppCallsQuery {
        session_id: SessionId::from(session.as_str()),
    })
    .map_err(ApiError::internal)?;
    forward(
        &board,
        &station,
        &station_id,
        "session.app_calls",
        params,
        READ_WAIT,
    )
    .await
    .map(Json)
}

/// `POST /api/sessions/{station}/{session}/app-calls/{call_id}/release`:
/// marks an `uncertain` app call as stopped. `{session}` is the call's own
/// session. It needs no app id and no body.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_session`, `unknown_call`; 409 `apps_unsupported`,
/// `station_unreachable`, `call_running`; 502 `station_error`; 504 `timeout`.
pub async fn release(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, call_id)): Path<(String, String, String)>,
) -> Result<Json<AppReleaseReply>, ApiError> {
    const WHAT: &str = "releases an app action";
    let result = async {
        owner_only(&principal, WHAT)?;
        if !plain(&session) {
            return Err(bad("the session id is not valid"));
        }
        if !is_ulid(&call_id) {
            return Err(bad("the call id is not valid"));
        }
        let station_id = ready(&board, &principal, &station, &session, WHAT).await?;
        let params = serde_json::to_value(AppReleaseParams {
            session_id: SessionId::from(session.as_str()),
            call_id: call_id.clone(),
        })
        .map_err(ApiError::internal)?;
        forward::<AppReleaseReply>(
            &board,
            &station,
            &station_id,
            "session.app_release",
            params,
            READ_WAIT,
        )
        .await
    }
    .await;
    let (kind, word) = match &result {
        Ok(r) => (AuditResult::Ok, if r.released { "ok" } else { "not_open" }),
        Err(e) => (audit_result_of(e.status), e.code),
    };
    write_audit(
        &board,
        &principal,
        "session.app_release",
        &station,
        &session,
        kind,
        release_detail(&call_id, word),
    )
    .await;
    result.map(Json)
}
