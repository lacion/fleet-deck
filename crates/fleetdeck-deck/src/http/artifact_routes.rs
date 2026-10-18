//! Artifacts on the board API and the public share route (R12, DL-165).
//!
//! - `GET /api/artifacts?session=&station=&q=` lists the deck's index;
//!   `GET /api/artifacts/{station}/{id}` gives the versions and the shares.
//! - `GET /api/artifacts/{station}/{id}/{version}/raw/{*path}` serves one
//!   file of a version from the deck's cache (filled from the station when
//!   needed). It needs the board login (`read`).
//! - `POST /api/artifacts/publish` relays `artifacts.publish` (`drive`).
//! - `POST /api/artifacts/{station}/{id}/shares` and
//!   `DELETE /api/artifacts/{station}/{id}/shares/{token}` make and revoke
//!   share links (owner only).
//! - `POST /api/artifacts/{station}/{id}/view` makes a short link for the
//!   board's own viewer: a sandboxed frame never sends the board cookie, so
//!   its page could not load its bundled files from the cookie-guarded raw
//!   route.
//! - `GET /a/{token}/{*path}` is public: the token is the credential. It
//!   serves from the cache and answers 404 for an unknown, revoked or
//!   expired token.
//!
//! Every HTML or SVG answer carries `Content-Security-Policy: sandbox ...`,
//! so the document has an opaque origin and cannot use the deck cookie or
//! API. A public link reaches anyone who can reach the deck and holds the
//! URL (DL-156).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::artifacts::{
    ArtifactDetail, ArtifactGetQuery, ArtifactPublish, ArtifactPublishRequest, ArtifactPublished,
    ArtifactVersion, DeckArtifact, DeckArtifactDetail, MAX_AUTHOR_CHARS, ShareLink, ShareRequest,
    ViewLink, ViewRequest,
};
use serde::Deserialize;

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::artifacts::{NewShare, ShareRow};
use crate::store::auth::{random_token, secret_hash};
use crate::store::{AuditResult, NewAudit, Principal, StationRecord};

#[path = "artifact_common.rs"]
pub(super) mod common;
#[path = "artifact_stream.rs"]
pub(super) mod stream;
use common::{
    ask, bad, db, file_headers, live_artifact, not_found, ok_path, station_of, unknown_artifact,
};
use stream::{Cache, ensure_cached, serve_cached, stream_file};

/// The longest the deck waits for one answer of a station.
pub const STATION_WAIT: Duration = Duration::from_secs(20);
/// The longest share: one year.
pub const MAX_SHARE_DAYS: u32 = 365;
/// How long the board's own view link lives.
pub const VIEW_LINK_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The largest version that the deck caches (NV-01). A larger version is
/// streamed from the station and never cached; DL-155 keeps the publish
/// without a size limit.
const MAX_CACHED_VERSION_BYTES: u64 = 64 * 1024 * 1024;
/// The content security policy of an HTML or SVG answer (DL-165).
pub const SANDBOX_CSP: &str = "sandbox allow-scripts allow-popups allow-forms allow-modals; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'";

/// `GET /api/artifacts` query.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub session: Option<String>,
    pub station: Option<String>,
    pub q: Option<String>,
}

/// `GET /api/artifacts`: the index, newest first.
///
/// # Errors
/// 404 `unknown_station`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<DeckArtifact>>, ApiError> {
    let station = match p.station.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => Some(station_of(&board, name).await?.station_id),
        None => None,
    };
    let session = p.session.filter(|s| !s.is_empty()).map(SessionId::from);
    let q = p.q.filter(|q| !q.trim().is_empty());
    Ok(Json(
        board
            .store
            .artifacts(station, session, q)
            .await
            .map_err(db)?,
    ))
}

async fn indexed(board: &Board, rec: &StationRecord, id: &str) -> Result<DeckArtifact, ApiError> {
    board
        .store
        .artifact(rec.station_id.clone(), id.to_owned())
        .await
        .map_err(db)?
        .ok_or_else(unknown_artifact)
}

