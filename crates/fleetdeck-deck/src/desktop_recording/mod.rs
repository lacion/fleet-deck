//! The owner recording on the deck (R13 P5b-1 task K1, design
//! `omarchy-p5b.md` §3.2, §3.3, §4, §5.2, §6.1-§6.4, §7.5, §10, §14-§16).
//!
//! - [`start`]: the `screen_record` branch of `POST .../desktop/act`. It records
//!   the capture id, the hash, the seconds, the owner login and the link epoch
//!   (`prepared`, then `attempted`), then sends `desktop.act` once with the
//!   capture id as the authority `action_id`. A recording is never sent again.
//! - [`stop`]: `POST .../captures/{capture_id}/stop`. It commits the stop
//!   intent first, then sends `desktop.capture_stop` once (5 s).
//! - [`active`]: `GET .../desktop/recording` (`desktop.capture_active`).
//! - [`status`]: `GET .../captures/{capture_id}` of a recording.
//! - [`lease`]: the `desktop.capture_lease` handler of the station.
//! - [`recording_provenance`]: the deck's own provenance of an owner
//!   recording.
//!
//! A station text never reaches an answer: see [`RecordingError`]. An audit row
//! holds only fixed codes and the deck's own identifiers: no credential, path,
//! stderr or bytes.

use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::StopReason;

use crate::desktop_relay::RelayError;
use crate::link::desktop_authority::CallError;
use crate::store::desktop_recordings::{RecordingRow, reason_str};
use crate::store::{AuditResult, DeckStore, NewAudit};

mod active;
mod errors;
mod lease;
mod policy;
mod provenance;
mod start;
mod stop;

pub use active::{active, status};
pub use errors::RecordingError;
pub(crate) use lease::lease;
pub(crate) use provenance::reconcile;
pub use provenance::recording_provenance;
pub use start::start;
pub use stop::stop;

/// The deck wait for the station action: `seconds` plus 35 s (§10).
pub const WAIT_EXTRA: Duration = Duration::from_secs(35);
/// The whole start handler, with the indexing: `seconds` plus 40 s (§10).
pub const HANDLER_EXTRA: Duration = Duration::from_secs(40);
/// The online stop RPC (§10).
pub const STOP_WAIT: Duration = Duration::from_secs(5);
/// The whole stop handler (§10).
pub const STOP_HANDLER: Duration = Duration::from_secs(10);
/// One station read: the status and the active discovery (§10).
pub const STATUS_WAIT: Duration = Duration::from_secs(5);
/// The whole status handler (§10).
pub const LOOKUP_DEADLINE: Duration = Duration::from_secs(10);

/// The audit action of a recording (accepted, started, terminal).
const AUDIT_CAPTURE: &str = "desktop.capture";
/// The audit action of an owner stop (intent, outcome).
const AUDIT_STOP: &str = "desktop.capture_stop";

/// The waits of one recording request (a test makes them short).
#[derive(Debug, Clone, Copy)]
pub struct RecordOptions {
    /// The unit of the `seconds` of a start (1 s).
    pub second: Duration,
    /// Added to `seconds` for the deck wait for the station action.
    pub wait_extra: Duration,
    /// Added to `seconds` for the whole start handler.
    pub handler_extra: Duration,
    /// The online stop RPC.
    pub stop_wait: Duration,
    /// The whole stop handler.
    pub stop_handler: Duration,
    /// One station read.
    pub status_wait: Duration,
    /// The whole status handler.
    pub lookup: Duration,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            second: Duration::from_secs(1),
            wait_extra: WAIT_EXTRA,
            handler_extra: HANDLER_EXTRA,
            stop_wait: STOP_WAIT,
            stop_handler: STOP_HANDLER,
            status_wait: STATUS_WAIT,
            lookup: LOOKUP_DEADLINE,
        }
    }
}

impl RecordOptions {
    /// The deck wait for the station action of a recording of `seconds`.
    #[must_use]
    pub fn wait(&self, seconds: u32) -> Duration {
        self.second.saturating_mul(seconds) + self.wait_extra
    }

    /// The whole start handler of a recording of `seconds`.
    #[must_use]
    pub fn handler(&self, seconds: u32) -> Duration {
        self.second.saturating_mul(seconds) + self.handler_extra
    }
}

