//! The cache fill and the streaming of the artifact files (NV-01).

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_link::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{
    ArtifactDetail, ArtifactFile, ArtifactFileQuery, ArtifactGetQuery, ArtifactVersion,
    MAX_CHUNK_BYTES,
};
use futures_util::future::BoxFuture;

use super::common::{ask, ask_on, control_of, db, file_headers, live_artifact, not_found};
use super::{ApiError, Board, MAX_CACHED_VERSION_BYTES, StationRecord};
use crate::store::DeckStore;

/// One chunk of a file: its bytes, and whether it is the last one (`eof` or
/// no bytes).
async fn read_chunk(
    control: &Control,
    name: &str,
    artifact_id: &str,
    version: u32,
    path: &str,
    offset: u64,
) -> Result<(Vec<u8>, bool), ApiError> {
    let chunk: ArtifactFile = ask_on(
        control,
        name,
        "artifacts.file",
        serde_json::json!(ArtifactFileQuery {
            artifact_id: artifact_id.to_owned(),
            version,
            path: path.to_owned(),
            offset: Some(offset),
            length: None,
        }),
    )
    .await?;
    let part = STANDARD
        .decode(&chunk.data_base64)
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string()))?;
    let last = chunk.eof || part.is_empty();
    Ok((part, last))
}

/// Asks the station for the version and stores its metadata.
async fn fetch_detail(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
) -> Result<ArtifactDetail, ApiError> {
    live_artifact(board, &rec.station_id, artifact_id).await?;
    let detail: ArtifactDetail = ask(
        board,
        rec,
        "artifacts.get",
        serde_json::json!(ArtifactGetQuery {
            artifact_id: artifact_id.to_owned(),
            version: Some(version),
        }),
    )
    .await?;
    board
        .store
        .put_artifact_versions(rec.station_id.clone(), detail.versions.clone())
        .await
        .map_err(db)?;
    Ok(detail)
}

/// Reads every file of one version from the station into the cache. The
/// caller checked that the version is not larger than
/// [`MAX_CACHED_VERSION_BYTES`].
async fn fill_cache(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
    files: &[String],
) -> Result<(), ApiError> {
    let control = control_of(board, rec)?;
    for path in files {
        let mut bytes = Vec::new();
        loop {
            live_artifact(board, &rec.station_id, artifact_id).await?;
            let (part, last) = read_chunk(
                &control,
                &rec.name,
                artifact_id,
                version,
                path,
                bytes.len() as u64,
            )
            .await?;
            bytes.extend_from_slice(&part);
            // The reported size is wrong: stop before the Vec grows (NV-01).
            if bytes.len() as u64 > MAX_CACHED_VERSION_BYTES {
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "station_error",
                    "the station sent more bytes than the version size",
                ));
            }
            if last {
                break;
            }
        }
        board
            .store
            .put_artifact_file(
                rec.station_id.clone(),
                artifact_id.to_owned(),
                version,
                path.clone(),
                bytes,
            )
            .await
            .map_err(db)?;
    }
    Ok(())
}

/// How `ensure_cached` left a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cache {
    /// The cache holds every file of the version.
    Cached,
    /// The version is too large to cache: read it from the station.
    Stream,
}

/// The metadata of one version: the stored row, or the answer of the
/// station (which the deck then stores). It caches no file.
pub(crate) async fn version_meta(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
) -> Result<ArtifactVersion, ApiError> {
    let known = board
        .store
        .artifact_versions(rec.station_id.clone(), artifact_id.to_owned())
        .await
        .map_err(db)?
        .into_iter()
        .find(|v| v.version == version && !v.files.is_empty());
    match known {
        Some(v) => Ok(v),
        None => Ok(fetch_detail(board, rec, artifact_id, version)
            .await?
            .version),
    }
}

/// Makes sure the cache holds `version`: it reads the version from the
/// station when it does not. A version over [`MAX_CACHED_VERSION_BYTES`] is
/// not cached (NV-01); the deck stores only its metadata.
pub(crate) async fn ensure_cached(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
) -> Result<Cache, ApiError> {
    let meta = version_meta(board, rec, artifact_id, version).await?;
    if meta.size > MAX_CACHED_VERSION_BYTES {
        return Ok(Cache::Stream);
    }
    let cached = board
        .store
        .cached_versions(rec.station_id.clone(), artifact_id.to_owned())
        .await
        .map_err(db)?;
    if cached.contains(&version) {
        return Ok(Cache::Cached);
    }
    fill_cache(board, rec, artifact_id, version, &meta.files).await?;
    // The cache write refuses a tombstoned identity inside its transaction;
    // the result of a fill that lost the race is not served (R13 P5a A4).
    live_artifact(board, &rec.station_id, artifact_id).await?;
    Ok(Cache::Cached)
}

/// What the body of a served file needs between two chunks. One body type
/// serves every byte path (owner `raw`, public `/a/{token}/...`, the display
/// artifact route), from the cache or from the station (R13 P5a A4).
struct Chunks {
    store: DeckStore,
    station: StationId,
    artifact_id: String,
    /// The station read for a streamed body; `None` for a cached body.
    remote: Option<Remote>,
    /// The bytes that are read and not yet sent.
    buf: Bytes,
    /// The bytes that the station sent so far.
    fetched: u64,
    /// No more bytes come after `buf`.
    eof: bool,
    done: bool,
    gate: Option<Gate>,
}

/// The station read of a streamed body.
struct Remote {
    control: Control,
    name: String,
    version: u32,
    path: String,
}

