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
//! `sandbox` CSP with no `allow-scripts`), except for a wall app (B17 §3.4.2):
//! the one HTML file of an `artifact` directive with `app: true`, on a display
//! with `allow_apps` on (read live), for a request that declares
//! `wall_apps.v1` (the header or the query `features=`). That answer has the
//! app policy of B15 (scripts, `connect-src 'none'`). Its last check and the
//! check before each chunk also need the directive generation that chose the
//! policy and the app permission still on. There is no share row, no
//! capability URL and no audit row for a read.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{ArtifactKind, content_type_of};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody};
use fleetdeck_proto::version::feature;

use super::Board;
use super::app_csp;
use super::artifact_projection::declares;
use super::artifact_routes::common::ok_path;
use super::artifact_routes::stream::{
    Cache, Gate, ensure_cached, serve_cached, stream_with, version_meta,
};
use super::display_view::{in_scope, scope_of};
use super::guard::ApiError;
use crate::coordinator_desktop::effective_directive;
use crate::store::display_directives::EffectiveScope;
use crate::store::{DisplayLogin, Principal};
use fleetdeck_proto::apps::AppCsp;

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
pub(super) fn miss() -> ApiError {
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

/// The policy of a wall app: the app policy of B15 with no app domains
/// (`connect-src 'none'` always).
fn app_policy() -> String {
    app_csp::header(&AppCsp::default())
}

/// True when the answer is a wall app: the directive has `app: true`, the
/// display allows apps, the request declares `wall_apps.v1`, the file is HTML
/// and the deck holds the bytes (`Cache::Cached`; a streamed version stays
/// inert, fail closed).
fn is_app(app_flag: bool, allow_apps: bool, declared: bool, path: &str, cache: Cache) -> bool {
    app_flag
        && allow_apps
        && declared
        && ArtifactKind::of_path(path) == ArtifactKind::Html
        && cache == Cache::Cached
}

/// The policy header of the answer: the app policy for an app, else the
/// inert policy.
///
/// # Errors
/// An internal error when the app policy is not a valid header value.
fn policy_header(app: bool) -> Result<HeaderValue, ApiError> {
    if app {
        HeaderValue::from_str(&app_policy()).map_err(ApiError::internal)
    } else {
        Ok(HeaderValue::from_static(INERT_CSP))
    }
}

/// True when a wall can show the files of a version: exactly one file of a
/// kind other than `Other` (R13 §5D item 1).
fn displayable(files: &[String]) -> bool {
    matches!(files, [only] if ArtifactKind::of_path(only) != ArtifactKind::Other)
}

/// What one [`authorize`] saw: the generation of the directive, its `app`
/// flag, the live `allow_apps` of the display (false when the flag is off)
/// and the effective scope that was read between the two equal directive
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Authority {
    pub generation: u64,
    pub app_flag: bool,
    pub allow_apps: bool,
    pub scope: EffectiveScope,
}

impl Authority {
    /// True when the directive and the display allow an app.
    pub fn app(&self) -> bool {
        self.app_flag && self.allow_apps
    }

    /// True when `now` still allows an answer that was decided on `self`
    /// (B17 §3.4.2): the same directive generation (a new directive, also of
    /// the same artifact, or a reset by a setting change, moves it up), and
    /// for an app answer the app flag and `allow_apps` still on.
    pub fn holds(&self, now: &Self, app: bool) -> bool {
        now.generation == self.generation && (!app || now.app())
    }
}

/// The directive, the scope and the artifact of this moment (R13 §5D items
/// 17 and 20): the directive names exactly `target`, the station is in the
/// scope, and a repository filter matches the repository of the artifact's
/// session. The directive is read before and after the scope, and the two
/// reads must be equal.
pub(super) async fn authorize(
    board: &Arc<Board>,
    login: &DisplayLogin,
    target: &ArtifactRef,
) -> Result<Authority, ApiError> {
    let read = || async {
        effective_directive(&board.link, &login.display_id)
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
    let app_flag = match &before.directive {
        DirectiveBody::Artifact { artifact, app, .. } if artifact == target => *app,
        _ => return Err(miss()),
    };
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
    let allow_apps = if app_flag {
        board
            .store
            .display_get(login.display_id.clone())
            .await
            .map_err(ApiError::internal)?
            .is_some_and(|d| d.allow_apps)
    } else {
        false
    };
    if read().await? != before {
        return Err(miss());
    }
    Ok(Authority {
        generation: before.generation,
        app_flag,
        allow_apps,
        scope,
    })
}

/// [`authorize`], and the answer must still hold what `decided` allowed
/// (see [`Authority::holds`]).
async fn authorize_bound(
    board: &Arc<Board>,
    login: &DisplayLogin,
    target: &ArtifactRef,
    decided: &Authority,
    app: bool,
) -> Result<(), ApiError> {
    let now = authorize(board, login, target).await?;
    if decided.holds(&now, app) {
        Ok(())
    } else {
        Err(miss())
    }
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
    request_headers: HeaderMap,
    Query(query): Query<Vec<(String, String)>>,
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
    let declared = declares(&request_headers, &query, feature::WALL_APPS);
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
    let decided = authorize(&board, &login, &target).await?;
    let mut headers = display_headers(&path);
    // The gate of the body: an inert answer needs the authority of each
    // moment; an app answer also needs the directive generation and the app
    // permission that chose its policy (B17 §3.4.2).
    let gate_for = |app: bool| -> Gate {
        let (board, login, target) = (Arc::clone(&board), login.clone(), target.clone());
        let decided = decided.clone();
        Arc::new(move || {
            let (board, login, target) = (Arc::clone(&board), login.clone(), target.clone());
            let decided = decided.clone();
            Box::pin(async move {
                if app {
                    authorize_bound(&board, &login, &target, &decided, true)
                        .await
                        .is_ok()
                } else {
                    authorize(&board, &login, &target).await.is_ok()
                }
            })
        })
    };
    if cache == Cache::Stream {
        let gate = gate_for(false);
        headers.insert(header::CONTENT_SECURITY_POLICY, policy_header(false)?);
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
            path.clone(),
        )
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(miss)?;
    // An app gets `APP_BOOTSTRAP` in its HTML (B17 C1b). A file that is not
    // UTF-8 stays inert with its bytes unchanged (fail closed).
    let wants_app = is_app(decided.app_flag, decided.allow_apps, declared, &path, cache);
    let (app, bytes) = match (wants_app, String::from_utf8(bytes)) {
        (true, Ok(text)) => (true, super::app_public::with_bootstrap(&text).into_bytes()),
        (_, Ok(text)) => (false, text.into_bytes()),
        (_, Err(e)) => (false, e.into_bytes()),
    };
    headers.insert(header::CONTENT_SECURITY_POLICY, policy_header(app)?);
    // The authority of this moment, right before the bytes leave; the body
    // then checks it again before each chunk. An app answer also needs the
    // generation and the app permission that chose its policy.
    if app {
        authorize_bound(&board, &login, &target, &decided, true).await?;
    } else {
        authorize(&board, &login, &target).await?;
    }
    let gate = gate_for(app);
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