/// `GET /api/artifacts/{station}/{id}`: the index row, every version and
/// the owner's shares. A connected station refreshes the version list; a
/// version that the deck knows only from its event has no files and size 0
/// until a station answers.
///
/// # Errors
/// 404 `unknown_station`, `unknown_artifact`.
pub async fn get(
    State(board): State<Arc<Board>>,
    Path((station, id)): Path<(String, String)>,
) -> Result<Json<DeckArtifactDetail>, ApiError> {
    let rec = station_of(&board, &station).await?;
    let artifact = indexed(&board, &rec, &id).await?;
    if board.link.control_for(&rec.station_id).is_some() {
        live_artifact(&board, &rec.station_id, &id).await?;
        let fresh: Result<ArtifactDetail, ApiError> = ask(
            &board,
            &rec,
            "artifacts.get",
            serde_json::json!(ArtifactGetQuery {
                artifact_id: id.clone(),
                version: None,
            }),
        )
        .await;
        match fresh {
            Ok(detail) => board
                .store
                .put_artifact_versions(rec.station_id.clone(), detail.versions)
                .await
                .map_err(db)?,
            Err(e) => tracing::debug!(code = e.code, "artifact versions not refreshed"),
        }
    }
    let known = board
        .store
        .artifact_versions(rec.station_id.clone(), id.clone())
        .await
        .map_err(db)?;
    let versions = (1..=artifact.latest_version)
        .map(|n| {
            known
                .iter()
                .find(|v| v.version == n)
                .cloned()
                .unwrap_or_else(|| ArtifactVersion {
                    artifact_id: id.clone(),
                    version: n,
                    size: 0,
                    sha256: String::new(),
                    files: Vec::new(),
                    created_at: 0,
                    capture: None,
                })
        })
        .collect();
    let shares = board
        .store
        .shares_of(rec.station_id.clone(), id.clone())
        .await
        .map_err(db)?;
    // A deletion during the refresh: nothing of the artifact is answered.
    live_artifact(&board, &rec.station_id, &id).await?;
    Ok(Json(DeckArtifactDetail {
        artifact,
        versions,
        shares,
    }))
}

/// `GET /api/artifacts/{station}/{id}/{version}/raw/{*path}`.
///
/// # Errors
/// 400 `invalid_path`; 404 `unknown_station`, `unknown_artifact`,
/// `not_found`; 409 `station_unreachable` (not cached and the station is
/// not connected).
pub async fn raw(
    State(board): State<Arc<Board>>,
    Path((station, id, version, path)): Path<(String, String, u32, String)>,
) -> Result<Response, ApiError> {
    if !ok_path(&path) {
        return Err(bad("invalid_path", "a path has no `..`, NUL or `\\`"));
    }
    let rec = station_of(&board, &station).await?;
    indexed(&board, &rec, &id).await?;
    if ensure_cached(&board, &rec, &id, version).await? == Cache::Stream {
        return stream_file(&board, &rec, &id, version, &path).await;
    }
    live_artifact(&board, &rec.station_id, &id).await?;
    let bytes = board
        .store
        .cached_artifact_file(rec.station_id.clone(), id.clone(), version, path.clone())
        .await
        .map_err(db)?
        .ok_or_else(|| {
            not_found(
                "not_found",
                format!("`{path}` is not a file of this version"),
            )
        })?;
    Ok(serve_cached(
        &board.store,
        &rec.station_id,
        &id,
        file_headers(&path),
        bytes,
        None,
    ))
}

fn author_of(principal: &Principal) -> String {
    principal.actor().chars().take(MAX_AUTHOR_CHARS).collect()
}

