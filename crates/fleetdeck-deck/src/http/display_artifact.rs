//! The display artifact route (R13 §5A item 6, §5D item 1, §5E item 3b, R13
//! P2b artifacts): `GET
//! /api/displays/self/artifact/{station}/{id}/{version}/{*path}`.
//!
//! The route authenticates with `fd_display` only (the guard) and serves one
//! file of one version, and only when all of these hold:
//! - the current directive of the display is mode `artifact` with exactly
//!   this `{station, id, version}` (the store checks the sender and the
//!   reference again on every read);
//! - the station is in the effective scope, and a repository filter matches
//!   the repository of the artifact's session (an artifact without a session
//!   fails a repository filter);
//! - the version holds exactly one file of a kind that a wall can show (HTML,
//!   Markdown, image, SVG, PDF), and `{path}` is that file.
//!
//! Every miss before the station work is the same `404 unknown_artifact`, so
//! there is no existence oracle. The scope, the directive and the version are
//! checked again after every await, and a streamed file checks them before
//! each chunk. No station text reaches a display. The answer is inert (a
//! `sandbox` CSP with no `allow-scripts`). There is no share row, no
//! capability URL and no audit row for a read.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{ArtifactKind, content_type_of};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody};

use super::Board;
use super::artifact_routes::common::ok_path;
use super::artifact_routes::stream::{
    Cache, Gate, ensure_cached, serve_cached, stream_with, version_meta,
};
use super::display_view::{in_scope, scope_of};
use super::guard::ApiError;
use crate::store::{DisplayLogin, Principal};

/// The `{*path}` segment that names the one file of the version.
pub const ONE_FILE: &str = "_";

/// The header of every answer of this route (R13 §5E item 3b). It has no
/// `allow-scripts`.
pub const INERT_CSP: &str = "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:";

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route(
        "/api/displays/self/artifact/{station}/{id}/{version}/{*path}",
        get(artifact),
    )
}

/// The one answer for every miss before the station work.
fn miss() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_artifact",
        "No such artifact.",
    )
}

fn not_displayable() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "not_displayable",
        "This artifact cannot be shown on a wall; open it on your own device.",
    )
}

/// The fixed answer for an error of the shared helpers or of the station
/// (no station text, no station name).
fn failure(e: ApiError) -> ApiError {
    match e.code {
        "unknown_artifact" | "not_found" | "unknown_station" | "unknown_version"
        | "invalid_path" | "unknown_session" => miss(),
        "station_unreachable" => ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            "The station is not connected.",
        ),
        "timeout" => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "The station did not answer in time.",
        ),
        "login_required" => e,
        _ if e.status == StatusCode::INTERNAL_SERVER_ERROR => e,
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "The station could not send the artifact.",
        ),
    }
}

/// The headers of a 200 answer: the content type of the file, `nosniff`, no
/// referrer, `no-store` and the inert CSP for every kind.
fn display_headers(path: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_of(path)),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(INERT_CSP),
    );
    headers
}

/// True when a wall can show the files of a version: exactly one file of a
/// kind other than `Other` (R13 §5D item 1).
fn displayable(files: &[String]) -> bool {
    matches!(files, [only] if ArtifactKind::of_path(only) != ArtifactKind::Other)
}

/// The directive, the scope and the artifact of this moment (R13 §5D items
/// 17 and 20): the directive names exactly `target`, the station is in the
/// scope, and a repository filter matches the repository of the artifact's
/// session. The directive is read before and after the scope, and the two
/// reads must be equal.
async fn authorize(
    board: &Arc<Board>,
    login: &DisplayLogin,
    target: &ArtifactRef,
) -> Result<(), ApiError> {
    let read = || async {
        board
            .store
            .display_directive(login.display_id.clone())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "login_required",
                    "the display login ended",
                )
            })
    };
    let before = read().await?;
    match &before.directive {
        DirectiveBody::Artifact { artifact, .. } if artifact == target => {}
        _ => return Err(miss()),
    }
    let scope = scope_of(board, login).await?;
    let row = board
        .store
        .artifact(target.station_id.clone(), target.artifact_id.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(miss)?;
    if target.version > row.latest_version {
        return Err(miss());
    }
    let root = match row.session_id {
        Some(session) => board
            .store
            .session_summary(target.station_id.clone(), session)
            .await
            .map_err(ApiError::internal)?
            .and_then(|s| s.repo.map(|r| r.root)),
        None => None,
    };
    if !in_scope(&scope, &target.station_id, root.as_deref()) {
        return Err(miss());
    }
    if read().await? != before {
        return Err(miss());
    }
    Ok(())
}

/// `GET /api/displays/self/artifact/{station}/{id}/{version}/{*path}`.
///
/// # Errors
/// 401 `login_required`; 403 `display_only`; 404 `unknown_artifact`; 409
/// `not_displayable`, `station_unreachable`; 502 `station_error`; 504
/// `timeout`.
async fn artifact(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, id, version, path)): Path<(String, String, String, String)>,
) -> Result<Response, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    let version = version
        .parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(miss)?;
    if !ok_path(&path) {
        return Err(miss());
    }
    let target = ArtifactRef {
        station_id: StationId::new(station),
        artifact_id: id,
        version,
    };
    authorize(&board, &login, &target).await?;
    let rec = board
        .store
        .station(target.station_id.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(miss)?;
    let meta = version_meta(&board, &rec, &target.artifact_id, version)
        .await
        .map_err(failure)?;
    authorize(&board, &login, &target).await?;
    if !displayable(&meta.files) {
        return Err(not_displayable());
    }
    // The wall does not know the file name: `_` names the one file of the
    // version (R15 Q1). The content type comes from the real name.
    let path = match meta.files.first() {
        Some(file) if path == ONE_FILE || *file == path => file.clone(),
        _ => return Err(miss()),
    };
    let cache = ensure_cached(&board, &rec, &target.artifact_id, version)
        .await
        .map_err(failure)?;
    authorize(&board, &login, &target).await?;
    let headers = display_headers(&path);
    let gate: Gate = {
        let (board, login, target) = (Arc::clone(&board), login.clone(), target.clone());
        Arc::new(move || {
            let (board, login, target) = (Arc::clone(&board), login.clone(), target.clone());
            Box::pin(async move { authorize(&board, &login, &target).await.is_ok() })
        })
    };
    if cache == Cache::Stream {
        return stream_with(
            &board,
            &rec,
            &target.artifact_id,
            version,
            &path,
            headers,
            Some(gate),
        )
        .await
        .map_err(failure);
    }
    let bytes = board
        .store
        .cached_artifact_file(
            rec.station_id.clone(),
            target.artifact_id.clone(),
            version,
            path,
        )
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(miss)?;
    // The authority of this moment, right before the bytes leave; the body
    // then checks it again before each chunk.
    authorize(&board, &login, &target).await?;
    Ok(serve_cached(
        &board.store,
        &rec.station_id,
        &target.artifact_id,
        headers,
        bytes,
        Some(gate),
    ))
}

#[cfg(test)]
#[path = "display_artifact_tests.rs"]
mod display_artifact_tests;
