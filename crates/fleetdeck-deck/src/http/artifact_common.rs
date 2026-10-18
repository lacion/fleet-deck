//! The helpers that the artifact routes share (R12, DL-165; R13 P2b): the
//! station lookup and link calls, the error mapping of the `artifacts.*`
//! station codes, the path check and the response headers of an owner or
//! share answer. The display artifact route (`display_artifact`) uses them
//! too.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use fleetdeck_link::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{ArtifactKind, content_type_of};
use fleetdeck_proto::link::Outcome;
use serde::de::DeserializeOwned;

use super::{Board, SANDBOX_CSP, STATION_WAIT};
use crate::http::guard::ApiError;
use crate::store::{DeckError, StationRecord};

pub(crate) fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

pub(crate) fn not_found(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, message)
}

pub(crate) fn db(e: DeckError) -> ApiError {
    ApiError::internal(e)
}

/// 404 `unknown_artifact`: the answer for a missing or a deleted artifact.
pub(crate) fn unknown_artifact() -> ApiError {
    not_found("unknown_artifact", "No such artifact.")
}

/// 404 `unknown_artifact` when the identity has a tombstone (R13 P5a A4). A
/// route calls it before it asks a station for the artifact, and again after
/// a station read that fills the cache.
pub(crate) async fn live_artifact(
    board: &Board,
    station: &StationId,
    artifact_id: &str,
) -> Result<(), ApiError> {
    match board
        .store
        .tombstone(station.clone(), artifact_id.to_owned())
        .await
        .map_err(db)?
    {
        None => Ok(()),
        Some(_) => Err(unknown_artifact()),
    }
}

pub(crate) async fn station_of(board: &Board, name: &str) -> Result<StationRecord, ApiError> {
    board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(db)?
        .ok_or_else(|| not_found("unknown_station", format!("no station is named `{name}`")))
}

/// Maps a station `artifacts.*` error code to an HTTP error.
pub(crate) fn station_error(code: &str, message: String) -> ApiError {
    use fleetdeck_proto::artifacts::code as c;
    let (status, known) = match code {
        c::INVALID_PATH => (StatusCode::BAD_REQUEST, c::INVALID_PATH),
        c::INVALID_PARAMS => (StatusCode::BAD_REQUEST, c::INVALID_PARAMS),
        c::OUTSIDE_ROOTS => (StatusCode::BAD_REQUEST, c::OUTSIDE_ROOTS),
        c::NOT_FOUND => (StatusCode::NOT_FOUND, c::NOT_FOUND),
        c::UNKNOWN_SESSION => (StatusCode::NOT_FOUND, c::UNKNOWN_SESSION),
        c::UNKNOWN_ARTIFACT => (StatusCode::NOT_FOUND, c::UNKNOWN_ARTIFACT),
        c::UNSUPPORTED => (StatusCode::NOT_IMPLEMENTED, c::UNSUPPORTED),
        _ => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{code}: {message}"),
            );
        }
    };
    ApiError::new(status, known, message)
}

/// The live link of a station, or 409 `station_unreachable`.
pub(crate) fn control_of(board: &Board, rec: &StationRecord) -> Result<Control, ApiError> {
    board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(|| unreachable_error(&rec.name))
}

pub(crate) fn unreachable_error(name: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{name}` has no live link to this deck"),
    )
}

/// Sends `method` on `control` to the station named `name` and decodes the
/// answer.
pub(crate) async fn ask_on<T: DeserializeOwned>(
    control: &Control,
    name: &str,
    method: &'static str,
    params: serde_json::Value,
) -> Result<T, ApiError> {
    let outcome = tokio::time::timeout(STATION_WAIT, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable_error(name))?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(station_error(&e.code, e.message)),
    }
}

/// Sends `method` to the station on its live link and decodes the answer.
pub(crate) async fn ask<T: DeserializeOwned>(
    board: &Board,
    rec: &StationRecord,
    method: &'static str,
    params: serde_json::Value,
) -> Result<T, ApiError> {
    let control = control_of(board, rec)?;
    ask_on(&control, &rec.name, method, params).await
}

pub(crate) fn ok_path(path: &str) -> bool {
    !path.contains('\0') && !path.contains('\\') && !path.split('/').any(|s| s == ".." || s == ".")
}

/// The headers for one file: its content type, `nosniff`, and for a
/// document (HTML, SVG) the sandbox policy.
pub(crate) fn file_headers(path: &str) -> HeaderMap {
    let content_type = content_type_of(path);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let kind = ArtifactKind::of_path(path);
    if matches!(kind, ArtifactKind::Html | ArtifactKind::Svg) {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SANDBOX_CSP),
        );
    }
    headers
}

/// The headers of a `200` or `206` answer of an MP4 file (R13 P5b E1): the
/// headers of [`file_headers`], the range support, the strong `ETag` and the
/// exact `Content-Length` of the body. No compression or transformation
/// applies to these answers, so none of them sets `Content-Encoding`.
pub(crate) fn video_headers(path: &str, etag: HeaderValue, length: u64) -> HeaderMap {
    let mut headers = file_headers(path);
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::ETAG, etag);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers
}
