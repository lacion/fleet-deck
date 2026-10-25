//! The file routes of artifacts (R12, DL-165; R13 P5b E1):
//!
//! - `GET` and `HEAD /api/artifacts/{station}/{id}/{version}/raw/{*path}`
//!   serve one file of a version to the board login (`read`);
//! - `GET` and `HEAD /a/{token}/{*path}` serve the file of a share or view
//!   link; the token is the credential.
//!
//! A version with one `video/mp4` file is served by `serve_file` (byte
//! ranges, `HEAD`, `ETag`); every other file keeps the whole-file answer
//! from the cache or the station.

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use fleetdeck_proto::artifacts::content_type_of;

use super::common::{bad, db, file_headers, live_artifact, not_found, ok_path, station_of};
use super::range::{HEADERS_LIMIT, VIDEO_MIME, in_scope, serve_file, video_timeout};
use super::stream::{Cache, Gate, ensure_cached, serve_cached, stream_file, version_meta};
use super::{ApiError, Board, indexed};
use crate::store::artifacts::ShareRow;
use crate::store::auth::secret_hash;
use crate::store::{AuditResult, NewAudit, Principal};

/// The read authority of the raw route: the login or the token of the
/// request must be alive. A lookup that fails counts as a refusal.
fn owner_gate(board: &Arc<Board>, principal: Principal) -> Gate {
    let board = Arc::clone(board);
    Arc::new(move || {
        let (board, principal) = (Arc::clone(&board), principal.clone());
        Box::pin(async move {
            match board.store.principal_alive(&principal).await {
                Ok(alive) => alive,
                Err(e) => {
                    tracing::error!(error = %e, "read authority not checked");
                    false
                }
            }
        })
    })
}

/// `GET` and `HEAD /api/artifacts/{station}/{id}/{version}/raw/{*path}`.
///
/// # Errors
/// 400 `invalid_path`, `invalid_range` (an MP4 file); 404 `unknown_station`,
/// `unknown_artifact`, `not_found`; 409 `station_unreachable` (not cached and
/// the station is not connected); 502 `station_error` and 504 `timeout` (an
/// MP4 file).
pub async fn raw(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    method: Method,
    request: HeaderMap,
    Path((station, id, version, path)): Path<(String, String, u32, String)>,
) -> Result<Response, ApiError> {
    if !ok_path(&path) {
        return Err(bad("invalid_path", "a path has no `..`, NUL or `\\`"));
    }
    let rec = station_of(&board, &station).await?;
    indexed(&board, &rec, &id).await?;
    let video = async {
        let meta = version_meta(&board, &rec, &id, version).await?;
        if !in_scope(&meta, &path) {
            return Ok(None);
        }
        let gate = owner_gate(&board, principal);
        let head = method == Method::HEAD;
        serve_file(&board, &rec, &id, &meta, &path, &request, head, gate)
            .await
            .map(Some)
    };
    // The headers of a video answer come within 25 seconds (contract 10).
    if let Some(response) = tokio::time::timeout(HEADERS_LIMIT, video)
        .await
        .map_err(|_| video_timeout())??
    {
        return Ok(response);
    }
    if method == Method::HEAD {
        // Only an MP4 file answers a HEAD; the guard gave every other file
        // this answer before the HEAD row existed.
        return Err(not_found("not_found", "no such method"));
    }
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

/// One view is one open of the document, not one of its files, and not one
/// of the range requests that a player makes while it seeks: a whole file or
/// a range that starts at byte 0.
fn opens_document(response: &Response) -> bool {
    match response.status() {
        StatusCode::OK => true,
        StatusCode::PARTIAL_CONTENT => response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("bytes 0-")),
        _ => false,
    }
}

