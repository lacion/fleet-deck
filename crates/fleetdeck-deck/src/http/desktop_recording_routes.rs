//! The owner recording routes (R13 P5b-1 task K1, design `omarchy-p5b.md`
//! §3.2, §4.3, §4.4, §14):
//! - the `screen_record` branch of `POST /api/stations/{station}/desktop/act`
//!   ([`act`], called by [`super::desktop_routes`]): owner, Admin, the header
//!   `X-FleetDeck-Capture-Id`, `seconds` from 1 to 120; `200` with the result,
//!   `202` with a state body, or a recording error;
//! - `GET /api/stations/{station}/desktop/recording` (owner, Admin): the
//!   active recording of the station;
//! - `POST /api/stations/{station}/desktop/captures/{capture_id}/stop` (owner,
//!   Admin, CSRF, body `{}`): the owner stop.
//!
//! A request without `X-FleetDeck-Features: artifacts_video.v1` gets the legacy
//! projection of the artifact in the answer (§14.10); the header grants no
//! authority. The work, the audit rows and the fixed texts are in
//! [`crate::desktop_recording`].

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{CaptureStatus, text, valid_capture_id};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::recording::{
    ActiveRecording, RecordingCaptureStatus, project_legacy, valid_seconds,
};
use fleetdeck_proto::version::feature;
use serde::Deserialize;

use super::Board;
use super::desktop_capture_routes::{header_id_or, invalid_id};
use super::desktop_routes::{ctx, refused, relay_error, station_id};
use super::guard::{ApiError, owner_only};
use crate::desktop_capture::Reply;
use crate::desktop_capture::recording::{self, RecordOptions, RecordingError};
use crate::desktop_relay::RelayError;
use crate::store::Principal;
use crate::store::desktop_recordings::RecordingRow;

/// The header with which a board declares the features it understands.
pub const FEATURES_HEADER: &str = "X-FleetDeck-Features";

/// The largest body of the stop route: `{}`.
const MAX_STOP_BODY: usize = 1024;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route("/api/stations/{station}/desktop/recording", get(active))
        .route(
            "/api/stations/{station}/desktop/captures/{capture_id}/stop",
            post(stop).layer(DefaultBodyLimit::max(MAX_STOP_BODY)),
        )
}

fn recording_error(e: RecordingError) -> ApiError {
    let (status, code, text) = e.http();
    ApiError::new(status, code, text)
}

/// True when the request declares `artifacts_video.v1` (a comma or space
/// separated token list; the other tokens do not matter).
#[must_use]
pub fn video_aware(headers: &HeaderMap) -> bool {
    headers
        .get_all(FEATURES_HEADER)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split([',', ' ', '\t']))
        .any(|token| token == feature::ARTIFACTS_VIDEO)
}

/// The legacy projection of the artifact of a status (§14.6-§14.9).
fn project_status(status: &mut CaptureStatus) {
    if let CaptureStatus::Committed { published, .. } = status {
        project_legacy(published);
    }
}

/// Projects a recording status for a request without `artifacts_video.v1`.
pub(super) fn project(headers: &HeaderMap, status: &mut RecordingCaptureStatus) {
    if !video_aware(headers) {
        project_status(&mut status.status);
    }
}

/// The whole number of seconds of the body, or `None`: a fraction, a negative
/// number and a number outside 1 to 120 are not valid.
fn duration(n: &serde_json::Number) -> Option<u32> {
    n.as_u64()
        .filter(|n| valid_seconds(*n))
        .and_then(|n| u32::try_from(n).ok())
}

/// The status of a recording row (the lookup route calls it).
pub(super) async fn status(
    board: &Arc<Board>,
    station: &StationId,
    row: RecordingRow,
) -> Result<RecordingCaptureStatus, ApiError> {
    recording::status(&ctx(board), station, row, RecordOptions::default())
        .await
        .map_err(ApiError::internal)
}

