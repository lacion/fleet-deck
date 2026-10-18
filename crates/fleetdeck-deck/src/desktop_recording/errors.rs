//! The recording errors of the deck side (R13 P5b-1 task K1, design
//! `omarchy-p5b.md` §16): [`RecordingError`] and the fixed table of the
//! station recording codes. The station text never reaches an answer.

use axum::http::StatusCode;
use fleetdeck_proto::capture::{code, text};
use fleetdeck_proto::coordinator_desktop_text::{code as cd_code, text as cd_text};

use crate::desktop_relay::RelayError;
use crate::store::AuditResult;
use crate::store::desktop_captures::CaptureRow;

/// The code of the 501 answers.
const UNSUPPORTED: &str = "desktop_unsupported";
/// The station code of an unsupported recording.
const STATION_UNSUPPORTED: &str = "unsupported";

/// Why a recording request failed. `Relay` has the fixed texts of every other
/// desktop failure, except `Unsupported`, which has the recording texts
/// (501); `Coded` is a code with its own status and text (§16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingError {
    Relay(RelayError),
    Coded {
        status: StatusCode,
        code: &'static str,
        text: &'static str,
    },
}

impl From<RelayError> for RecordingError {
    fn from(e: RelayError) -> Self {
        Self::Relay(e)
    }
}

/// One station recording code: its status and its fixed text.
struct Known {
    code: &'static str,
    status: StatusCode,
    text: &'static str,
}

const fn known(code: &'static str, status: StatusCode, text: &'static str) -> Known {
    Known { code, status, text }
}

/// The station codes of a recording that the deck relays (§16; the terminal
/// codes of the owner column of §7.5).
const KNOWN: [Known; 14] = [
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
        code::RECORDING_TOO_LARGE,
        StatusCode::PAYLOAD_TOO_LARGE,
        text::RECORDING_TOO_LARGE,
    ),
    known(
        code::RECORDING_DIMENSIONS,
        StatusCode::UNPROCESSABLE_ENTITY,
        text::RECORDING_DIMENSIONS,
    ),
    known(
        code::RECORDING_FAILED,
        StatusCode::BAD_GATEWAY,
        text::RECORDING_FAILED,
    ),
    known(
        code::RECORDING_INVALID,
        StatusCode::BAD_GATEWAY,
        text::RECORDING_INVALID,
    ),
    known(
        code::RECORDING_PUBLISH_FAILED,
        StatusCode::BAD_GATEWAY,
        text::RECORDING_PUBLISH_FAILED,
    ),
    known(
        code::RECORDING_CANCELLED,
        StatusCode::CONFLICT,
        text::RECORDING_CANCELLED,
    ),
    known(
        code::RECORDING_INTERRUPTED,
        StatusCode::CONFLICT,
        text::RECORDING_INTERRUPTED,
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
        cd_code::DESKTOP_NOT_FOUND,
        StatusCode::NOT_FOUND,
        "The target of the desktop action was not found.",
    ),
    known(
        cd_code::DESKTOP_TIMEOUT,
        StatusCode::GATEWAY_TIMEOUT,
        "The station did not answer in time; the action may have run.",
    ),
];

impl RecordingError {
    fn of_known(k: &Known) -> Self {
        Self::Coded {
            status: k.status,
            code: k.code,
            text: k.text,
        }
    }

    /// The error of a station code, with the code and the fixed text to
    /// store: the codes of §16 and `unsupported` with the missing-tool text
    /// (`wf-recorder`) or, for any other message, the text of an unsupported
    /// capability. Any other answer is `None`.
    #[must_use]
    pub fn from_station(
        station_code: &str,
        message: &str,
    ) -> Option<(Self, &'static str, &'static str)> {
        if station_code == STATION_UNSUPPORTED {
            let fixed = if message == text::NO_RECORDER {
                text::NO_RECORDER
            } else {
                text::RECORDING_UNSUPPORTED
            };
            let e = Self::Coded {
                status: StatusCode::NOT_IMPLEMENTED,
                code: UNSUPPORTED,
                text: fixed,
            };
            return Some((e, STATION_UNSUPPORTED, fixed));
        }
        KNOWN
            .iter()
            .find(|k| k.code == station_code)
            .map(|k| (Self::of_known(k), k.code, k.text))
    }

    /// The code and the fixed text that a station `failed` status stores: a
    /// known code keeps its text; any other code is `recording_failed`.
    #[must_use]
    pub fn fixed(station_code: &str, message: &str) -> (&'static str, &'static str) {
        Self::from_station(station_code, message).map_or(
            (code::RECORDING_FAILED, text::RECORDING_FAILED),
            |(_, stored, fixed)| (stored, fixed),
        )
    }

    /// As [`Self::fixed`]; a coordinator recording also keeps the coordinator
    /// terminal codes of §7.5 that are not recording codes (`authority_lost`,
    /// `authority_unknown`, `desktop_read_timeout`) with the text of P6.
    #[must_use]
    pub fn fixed_for(
        coordinator: bool,
        station_code: &str,
        message: &str,
    ) -> (&'static str, &'static str) {
        if coordinator {
            for passed in [
                cd_code::AUTHORITY_LOST,
                cd_code::AUTHORITY_UNKNOWN,
                cd_code::DESKTOP_READ_TIMEOUT,
            ] {
                if station_code == passed {
                    return (passed, cd_text(passed).unwrap_or_default());
                }
            }
        }
        Self::fixed(station_code, message)
    }

    /// The error of a stored `failed` row.
    pub(super) fn from_row(row: &CaptureRow) -> Self {
        let stored_code = row.code.as_deref().unwrap_or(code::RECORDING_FAILED);
        let message = row.message.as_deref().unwrap_or(text::RECORDING_FAILED);
        Self::from_station(stored_code, message)
            .map_or_else(|| Self::of_known(&KNOWN[5]), |(e, _, _)| e)
    }

    /// 501 `desktop_unsupported`: a feature of the owner set is missing.
    pub(super) fn unsupported() -> Self {
        Self::Relay(RelayError::Unsupported)
    }

    /// 400 `invalid_body`: a monitor name that is not valid.
    pub(super) fn invalid_body() -> Self {
        Self::Coded {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_body",
            text: "the body is not a valid desktop action",
        }
    }

    pub(super) fn id_conflict() -> Self {
        Self::of_known(&KNOWN[11])
    }

    /// 404 `unknown_capture`.
    pub(super) fn unknown_capture() -> Self {
        Self::Coded {
            status: StatusCode::NOT_FOUND,
            code: "unknown_capture",
            text: "No such capture.",
        }
    }

    /// 409 `not_recording`: the capture is a screenshot.
    pub(super) fn not_recording() -> Self {
        Self::Coded {
            status: StatusCode::CONFLICT,
            code: code::NOT_RECORDING,
            text: text::NOT_RECORDING,
        }
    }

    /// The HTTP status, error code and fixed text.
    #[must_use]
    pub fn http(self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::Relay(RelayError::Unsupported) => (
                StatusCode::NOT_IMPLEMENTED,
                UNSUPPORTED,
                text::RECORDING_UNSUPPORTED,
            ),
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
            Self::Coded { code, .. }
                if code == "invalid_body"
                    || code == "capture_id_conflict"
                    || code == "unknown_capture"
                    || code == "not_recording" =>
            {
                AuditResult::Refused
            }
            _ => AuditResult::Failed,
        }
    }
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
