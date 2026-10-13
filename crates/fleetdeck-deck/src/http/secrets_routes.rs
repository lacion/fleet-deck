//! The credential relay route of the settings (DL-172, DL-173 doctrine rule
//! 5): `POST /api/secrets/stations/{station}/settings { key, value,
//! revision, elevation }`.
//!
//! The value crosses the deck once, in memory, inside this request and its
//! answer. It is held in a [`Secret`], sent to the station as
//! `settings.secret_set`, and dropped. It never goes through `commands`, the
//! outbox or receipts, and it is in no log, audit row, error or answer. Every
//! error is a [`RelayError`] with a fixed code and a fixed text: no handler
//! formats an error that holds the body, and none uses `ApiError::internal`.
//! The body is limited to [`MAX_BODY`].

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_core::Secret;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::settings::{SecretSet, SecretSetParams, SettingsSaved};
use serde::Deserialize;
use serde_json::json;

use super::Board;
use super::guard::ApiError;
use super::settings_routes::{SETTINGS_WAIT, is_elevated, owner, station_failure};
use crate::store::{AuditResult, NewAudit, Principal};

/// The largest request body of this route: 16 KiB.
pub const MAX_BODY: usize = 16 * 1024;
const ACTION: &str = "settings.station_secret_set";

/// The body. `Debug` hides the value.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretBody {
    key: String,
    value: String,
    revision: String,
    #[serde(default)]
    elevation: Option<String>,
}

impl std::fmt::Debug for SecretBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBody")
            .field("key", &self.key)
            .field("value", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Why a relay failed. Each variant has one fixed code and one fixed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayError {
    TooLarge,
    InvalidBody,
    ElevationRequired,
    UnknownStation,
    Unreachable,
    Timeout,
    Database,
    StationError,
    /// A save error of the station, by its code.
    Save(&'static str),
}

impl RelayError {
    fn api(self) -> ApiError {
        let (status, code, text) = match self {
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "the request body is too large",
            ),
            Self::InvalidBody => (
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the request body is not a valid secret request",
            ),
            Self::ElevationRequired => (
                StatusCode::BAD_REQUEST,
                "elevation_required",
                "a recent sign-in is needed",
            ),
            Self::UnknownStation => (
                StatusCode::NOT_FOUND,
                "unknown_station",
                "no station has this name",
            ),
            Self::Unreachable => (
                StatusCode::CONFLICT,
                "station_unreachable",
                "the station has no live link to this deck",
            ),
            Self::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            ),
            Self::Database => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the deck failed; see its log",
            ),
            Self::StationError => (
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station answered with an error",
            ),
            // The station's code decides; the text is fixed.
            Self::Save(code) => {
                let wire = fleetdeck_proto::link::WireError::new(code, "");
                let err = station_failure(&wire, true);
                return if code == "elevation_required" {
                    Self::ElevationRequired.api()
                } else {
                    err
                };
            }
        };
        let err = ApiError::new(status, code, text);
        if self == Self::ElevationRequired {
            let mut m = serde_json::Map::new();
            m.insert("keys".to_owned(), json!(["provider_instance"]));
            return err.with_details(m);
        }
        err
    }
}

/// A key that is shaped like a settings key, for the audit row; any other
/// text (a pasted value) is shown as `?`.
fn audit_key(key: &str) -> &str {
    let shaped = key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'[' | b']'));
    if shaped { key } else { "?" }
}

/// The save error codes that the relay passes on.
fn known_code(code: &str) -> Option<&'static str> {
    [
        "settings_changed",
        "settings_changed_on_disk",
        "invalid",
        "cannot_apply",
        "secret_via_relay",
        "elevation_required",
        "settings_write_failed",
        "secret_store_unavailable",
    ]
    .into_iter()
    .find(|c| *c == code)
}

async fn relay(
    board: &Board,
    name: &str,
    params: serde_json::Value,
) -> Result<SettingsSaved, RelayError> {
    let rec = board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "settings secret relay: database");
            RelayError::Database
        })?
        .ok_or(RelayError::UnknownStation)?;
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or(RelayError::Unreachable)?;
    let outcome = tokio::time::timeout(
        SETTINGS_WAIT,
        control.request("settings.secret_set", None, params),
    )
    .await
    .map_err(|_| RelayError::Timeout)?
    .map_err(|_| RelayError::Unreachable)?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| RelayError::StationError),
        Outcome::Err(e) => {
            Err(known_code(&e.code).map_or(RelayError::StationError, RelayError::Save))
        }
    }
}

/// `POST /api/secrets/stations/{station}/settings`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required` (`keys`), `invalid`,
/// `cannot_apply`, `secret_via_relay`; 403 `owner_only`; 404
/// `unknown_station`; 409 `station_unreachable`, `settings_changed`,
/// `settings_changed_on_disk`; 413 `too_large`; 500 `internal`,
/// `settings_write_failed`, `secret_store_unavailable`; 502
/// `station_error`; 504 `timeout`. The texts are fixed.
pub async fn station_settings(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<Json<SettingsSaved>, ApiError> {
    let session = owner(&board, &principal, ACTION, &name, "sets a station secret").await?;
    let result = relay_checked(&board, &session, &name, body).await;
    let (key, outcome) = match &result {
        Ok((key, _)) => (key.clone(), AuditResult::Ok),
        Err((key, _)) => (key.clone(), AuditResult::Failed),
    };
    let mut row = NewAudit::new("owner", ACTION, outcome).target(name);
    row = match &result {
        Ok(_) => row.detail(key),
        Err((_, e)) => row.detail(format!("{}: {key}", e.api().code)),
    };
    board.store.audit_or_log(row).await;
    result
        .map(|(_, saved)| Json(saved))
        .map_err(|(_, e)| e.api())
}

/// The key (for the audit row) with the saved answer or the error.
type Relayed = Result<(String, SettingsSaved), (String, RelayError)>;

async fn relay_checked(
    board: &Board,
    session: &crate::store::auth::LoginSession,
    name: &str,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Relayed {
    // The body is parsed here, not by the `Json` extractor: a serde error
    // text can quote the input, and the input can be the secret.
    let bytes = body.map_err(|e| {
        let kind = if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            RelayError::TooLarge
        } else {
            RelayError::InvalidBody
        };
        (String::new(), kind)
    })?;
    let body: SecretBody =
        serde_json::from_slice(&bytes).map_err(|_| (String::new(), RelayError::InvalidBody))?;
    drop(bytes);
    let key = audit_key(&body.key).to_owned();
    let SecretBody {
        key: raw_key,
        value,
        revision,
        elevation,
    } = body;
    let value = Secret::new(value);
    if !is_elevated(board, session, elevation.as_deref()) {
        return Err((key, RelayError::ElevationRequired));
    }
    let params = json!(SecretSetParams {
        secret: SecretSet {
            key: raw_key,
            value: value.expose().clone(),
        },
        revision,
        elevated: true,
    });
    drop(value);
    relay(board, name, params)
        .await
        .map(|saved| (key.clone(), saved))
        .map_err(|e| (key, e))
}