/// `POST /api/artifacts/publish`.
///
/// # Errors
/// 400 `invalid_body` and the station's codes; 404 `unknown_station`; 409
/// `station_unreachable`.
pub async fn publish(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<ArtifactPublishRequest>, JsonRejection>,
) -> Result<Json<ArtifactPublished>, ApiError> {
    let result = async {
        let Json(req) = body.map_err(|_| {
            bad(
                "invalid_body",
                "the body must be {\"station\": string, \"path\": string, \"session_id\"?, \"title\"?, \"artifact_id\"?}",
            )
        })?;
        let rec = station_of(&board, &req.station).await?;
        let mut done: ArtifactPublished = ask(
            &board,
            &rec,
            "artifacts.publish",
            serde_json::json!(ArtifactPublish {
                session_id: req.session_id,
                path: req.path,
                title: req.title,
                artifact_id: req.artifact_id,
                author: Some(author_of(&principal)),
            }),
        )
        .await?;
        done.artifact.station_id = rec.station_id.as_str().to_owned();
        board
            .store
            .note_artifact(rec.station_id.clone(), &done.artifact)
            .await
            .map_err(db)?;
        board
            .store
            .put_artifact_versions(rec.station_id, vec![done.version.clone()])
            .await
            .map_err(db)?;
        Ok::<_, ApiError>(done)
    }
    .await;
    let row = match &result {
        Ok(p) => NewAudit::new(principal.actor(), "artifacts.publish", AuditResult::Ok)
            .target(p.artifact.artifact_id.clone())
            .detail(format!("version {}", p.version.version)),
        Err(e) => NewAudit::new(principal.actor(), "artifacts.publish", AuditResult::Failed)
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(Json)
}

/// The origin that a share URL starts with: the request's own `Origin` (the
/// guard checked it), else the deck's first board origin.
fn base_url(board: &Board, headers: &HeaderMap) -> String {
    headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .filter(|o| board.origins.allows(o))
        .map(str::to_owned)
        .or_else(|| board.origins.list().into_iter().next())
        .unwrap_or_default()
}

async fn version_to_pin(
    board: &Board,
    rec: &StationRecord,
    artifact: &DeckArtifact,
    wanted: Option<u32>,
) -> Result<u32, ApiError> {
    let version = wanted.unwrap_or(artifact.latest_version);
    if version == 0 || version > artifact.latest_version {
        return Err(not_found(
            "unknown_version",
            format!(
                "artifact `{}` has no version {version}",
                artifact.artifact_id
            ),
        ));
    }
    // The bytes go into the deck cache now: the link works while the
    // station is off. A version over the cache limit is streamed instead.
    ensure_cached(board, rec, &artifact.artifact_id, version).await?;
    live_artifact(board, &rec.station_id, &artifact.artifact_id).await?;
    Ok(version)
}

fn new_token() -> Result<(String, String), ApiError> {
    let token = random_token().map_err(db)?;
    let hash = secret_hash(&token);
    Ok((token, hash))
}

/// `POST /api/artifacts/{station}/{id}/shares` (owner only).
///
/// # Errors
/// 400 `invalid_body`, `invalid_expiry`; 403 `owner_only`; 404
/// `unknown_station`, `unknown_artifact`, `unknown_version`; 409
/// `station_unreachable` (the version is not cached yet).
pub async fn share(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path((station, id)): Path<(String, String)>,
    body: Result<Json<ShareRequest>, JsonRejection>,
) -> Result<Json<ShareLink>, ApiError> {
    let result = async {
        owner_only(&principal, "makes share links")?;
        let Json(req) = body.map_err(|_| {
            bad(
                "invalid_body",
                "the body must be {\"version\"?: number, \"expires_in_days\": number}",
            )
        })?;
        if req.expires_in_days == 0 || req.expires_in_days > MAX_SHARE_DAYS {
            return Err(bad(
                "invalid_expiry",
                format!("`expires_in_days` is 1 to {MAX_SHARE_DAYS}"),
            ));
        }
        let rec = station_of(&board, &station).await?;
        let artifact = indexed(&board, &rec, &id).await?;
        version_to_pin(&board, &rec, &artifact, req.version).await?;
        let (token, hash) = new_token()?;
        let expires_at = board
            .store
            .now_ms()
            .saturating_add(i64::from(req.expires_in_days) * DAY_MS);
        let row = board
            .store
            .create_artifact_share(NewShare {
                token_hash: hash,
                station_id: rec.station_id,
                artifact_id: id.clone(),
                version: req.version,
                view: false,
                created_by: principal.actor(),
                expires_at,
            })
            .await
            .map_err(db)?
            .ok_or_else(unknown_artifact)?;
        Ok(ShareLink {
            url: format!("{}/a/{token}/", base_url(&board, &headers)),
            share_id: row.share_id,
            version: row.version,
            expires_at,
        })
    }
    .await;
    let target = format!("{station}/{id}");
    let audit = match &result {
        Ok(link) => NewAudit::new(principal.actor(), "artifacts.share", AuditResult::Ok)
            .target(target)
            .detail(format!(
                "share {} version {}",
                link.share_id,
                link.version
                    .map_or_else(|| "latest".to_owned(), |v| v.to_string())
            )),
        Err(e) => NewAudit::new(principal.actor(), "artifacts.share", AuditResult::Refused)
            .target(target)
            .detail(e.code),
    };
    board.store.audit_or_log(audit).await;
    result.map(Json)
}

/// `DELETE /api/artifacts/{station}/{id}/shares/{token}` (owner only). The
/// last segment is the share id (or the full token). A revoked share stays
/// revoked.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`, `unknown_share`.
pub async fn revoke(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, id, token)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = async {
        owner_only(&principal, "revokes share links")?;
        let rec = station_of(&board, &station).await?;
        let found = board
            .store
            .revoke_artifact_share(
                rec.station_id,
                id.clone(),
                secret_hash(&token),
                token.clone(),
            )
            .await
            .map_err(db)?;
        if found {
            Ok(serde_json::json!({ "revoked": true }))
        } else {
            Err(not_found("unknown_share", "no such share of this artifact"))
        }
    }
    .await;
    let mut row = NewAudit::new(
        principal.actor(),
        "artifacts.share_revoke",
        if result.is_ok() {
            AuditResult::Ok
        } else {
            AuditResult::Refused
        },
    )
    .target(format!("{station}/{id}"));
    if let Err(e) = &result {
        row = row.detail(e.code);
    }
    board.store.audit_or_log(row).await;
    result.map(Json)
}