/// The `screen_record` branch of `POST .../desktop/act`. The owner check and
/// the body parse are done.
///
/// # Errors
/// 400 `capture_id_required`, `invalid_capture_id`, `recording_duration`; 403
/// `desktop_forbidden`; the codes of [`RecordingError`].
pub(super) async fn act(
    board: &Arc<Board>,
    principal: &Principal,
    station: StationId,
    headers: &HeaderMap,
    monitor: String,
    seconds: serde_json::Number,
) -> Result<Response, ApiError> {
    let Principal::Owner(login) = principal else {
        let e = relay_error(RelayError::Forbidden);
        return Err(refused(board, principal, station.as_str(), e).await);
    };
    if !principal.has(Scope::Admin) {
        let e = relay_error(RelayError::Forbidden);
        return Err(refused(board, principal, station.as_str(), e).await);
    }
    let capture_id = match header_id_or(headers, text::CAPTURE_ID_REQUIRED) {
        Ok(id) => id,
        Err(e) => return Err(refused(board, principal, station.as_str(), e).await),
    };
    let Some(seconds) = duration(&seconds) else {
        let e = ApiError::new(
            StatusCode::BAD_REQUEST,
            fleetdeck_proto::capture::code::RECORDING_DURATION,
            text::RECORDING_DURATION,
        );
        return Err(refused(board, principal, station.as_str(), e).await);
    };
    // The task finishes the bookkeeping (the row, the indexing, the audit)
    // when the client goes away: a dropped waiter abandons nothing (§10).
    let relay = ctx(board);
    let session = login.session_hash.clone();
    let task = tokio::spawn(async move {
        recording::start(
            &relay,
            station,
            capture_id,
            monitor,
            seconds,
            session,
            RecordOptions::default(),
        )
        .await
    });
    let started = match task.await {
        Ok(result) => result,
        Err(e) => return Err(ApiError::internal(e)),
    };
    match started {
        Ok(Reply::Done(mut result)) => {
            if !video_aware(headers)
                && let Some(c) = result.capture.as_mut()
            {
                project_legacy(&mut c.published);
            }
            Ok(Json(*result).into_response())
        }
        Ok(Reply::State(status, mut body)) => {
            if !video_aware(headers) {
                project_status(&mut body);
            }
            Ok((status, Json(body)).into_response())
        }
        Err(e) => Err(recording_error(e)),
    }
}

/// `GET /api/stations/{station}/desktop/recording`.
///
/// # Errors
/// 403 `owner_only`, `desktop_forbidden`; 404 `unknown_station`; 409
/// `station_unreachable`; 501 `desktop_unsupported`; the codes of
/// [`RecordingError`].
async fn active(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ActiveRecording>, ApiError> {
    owner_only(&principal, "reads the active recording of a station")?;
    if !principal.has(Scope::Admin) {
        return Err(relay_error(RelayError::Forbidden));
    }
    let id = station_id(&board, &station).await?;
    let mut answer = recording::active(&ctx(&board), &id, RecordOptions::default())
        .await
        .map_err(recording_error)?;
    if let ActiveRecording::Active { status, .. } = &mut answer {
        project(&headers, status);
    }
    Ok(Json(answer))
}

/// The body of the stop route: exactly `{}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopBody {}

/// `POST /api/stations/{station}/desktop/captures/{capture_id}/stop`.
///
/// # Errors
/// 400 `invalid_capture_id`, `invalid_body`; 403 `owner_only`,
/// `desktop_forbidden`; 404 `unknown_station`, `unknown_capture`; 409
/// `not_recording`; 504 `desktop_timeout`.
async fn stop(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, capture_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<StopBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    owner_only(&principal, "stops a recording")?;
    if !principal.has(Scope::Admin) {
        return Err(relay_error(RelayError::Forbidden));
    }
    let id = station_id(&board, &station).await?;
    if !valid_capture_id(&capture_id) {
        return Err(invalid_id());
    }
    if body.is_err() {
        // A rejection text could quote the body.
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "the body of a stop is {}",
        ));
    }
    let (code, mut status) =
        recording::stop(&ctx(&board), &id, &capture_id, RecordOptions::default())
            .await
            .map_err(recording_error)?;
    project(&headers, &mut status);
    Ok((code, Json(status)).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn with(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(FEATURES_HEADER, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn only_the_token_artifacts_video_declares_the_video_feature() {
        assert!(video_aware(&with(&["artifacts_video.v1"])));
        assert!(video_aware(&with(&[
            "desktop_capture.v1, artifacts_video.v1"
        ])));
        assert!(video_aware(&with(&["a.v1 artifacts_video.v1 b.v1"])));
        assert!(video_aware(&with(&["a.v1", "artifacts_video.v1"])));
        for none in [
            vec![],
            vec![""],
            vec!["artifacts_video"],
            vec!["artifacts_video.v2"],
            vec!["xartifacts_video.v1"],
            vec!["desktop_recording.v1"],
        ] {
            assert!(!video_aware(&with(&none)), "{none:?}");
        }
    }

    #[test]
    fn the_duration_is_a_whole_number_from_1_to_120() {
        for ok in [1_u32, 30, 120] {
            assert_eq!(duration(&serde_json::Number::from(ok)), Some(ok));
        }
        for bad in [
            serde_json::Number::from(0),
            serde_json::Number::from(121),
            serde_json::Number::from(-1),
            serde_json::Number::from_f64(1.5).unwrap(),
            serde_json::Number::from_f64(30.0).unwrap(),
        ] {
            assert_eq!(duration(&bad), None, "{bad}");
        }
    }
}