/// A check that runs before each chunk of a served file leaves the deck
/// (R13 §5D item 20): false stops the body.
pub(crate) type Gate = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

fn cut(reason: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(reason.to_string())
}

impl Chunks {
    /// False when the artifact has a tombstone, or when the lookup failed
    /// (the check fails closed).
    async fn live(&self) -> bool {
        matches!(
            self.store
                .tombstone(self.station.clone(), self.artifact_id.clone())
                .await,
            Ok(None)
        )
    }
}

/// The body of a file in chunks of at most [`MAX_CHUNK_BYTES`]. Right before
/// each chunk it asks for the tombstone (one indexed query) and runs the
/// display [`Gate`] when there is one; a positive check ends the body with an
/// error. A streamed body also checks the tombstone before each station read,
/// so a pending deletion never reaches the bytes of the station.
fn chunked(state: Chunks, headers: HeaderMap) -> Response {
    let max = usize::try_from(MAX_CHUNK_BYTES).unwrap_or(usize::MAX);
    let body = futures_util::stream::unfold(state, move |mut s| async move {
        if s.done {
            return None;
        }
        if s.buf.is_empty() {
            if s.eof {
                return None;
            }
            if !s.live().await {
                s.done = true;
                return Some((Err(cut("the artifact was deleted")), s));
            }
            let read = match &s.remote {
                Some(r) => {
                    read_chunk(
                        &r.control,
                        &r.name,
                        &s.artifact_id,
                        r.version,
                        &r.path,
                        s.fetched,
                    )
                    .await
                }
                None => return None,
            };
            match read {
                Ok((part, last)) => {
                    s.fetched += part.len() as u64;
                    s.eof = last;
                    s.buf = Bytes::from(part);
                }
                Err(e) => {
                    s.done = true;
                    return Some((Err(cut(format!("{}: {}", e.code, e.message))), s));
                }
            }
            if s.buf.is_empty() {
                return None;
            }
        }
        // The authority of this moment, right before the chunk leaves
        // (R13 §5D item 20; R13 P5a A4).
        if !s.live().await {
            s.done = true;
            return Some((Err(cut("the artifact was deleted")), s));
        }
        if let Some(gate) = &s.gate
            && !gate().await
        {
            s.done = true;
            return Some((Err(cut("the display authority ended")), s));
        }
        let n = s.buf.len().min(max);
        let chunk = s.buf.split_to(n);
        Some((Ok::<_, std::io::Error>(chunk), s))
    });
    (StatusCode::OK, headers, Body::from_stream(body)).into_response()
}

/// Serves bytes that the deck holds (the cache) in chunks, with the checks of
/// [`chunked`].
pub(crate) fn serve_cached(
    store: &DeckStore,
    station: &StationId,
    artifact_id: &str,
    headers: HeaderMap,
    bytes: Vec<u8>,
    gate: Option<Gate>,
) -> Response {
    // The length is known: the answer keeps its `Content-Length`.
    let mut headers = headers;
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
    chunked(
        Chunks {
            store: store.clone(),
            station: station.clone(),
            artifact_id: artifact_id.to_owned(),
            remote: None,
            buf: Bytes::from(bytes),
            fetched: 0,
            eof: true,
            done: false,
            gate,
        },
        headers,
    )
}

/// Streams one file of a version from the station, chunk by chunk, without
/// the cache (NV-01). The first chunk is asked before the response is
/// returned, so a station that is off gives the normal error. A failure
/// after that ends the body with an error: the client sees a cut response.
pub(super) async fn stream_file(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
    path: &str,
) -> Result<Response, ApiError> {
    stream_with(
        board,
        rec,
        artifact_id,
        version,
        path,
        file_headers(path),
        None,
    )
    .await
}

/// As [`stream_file`], with the headers of the answer and, for a display, a
/// [`Gate`]. The gate runs before the response is returned (a refusal is
/// 404 `unknown_artifact`) and before every chunk leaves (a refusal ends the
/// body with an error).
pub(crate) async fn stream_with(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
    path: &str,
    headers: HeaderMap,
    gate: Option<Gate>,
) -> Result<Response, ApiError> {
    live_artifact(board, &rec.station_id, artifact_id).await?;
    let listed = board
        .store
        .artifact_versions(rec.station_id.clone(), artifact_id.to_owned())
        .await
        .map_err(db)?
        .into_iter()
        .any(|v| v.version == version && v.files.iter().any(|f| f == path));
    if !listed {
        return Err(not_found(
            "not_found",
            format!("`{path}` is not a file of this version"),
        ));
    }
    let control = control_of(board, rec)?;
    let (part, last) = read_chunk(&control, &rec.name, artifact_id, version, path, 0).await?;
    live_artifact(board, &rec.station_id, artifact_id).await?;
    if let Some(gate) = &gate
        && !gate().await
    {
        return Err(not_found("unknown_artifact", "No such artifact."));
    }
    Ok(chunked(
        Chunks {
            store: board.store.clone(),
            station: rec.station_id.clone(),
            artifact_id: artifact_id.to_owned(),
            remote: Some(Remote {
                control,
                name: rec.name.clone(),
                version,
                path: path.to_owned(),
            }),
            fetched: part.len() as u64,
            eof: last,
            buf: Bytes::from(part),
            done: false,
            gate,
        },
        headers,
    ))
}

#[cfg(test)]
#[path = "artifact_stream_tests.rs"]
mod stream_tests;