/// `POST /api/artifacts/{station}/{id}/view`: a one-hour link for the
/// board's viewer. It is a share row of kind `view`: it is not listed, not
/// counted and not audited.
///
/// # Errors
/// 400 `invalid_body`; 404 `unknown_station`, `unknown_artifact`,
/// `unknown_version`; 409 `station_unreachable`.
pub async fn view(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, id)): Path<(String, String)>,
    body: Result<Json<ViewRequest>, JsonRejection>,
) -> Result<Json<ViewLink>, ApiError> {
    let Json(req) =
        body.map_err(|_| bad("invalid_body", "the body must be {\"version\"?: number}"))?;
    let rec = station_of(&board, &station).await?;
    let artifact = indexed(&board, &rec, &id).await?;
    let version = version_to_pin(&board, &rec, &artifact, req.version).await?;
    let (token, hash) = new_token()?;
    let expires_at = board.store.now_ms().saturating_add(VIEW_LINK_MS);
    board
        .store
        .create_artifact_share(NewShare {
            token_hash: hash,
            station_id: rec.station_id,
            artifact_id: id,
            version: Some(version),
            view: true,
            created_by: principal.actor(),
            expires_at,
        })
        .await
        .map_err(db)?
        .ok_or_else(unknown_artifact)?;
    if let Err(e) = board.store.prune_view_links().await {
        tracing::debug!(error = %e, "view links not pruned");
    }
    Ok(Json(ViewLink {
        url: format!("/a/{token}/"),
        expires_at,
    }))
}

fn gone() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

async fn share_of(board: &Board, token: &str) -> Option<(ShareRow, String)> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hash = secret_hash(token);
    let row = board
        .store
        .share_by_hash(hash.clone())
        .await
        .ok()
        .flatten()?;
    row.live(board.store.now_ms()).then_some((row, hash))
}