/// One request on `control`, with the unmapped error: a timeout is
/// [`RelayError::Timeout`], a closed link is [`RelayError::Unreachable`], and
/// a malformed answer is [`RelayError::Failed`] (the station text is never
/// logged: it could hold desktop data).
pub(crate) async fn request<T: serde::de::DeserializeOwned>(
    control: &Control,
    method: &str,
    params: serde_json::Value,
    wait: Duration,
) -> Result<T, CallError> {
    let outcome = tokio::time::timeout(wait, control.request(method, None, params))
        .await
        .map_err(|_| CallError::Relay(RelayError::Timeout))?
        .map_err(|_| CallError::Relay(RelayError::Unreachable))?;
    match outcome {
        Outcome::Ok(v) => {
            serde_json::from_value(v).map_err(|_| CallError::Relay(RelayError::Failed))
        }
        Outcome::Err(e) => Err(CallError::Wire {
            code: e.code,
            message: e.message,
        }),
    }
}

/// True for an artifact id that is safe to write into an audit row.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The fields of a `desktop.capture` audit row.
pub(crate) struct CaptureAudit<'a> {
    pub capture_id: &'a str,
    pub monitor: &'a str,
    pub seconds: Option<u32>,
    /// `accepted`, `started`, `terminal` or `refused`.
    pub stage: &'static str,
    pub code: &'a str,
    pub stop_reason: Option<StopReason>,
    pub artifact: Option<&'a str>,
}

/// Writes one `desktop.capture` row of kind `recording` (§15).
pub(crate) async fn audit_capture(
    store: &DeckStore,
    station: &StationId,
    result: AuditResult,
    f: CaptureAudit<'_>,
) {
    let mut detail = format!(
        "capture={} kind=recording monitor={}",
        f.capture_id, f.monitor
    );
    if let Some(n) = f.seconds {
        detail.push_str(&format!(" seconds={n}"));
    }
    detail.push_str(&format!(" stage={} code={}", f.stage, f.code));
    if let Some(r) = f.stop_reason {
        detail.push_str(&format!(" stop_reason={}", reason_str(r)));
    }
    if let Some(a) = f.artifact.filter(|a| plain_id(a)) {
        detail.push_str(&format!(" artifact={a}"));
    }
    store
        .audit_or_log(
            NewAudit::new("owner", AUDIT_CAPTURE, result)
                .target(station.as_str())
                .detail(detail),
        )
        .await;
}

/// Writes one `desktop.capture` row of kind `recording` for `row`: an owner
/// recording as the actor `owner`, a coordinator recording as the actor
/// `coordinator` with its caller and grant (R13 P5b-2 §15). The stage
/// `terminal` is written as the stage `outcome` for a coordinator recording.
pub(crate) async fn audit_row(
    store: &DeckStore,
    row: &RecordingRow,
    result: AuditResult,
    f: CaptureAudit<'_>,
) {
    let station = &row.capture.station_id;
    let Some(coordinator) = row.capture.coordinator.as_deref() else {
        audit_capture(store, station, result, f).await;
        return;
    };
    let stage = if f.stage == "terminal" {
        "outcome"
    } else {
        f.stage
    };
    let reason = f.stop_reason.map(reason_str);
    crate::coordinator_desktop::recording_stage(
        store,
        (coordinator, station),
        result,
        &crate::coordinator_desktop::RecordingStage {
            capture_id: f.capture_id,
            monitor: f.monitor,
            seconds: f.seconds,
            stage,
            code: f.code,
            stop_reason: reason.as_deref(),
            artifact: f.artifact.filter(|a| plain_id(a)),
        },
    )
    .await;
}

/// Writes the terminal `desktop.capture` row of a recording once: the row is
/// marked first, so a later call (a recovery pass after an answer) writes
/// nothing. `code` is `ok` or the failure code.
pub(crate) async fn audit_terminal(
    store: &DeckStore,
    row: &RecordingRow,
    code: &str,
    stop_reason: Option<StopReason>,
    artifact: Option<&str>,
) {
    let station = row.capture.station_id.clone();
    let first = store
        .recording_audited_terminal(station.clone(), row.capture.capture_id.clone())
        .await;
    match first {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            tracing::warn!(error = %e, "desktop recording: terminal audit not marked");
            return;
        }
    }
    let result = if code == "ok" {
        AuditResult::Ok
    } else {
        AuditResult::Failed
    };
    audit_row(
        store,
        row,
        result,
        CaptureAudit {
            capture_id: &row.capture.capture_id,
            monitor: &row.capture.monitor,
            seconds: row.requested_seconds,
            stage: "terminal",
            code,
            stop_reason: stop_reason.or(row.stop_reason),
            artifact,
        },
    )
    .await;
}

/// Writes one `desktop.capture_stop` row (§15): `stage` is `intent` or
/// `outcome`.
pub(crate) async fn audit_stop(
    store: &DeckStore,
    station: &StationId,
    result: AuditResult,
    capture_id: &str,
    detail: &str,
) {
    store
        .audit_or_log(
            NewAudit::new("owner", AUDIT_STOP, result)
                .target(station.as_str())
                .detail(format!("capture={capture_id} {detail}")),
        )
        .await;
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
