//! The cache fill and the streaming of the artifact files (NV-01).

use axum::body::{Body, Bytes};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_link::Control;
use fleetdeck_proto::artifacts::{
    ArtifactDetail, ArtifactFile, ArtifactFileQuery, ArtifactGetQuery,
};

use super::{
    ApiError, Board, MAX_CACHED_VERSION_BYTES, StationRecord, ask, ask_on, control_of, db,
    file_headers, not_found,
};

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
pub(super) enum Cache {
    /// The cache holds every file of the version.
    Cached,
    /// The version is too large to cache: read it from the station.
    Stream,
}

/// Makes sure the cache holds `version`: it reads the version from the
/// station when it does not. A version over [`MAX_CACHED_VERSION_BYTES`] is
/// not cached (NV-01); the deck stores only its metadata.
pub(super) async fn ensure_cached(
    board: &Board,
    rec: &StationRecord,
    artifact_id: &str,
    version: u32,
) -> Result<Cache, ApiError> {
    let known = board
        .store
        .artifact_versions(rec.station_id.clone(), artifact_id.to_owned())
        .await
        .map_err(db)?
        .into_iter()
        .find(|v| v.version == version && !v.files.is_empty());
    let meta = match known {
        Some(v) => v,
        None => {
            fetch_detail(board, rec, artifact_id, version)
                .await?
                .version
        }
    };
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
    Ok(Cache::Cached)
}

/// What the body of a streamed file needs between two chunks.
struct Chunks {
    control: Control,
    name: String,
    artifact_id: String,
    version: u32,
    path: String,
    offset: u64,
    /// The first chunk, asked before the response left (NV-01).
    first: Option<(Vec<u8>, bool)>,
    done: bool,
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
    let first = read_chunk(&control, &rec.name, artifact_id, version, path, 0).await?;
    let state = Chunks {
        control,
        name: rec.name.clone(),
        artifact_id: artifact_id.to_owned(),
        version,
        path: path.to_owned(),
        offset: 0,
        first: Some(first),
        done: false,
    };
    let body = futures_util::stream::unfold(state, |mut s| async move {
        if s.done {
            return None;
        }
        let (part, last) = match s.first.take() {
            Some(chunk) => chunk,
            None => match read_chunk(
                &s.control,
                &s.name,
                &s.artifact_id,
                s.version,
                &s.path,
                s.offset,
            )
            .await
            {
                Ok(chunk) => chunk,
                Err(e) => {
                    s.done = true;
                    let cut = std::io::Error::other(format!("{}: {}", e.code, e.message));
                    return Some((Err(cut), s));
                }
            },
        };
        if part.is_empty() {
            return None;
        }
        s.offset += part.len() as u64;
        s.done = last;
        Some((Ok::<_, std::io::Error>(Bytes::from(part)), s))
    });
    Ok((StatusCode::OK, file_headers(path), Body::from_stream(body)).into_response())
}