/// The version that a share serves: its pinned version, or the newest one
/// whose bytes the deck holds (a share of "the latest" prefers the newest
/// version and reads it from the station when it can). A version over the
/// cache limit is streamed while the station has a live link (NV-01); with
/// no link it counts as a version that is not cached.
async fn served_version(board: &Board, row: &ShareRow) -> Option<(u32, Cache)> {
    let rec = board
        .store
        .station(row.station_id.clone())
        .await
        .ok()
        .flatten();
    let latest = board
        .store
        .artifact(row.station_id.clone(), row.artifact_id.clone())
        .await
        .ok()
        .flatten()?
        .latest_version;
    let wanted = row.version.unwrap_or(latest);
    if let Some(rec) = &rec {
        match ensure_cached(board, rec, &row.artifact_id, wanted).await {
            Ok(Cache::Stream) if board.link.control_for(&rec.station_id).is_some() => {
                return Some((wanted, Cache::Stream));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(code = e.code, "share version not read from the station"),
        }
    }
    let cached = board
        .store
        .cached_versions(row.station_id.clone(), row.artifact_id.clone())
        .await
        .ok()?;
    if cached.contains(&wanted) {
        Some((wanted, Cache::Cached))
    } else if row.version.is_none() {
        cached.first().map(|v| (*v, Cache::Cached))
    } else {
        None
    }
}

async fn public_file(board: &Board, token: &str, path: &str) -> Response {
    if !ok_path(path) {
        return gone();
    }
    let Some((row, hash)) = share_of(board, token).await else {
        return gone();
    };
    let Some((version, cache)) = served_version(board, &row).await else {
        return gone();
    };
    if live_artifact(board, &row.station_id, &row.artifact_id)
        .await
        .is_err()
    {
        return gone();
    }
    let versions = board
        .store
        .artifact_versions(row.station_id.clone(), row.artifact_id.clone())
        .await
        .unwrap_or_default();
    let Some(files) = versions
        .iter()
        .find(|v| v.version == version)
        .map(|v| &v.files)
    else {
        return gone();
    };
    let Some(main) = files.first() else {
        return gone();
    };
    let wanted = if path.is_empty() { main.as_str() } else { path };
    if !files.iter().any(|f| f == wanted) {
        return gone();
    }
    let response = match cache {
        Cache::Cached => {
            let Ok(Some(bytes)) = board
                .store
                .cached_artifact_file(
                    row.station_id.clone(),
                    row.artifact_id.clone(),
                    version,
                    wanted.to_owned(),
                )
                .await
            else {
                return gone();
            };
            serve_cached(
                &board.store,
                &row.station_id,
                &row.artifact_id,
                file_headers(wanted),
                bytes,
                None,
            )
        }
        Cache::Stream => {
            let Ok(Some(rec)) = board.store.station(row.station_id.clone()).await else {
                return gone();
            };
            match stream_file(board, &rec, &row.artifact_id, version, wanted).await {
                Ok(response) => response,
                Err(e) => {
                    tracing::debug!(code = e.code, "shared file not streamed from the station");
                    return gone();
                }
            }
        }
    };
    // One view is one open of the document, not one of its files.
    if wanted == main && row.kind == "share" {
        if let Err(e) = board.store.count_share_view(hash).await {
            tracing::error!(error = %e, "share view not counted");
        }
        board
            .store
            .audit_or_log(
                NewAudit::new(
                    format!("share:{}", row.share_id),
                    "artifacts.share_view",
                    AuditResult::Ok,
                )
                .target(format!("{}/{}", row.station_id, row.artifact_id))
                .detail(format!("version {version}")),
            )
            .await;
    }
    response
}

/// `GET /a/{token}/{*path}`: public; the token is the credential.
pub async fn public(
    State(board): State<Arc<Board>>,
    Path((token, path)): Path<(String, String)>,
) -> Response {
    public_file(&board, &token, &path).await
}

/// `GET /a/{token}/`: the main file of the shared version.
pub async fn public_main(State(board): State<Arc<Board>>, Path(token): Path<String>) -> Response {
    public_file(&board, &token, "").await
}

/// `GET /a/{token}`: adds the slash that relative links need.
pub async fn public_slash(Path(token): Path<String>) -> Response {
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        Redirect::permanent(&format!("/a/{token}/")).into_response()
    } else {
        gone()
    }
}