async fn count_view(board: &Board, row: &ShareRow, hash: String, version: u32) {
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

/// The read authority of a public file: the share or view row must still be
/// live (not revoked, not expired) and still name the same artifact.
fn share_gate(board: &Arc<Board>, token: &str, row: &ShareRow) -> Gate {
    let (board, token) = (Arc::clone(board), token.to_owned());
    let (station, artifact_id) = (row.station_id.clone(), row.artifact_id.clone());
    Arc::new(move || {
        let (board, token) = (Arc::clone(&board), token.clone());
        let (station, artifact_id) = (station.clone(), artifact_id.clone());
        Box::pin(async move {
            share_of(&board, &token)
                .await
                .is_some_and(|(r, _)| r.station_id == station && r.artifact_id == artifact_id)
        })
    })
}

/// The answer for a version with one MP4 file, or `None` when the file is
/// not in the range scope or when the bytes cannot come from this path (the
/// station is not linked and the file is not cached): the caller then
/// follows the whole-file path. It never fills the cache.
async fn public_video(
    board: &Arc<Board>,
    row: &ShareRow,
    (token, hash): (&str, &str),
    path: &str,
    request: &HeaderMap,
    head: bool,
) -> Option<Response> {
    if !path.is_empty() && content_type_of(path) != VIDEO_MIME {
        return None;
    }
    let rec = board
        .store
        .station(row.station_id.clone())
        .await
        .ok()
        .flatten()?;
    let latest = board
        .store
        .artifact(row.station_id.clone(), row.artifact_id.clone())
        .await
        .ok()
        .flatten()?
        .latest_version;
    let wanted = row.version.unwrap_or(latest);
    let linked = board.link.control_for(&rec.station_id).is_some();
    let stored = board
        .store
        .artifact_versions(row.station_id.clone(), row.artifact_id.clone())
        .await
        .ok()?
        .into_iter()
        .find(|v| v.version == wanted && !v.files.is_empty());
    let meta = match stored {
        Some(meta) => meta,
        None if linked => version_meta(board, &rec, &row.artifact_id, wanted)
            .await
            .ok()?,
        None => return None,
    };
    let file = if path.is_empty() {
        meta.files.first()?.as_str()
    } else {
        path
    };
    if !in_scope(&meta, file) {
        return None;
    }
    let cached = board
        .store
        .cached_artifact_len(
            row.station_id.clone(),
            row.artifact_id.clone(),
            wanted,
            file.to_owned(),
        )
        .await
        .ok()
        .flatten()
        .is_some();
    if !cached && !linked {
        return None;
    }
    if live_artifact(board, &row.station_id, &row.artifact_id)
        .await
        .is_err()
    {
        return Some(gone());
    }
    let gate = share_gate(board, token, row);
    let response = match serve_file(
        board,
        &rec,
        &row.artifact_id,
        &meta,
        file,
        request,
        head,
        gate,
    )
    .await
    {
        Ok(response) => response,
        // The codes of a video answer; every other failure keeps the
        // indistinguishable answer of the public route.
        Err(e) if matches!(e.code, "invalid_range" | "station_error" | "timeout") => {
            return Some(e.into_response());
        }
        Err(e) => {
            tracing::debug!(code = e.code, "shared video not served");
            return Some(gone());
        }
    };
    if !head && row.kind == "share" && opens_document(&response) {
        count_view(board, row, hash.to_owned(), wanted).await;
    }
    Some(response)
}

async fn public_file(
    board: &Arc<Board>,
    token: &str,
    path: &str,
    request: &HeaderMap,
    head: bool,
) -> Response {
    if !ok_path(path) {
        return gone();
    }
    let Some((row, hash)) = share_of(board, token).await else {
        return gone();
    };
    let video = public_video(board, &row, (token, &hash), path, request, head);
    match tokio::time::timeout(HEADERS_LIMIT, video).await {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(_) => return video_timeout().into_response(),
    }
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
        count_view(board, &row, hash, version).await;
    }
    response
}

/// `GET` and `HEAD /a/{token}/{*path}`: public; the token is the credential.
pub async fn public(
    State(board): State<Arc<Board>>,
    method: Method,
    request: HeaderMap,
    Path((token, path)): Path<(String, String)>,
) -> Response {
    public_file(&board, &token, &path, &request, method == Method::HEAD).await
}

/// `GET` and `HEAD /a/{token}/`: the main file of the shared version.
pub async fn public_main(
    State(board): State<Arc<Board>>,
    method: Method,
    request: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    public_file(&board, &token, "", &request, method == Method::HEAD).await
}

/// `GET /a/{token}`: adds the slash that relative links need.
pub async fn public_slash(Path(token): Path<String>) -> Response {
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        Redirect::permanent(&format!("/a/{token}/")).into_response()
    } else {
        gone()
    }
}
