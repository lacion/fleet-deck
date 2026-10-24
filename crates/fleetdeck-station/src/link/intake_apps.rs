//! The `session.app_*` link methods of MCP Apps (B15, design
//! `docs/v2/design/mcp-apps.md` §4.6; feature `mcp_apps.v1`).
//!
//! - `session.app_meta` and `session.app_document` read the stored app.
//! - `session.app_call`, `session.app_calls` and `session.app_release` use
//!   [`crate::apps::calls`]: the workspace protection of the app calls is
//!   there.
//!
//! A missing app row answers `app_gone` (the caps deleted it) or
//! `unknown_app`. `session.app_calls` and `session.app_release` never read the
//! app row.

use base64::Engine as _;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::{
    APP_DOC_CHUNK_BYTES, AppCallParams, AppCallsQuery, AppDocumentChunk, AppDocumentQuery,
    AppMetaQuery, AppReleaseParams,
};
use fleetdeck_proto::link::{Outcome, WireError};

use super::{bad_params, internal, ok};
use crate::apps::calls::{self, AppEnv, AppFail, code};
use crate::link::StationLink;
use crate::manager::target::AppHost;
use crate::store::apps::AppLookup;

/// Serves one app method for a link.
pub(super) async fn call(ctx: &StationLink, method: &str, params: serde_json::Value) -> Outcome {
    let env = ctx.manager.app_env();
    handle(&env, |s| ctx.manager.app_host(s), method, params).await
}

fn fail(e: AppFail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

fn done<T: serde::Serialize>(result: Result<T, AppFail>) -> Outcome {
    match result {
        Ok(v) => ok(v),
        Err(e) => fail(e),
    }
}

/// The method handler; `host_of` gives the app host of a live session.
pub(super) async fn handle(
    env: &AppEnv,
    host_of: impl Fn(&SessionId) -> Option<AppHost>,
    method: &str,
    params: serde_json::Value,
) -> Outcome {
    match method {
        "session.app_meta" => match serde_json::from_value::<AppMetaQuery>(params) {
            Ok(q) => match lookup(env, q.session_id, q.app_id).await {
                Ok(meta) => ok(meta),
                Err(e) => fail(e),
            },
            Err(e) => bad_params(e),
        },
        "session.app_document" => match serde_json::from_value::<AppDocumentQuery>(params) {
            Ok(q) => done(document(env, q).await),
            Err(e) => bad_params(e),
        },
        "session.app_call" => match serde_json::from_value::<AppCallParams>(params) {
            Ok(q) => {
                let host = host_of(&q.session_id);
                done(calls::app_call(env, host, q).await)
            }
            Err(e) => bad_params(e),
        },
        "session.app_calls" => match serde_json::from_value::<AppCallsQuery>(params) {
            Ok(q) => {
                let host = host_of(&q.session_id);
                done(calls::app_calls(env, host, q.session_id).await)
            }
            Err(e) => bad_params(e),
        },
        "session.app_release" => match serde_json::from_value::<AppReleaseParams>(params) {
            Ok(q) => done(calls::app_release(env, q.session_id, q.call_id).await),
            Err(e) => bad_params(e),
        },
        other => internal(format!("`{other}` is not an app method")),
    }
}

async fn lookup(
    env: &AppEnv,
    session: SessionId,
    app_id: String,
) -> Result<fleetdeck_proto::apps::AppMeta, AppFail> {
    match env.store.app_lookup(session, app_id).await {
        Ok(AppLookup::Found(meta)) => Ok(meta),
        Ok(AppLookup::Gone) => Err(AppFail {
            code: code::APP_GONE,
            message: "This app is no longer stored.".to_owned(),
        }),
        Ok(AppLookup::Unknown) => Err(AppFail {
            code: code::UNKNOWN_APP,
            message: "Unknown app.".to_owned(),
        }),
        Err(e) => Err(AppFail {
            code: code::INTERNAL,
            message: e.to_string(),
        }),
    }
}

/// `session.app_document`: at most [`APP_DOC_CHUNK_BYTES`] from `offset`.
async fn document(env: &AppEnv, q: AppDocumentQuery) -> Result<AppDocumentChunk, AppFail> {
    let meta = lookup(env, q.session_id.clone(), q.app_id.clone()).await?;
    if q.offset > meta.doc_bytes {
        return Err(AppFail {
            code: code::INVALID_PARAMS,
            message: "The offset is past the end of the document.".to_owned(),
        });
    }
    let chunk = env
        .store
        .app_chunk(q.session_id, q.app_id, q.offset, APP_DOC_CHUNK_BYTES)
        .await
        .map_err(|e| AppFail {
            code: code::INTERNAL,
            message: e.to_string(),
        })?;
    // The row can leave between the lookup and the read (the caps).
    let Some((bytes, total)) = chunk else {
        return Err(AppFail {
            code: code::APP_GONE,
            message: "This app is no longer stored.".to_owned(),
        });
    };
    let end = q.offset.saturating_add(bytes.len() as u64);
    Ok(AppDocumentChunk {
        data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        eof: end >= total,
        total,
    })
}

#[cfg(test)]
#[path = "intake_apps_tests.rs"]
mod tests;
