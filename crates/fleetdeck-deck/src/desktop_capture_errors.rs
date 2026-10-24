//! The capture errors of the deck side (R13 P5a task E): [`CaptureError`] and
//! the fixed table of the station capture codes. Moved from
//! `desktop_capture.rs` for the module size rule (R13 P6 F0).

use axum::http::StatusCode;
use fleetdeck_proto::capture::{code, text};

use crate::desktop_relay::RelayError;
use crate::store::AuditResult;
use crate::store::desktop_captures::CaptureRow;

/// Why a capture request failed. `RelayError` has the fixed texts of every
/// other desktop failure; `Coded` is a capture code with its own status and
/// text (§4.1, §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    Relay(RelayError),
    Coded {
        status: StatusCode,
        code: &'static str,
        text: &'static str,
    },
}

impl From<RelayError> for CaptureError {
    fn from(e: RelayError) -> Self {
        Self::Relay(e)
    }
}

/// One station capture code: its status and its fixed text.
pub(super) struct Known {
    pub(super) code: &'static str,
    pub(super) status: StatusCode,
    pub(super) text: &'static str,
}

const fn known(code: &'static str, status: StatusCode, text: &'static str) -> Known {
    Known { code, status, text }
}

pub(super) const KNOWN: [Known; 10] = [
    known(code::BUSY, StatusCode::CONFLICT, text::BUSY),
    known(
        code::DISK_FULL,
        StatusCode::INSUFFICIENT_STORAGE,
        text::DISK_FULL,
    ),
    known(
        code::CAPTURE_QUOTA,
        StatusCode::CONFLICT,
        text::CAPTURE_QUOTA,
    ),
    known(
        code::CAPTURE_TOO_LARGE,
        StatusCode::PAYLOAD_TOO_LARGE,
        text::CAPTURE_TOO_LARGE,
    ),
    known(
        code::CAPTURE_DIMENSIONS,
        StatusCode::UNPROCESSABLE_ENTITY,
        text::CAPTURE_DIMENSIONS,
    ),
    known(
        code::CAPTURE_FAILED,
        StatusCode::BAD_GATEWAY,
        text::CAPTURE_FAILED,
    ),
    known(
        code::CAPTURE_PUBLISH_FAILED,
        StatusCode::BAD_GATEWAY,
        text::CAPTURE_PUBLISH_FAILED,
    ),
    known(
        code::SCREEN_LOCKED,
        StatusCode::CONFLICT,
        text::SCREEN_LOCKED,
    ),
    known(
        code::CAPTURE_ID_CONFLICT,
        StatusCode::CONFLICT,
        text::CAPTURE_ID_CONFLICT,
    ),
    known(
        code::CAPTURE_INTERRUPTED,
        StatusCode::BAD_GATEWAY,
        text::CAPTURE_INTERRUPTED,
    ),
];

/// The station code that the deck stores for the missing `grim`.
const NO_GRIM_CODE: &str = "unsupported";

impl CaptureError {
    pub(super) fn of_known(k: &Known) -> Self {
        Self::Coded {
            status: k.status,
            code: k.code,
            text: k.text,
        }
    }

    /// The error of a station capture code, with the code and the fixed text
    /// to store: the ten codes of §4.1 and `unsupported` with the grim text.
    /// Any other answer is `None`.
    #[must_use]
    pub fn from_station(
        station_code: &str,
        message: &str,
    ) -> Option<(Self, &'static str, &'static str)> {
        if station_code == NO_GRIM_CODE && message == text::NO_GRIM {
            let e = Self::Coded {
                status: StatusCode::CONFLICT,
                code: "desktop_unsupported",
                text: text::NO_GRIM,
            };
            return Some((e, NO_GRIM_CODE, text::NO_GRIM));
        }
        KNOWN
            .iter()
            .find(|k| k.code == station_code)
            .map(|k| (Self::of_known(k), k.code, k.text))
    }

    /// The error of a stored `failed` row.
    pub(super) fn from_row(row: &CaptureRow) -> Self {
        let stored_code = row.code.as_deref().unwrap_or(code::CAPTURE_FAILED);
        let message = row.message.as_deref().unwrap_or(text::CAPTURE_FAILED);
        Self::from_station(stored_code, message)
            .map_or_else(|| Self::of_known(&KNOWN[5]), |(e, _, _)| e)
    }

    pub(super) fn invalid_body() -> Self {
        Self::Coded {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_body",
            text: "the body is not a valid desktop action",
        }
    }

    pub(super) fn id_conflict() -> Self {
        Self::of_known(&KNOWN[8])
    }

    /// The HTTP status, error code and fixed text.
    #[must_use]
    pub fn http(self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::Relay(e) => {
                let (status, code) = e.http();
                (status, code, e.message())
            }
            Self::Coded { status, code, text } => (status, code, text),
        }
    }

    pub(super) fn audit_result(self) -> AuditResult {
        match self {
            Self::Relay(
                RelayError::ControlOff
                | RelayError::Unsupported
                | RelayError::Forbidden
                | RelayError::Invalid
                | RelayError::NotFound
                | RelayError::Conflict,
            ) => AuditResult::Refused,
            Self::Coded { code, .. } if code == "invalid_body" || code == "capture_id_conflict" => {
                AuditResult::Refused
            }
            _ => AuditResult::Failed,
        }
    }
}
