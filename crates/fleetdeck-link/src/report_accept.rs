//! The deck's side of a setup report on the pair channel (BG-3, design
//! `docs/v2/design/board-gaps.md` section 4).
//!
//! After the mutual Auth, an unpaired station key can send one
//! `ProvisionReport` (feature `provision_report.v1`) while the reporting
//! window of the deck is open. The handler checks the frame, gives the
//! validated fields and the authenticated key to the [`ReportSink`], sends
//! one `ProvisionReportReply` and closes the connection. A report never
//! creates a pairing, a pin, a station row, a grant, an invitation or a
//! control connection. The fingerprint comes from the authenticated key,
//! never from the frame.
//!
//! This module also holds the types that both ends share: the validated
//! report ([`ValidReport`]), the reply ([`ReportReply`]) and the sink.

use std::time::{SystemTime, UNIX_EPOCH};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use tokio::sync::OwnedSemaphorePermit;

use crate::handshake::Established;
use crate::hello::{HandshakeFrame, ReportStatus, feature};
use crate::node::{ListenerKind, Listening, NodeConfig};
use crate::pair_request::reason;
use crate::ws::{self, LinkError, Ws};

/// The close reason of a report frame that is too large or not valid.
pub const INVALID_REQUEST: &str = "invalid_request";

/// The most bytes of a report frame (the raw text).
pub const MAX_FRAME_BYTES: usize = 4096;

/// The most characters of a reported name.
pub const MAX_NAME_CHARS: usize = 64;

/// The seconds that a reply tells the reporter to wait: `stored` and
/// `disabled`.
pub const RETRY_AFTER_DEFAULT: u64 = 600;

/// The 21 terminal codes of the provisioning (`board-gaps.md` section 4.5).
/// A report with another code is stored as `unknown_error`.
pub const TERMINAL_CODES: [&str; 21] = [
    "file_invalid",
    "release_untrusted",
    "build_mismatch",
    "config_conflict",
    "config_changed",
    "service_conflict",
    "station_identity_changed",
    "paired_elsewhere",
    "denied",
    "cancelled",
    "revoked",
    "fingerprint_mismatch",
    "request_id_conflict",
    "pending",
    "unsupported",
    "invalid_request",
    "retry_file",
    "pairing_failed",
    "provider_probe_failed",
    "integration_failed",
    "unknown_error",
];

/// The phases of a report.
pub const PHASES: [&str; 6] = [
    "validated",
    "service",
    "paired",
    "link",
    "integration",
    "complete",
];

/// Why a report field is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReportRefusal {
    #[error("the terminal id is not 32 lowercase hex digits")]
    TerminalId,
    #[error("the state digest is not 64 lowercase hex digits")]
    StateDigest,
    #[error("the phase is not one of the setup phases")]
    Phase,
    #[error("the name is empty after control and direction characters are removed")]
    Name,
}

/// True for a control character or a text direction character.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// True when `text` is `len` lowercase hex digits.
#[must_use]
pub fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The validated fields of one report. The fields are the strings that go
/// on the wire and into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidReport {
    /// 32 lowercase hex digits.
    pub terminal_id: String,
    /// 64 lowercase hex digits.
    pub state_digest: String,
    /// One of [`TERMINAL_CODES`].
    pub code: String,
    /// One of [`PHASES`].
    pub phase: String,
    /// 1 to [`MAX_NAME_CHARS`] characters, without control or direction
    /// characters.
    pub name: String,
}

impl ValidReport {
    /// Checks the fields of a report. An unknown `code` becomes
    /// `unknown_error`; `name` loses its control and direction characters
    /// and is cut to [`MAX_NAME_CHARS`] characters.
    ///
    /// # Errors
    /// [`ReportRefusal`] for the first field that is not valid.
    pub fn validate(
        terminal_id: &str,
        state_digest: &str,
        code: &str,
        phase: &str,
        name: &str,
    ) -> Result<Self, ReportRefusal> {
        if !is_lower_hex(terminal_id, 32) {
            return Err(ReportRefusal::TerminalId);
        }
        if !is_lower_hex(state_digest, 64) {
            return Err(ReportRefusal::StateDigest);
        }
        if !PHASES.contains(&phase) {
            return Err(ReportRefusal::Phase);
        }
        let name: String = name
            .chars()
            .filter(|c| !is_unsafe_char(*c))
            .take(MAX_NAME_CHARS)
            .collect();
        if name.is_empty() {
            return Err(ReportRefusal::Name);
        }
        let code = if TERMINAL_CODES.contains(&code) {
            code
        } else {
            "unknown_error"
        };
        Ok(Self {
            terminal_id: terminal_id.to_owned(),
            state_digest: state_digest.to_owned(),
            code: code.to_owned(),
            phase: phase.to_owned(),
            name,
        })
    }

    /// The frame that carries this report.
    #[must_use]
    pub fn frame(&self) -> HandshakeFrame {
        HandshakeFrame::ProvisionReport {
            terminal_id: self.terminal_id.clone(),
            state_digest: self.state_digest.clone(),
            code: self.code.clone(),
            phase: self.phase.clone(),
            name: self.name.clone(),
        }
    }
}

/// The deck's answer to a report; it mirrors the frame
/// `ProvisionReportReply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportReply {
    pub status: ReportStatus,
    pub retry_after_seconds: u64,
    /// With `retry` only.
    pub retry_id: Option<String>,
    /// With `retry` only.
    pub terminal_id: Option<String>,
    /// With `retry` only.
    pub state_digest: Option<String>,
}

impl ReportReply {
    /// A reply without retry fields.
    #[must_use]
    pub fn plain(status: ReportStatus, retry_after_seconds: u64) -> Self {
        Self {
            status,
            retry_after_seconds,
            retry_id: None,
            terminal_id: None,
            state_digest: None,
        }
    }

    /// A `retry` reply for the exact binding of the report.
    #[must_use]
    pub fn retry(retry_id: String, terminal_id: String, state_digest: String) -> Self {
        Self {
            status: ReportStatus::Retry,
            retry_after_seconds: 0,
            retry_id: Some(retry_id),
            terminal_id: Some(terminal_id),
            state_digest: Some(state_digest),
        }
    }

    /// The frame that carries this reply.
    #[must_use]
    pub fn frame(&self) -> HandshakeFrame {
        HandshakeFrame::ProvisionReportReply {
            status: self.status,
            retry_after_seconds: self.retry_after_seconds,
            retry_id: self.retry_id.clone(),
            terminal_id: self.terminal_id.clone(),
            state_digest: self.state_digest.clone(),
        }
    }

    /// Checks that the retry fields are present with `retry` and only then,
    /// and that each one has its form. `None` for a reply that is not
    /// valid.
    #[must_use]
    pub fn checked(self) -> Option<Self> {
        let all =
            self.retry_id.is_some() && self.terminal_id.is_some() && self.state_digest.is_some();
        let none =
            self.retry_id.is_none() && self.terminal_id.is_none() && self.state_digest.is_none();
        let shaped = self.retry_id.as_deref().is_none_or(|v| is_lower_hex(v, 32))
            && self
                .terminal_id
                .as_deref()
                .is_none_or(|v| is_lower_hex(v, 32))
            && self
                .state_digest
                .as_deref()
                .is_none_or(|v| is_lower_hex(v, 64));
        let fits = if self.status == ReportStatus::Retry {
            all
        } else {
            none
        };
        (fits && shaped).then_some(self)
    }
}

/// What the deck does with a report. The deck implements it with its store
/// and its reporting window.
#[async_trait::async_trait]
pub trait ReportSink: Send + Sync {
    /// True while the reporting window is open. TLS asks it for every
    /// unpaired key of a tailnet listener.
    fn is_open(&self) -> bool;

    /// Handles one report of the authenticated key `peer`. `now_ms` is the
    /// Unix time of the receipt. It answers with the reply that goes to the
    /// reporter; it never fails (a store error is an answer too).
    async fn report(&self, peer: Fingerprint, report: ValidReport, now_ms: i64) -> ReportReply;
}

/// The fields of a `ProvisionReport` frame.
pub(crate) struct Fields {
    pub terminal_id: String,
    pub state_digest: String,
    pub code: String,
    pub phase: String,
    pub name: String,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `ProvisionReport`: check, hand to the sink, answer once and close.
/// `raw_len` is the length of the frame text on the wire.
pub(crate) async fn handle(
    node: &NodeConfig,
    mut ws: Ws,
    est: &Established,
    peer: PeerKey,
    on: &Listening,
    (fields, raw_len): (Fields, usize),
    _slot: OwnedSemaphorePermit,
) -> Result<(), LinkError> {
    let limit = on.hello;
    if !est.negotiated.has(feature::PROVISION_REPORT) {
        let detail = "the feature provision_report.v1 was not negotiated".to_owned();
        return Err(ws::close_reason(&mut ws, reason::UNSUPPORTED, detail).await);
    }
    let Some(sink) = node.provision_reports.clone() else {
        let detail = "this node accepts no setup reports".to_owned();
        return Err(ws::close_reason(&mut ws, reason::UNSUPPORTED, detail).await);
    };
    if node.pins.role_of(&peer) == Some(node.peer_role()) {
        let detail = "this key is already paired with this deck".to_owned();
        return Err(ws::close_reason(&mut ws, reason::ALREADY_PAIRED, detail).await);
    }
    if raw_len > MAX_FRAME_BYTES {
        let detail = format!("a setup report is at most {MAX_FRAME_BYTES} bytes");
        return Err(ws::close_reason(&mut ws, INVALID_REQUEST, detail).await);
    }
    let report = match ValidReport::validate(
        &fields.terminal_id,
        &fields.state_digest,
        &fields.code,
        &fields.phase,
        &fields.name,
    ) {
        Ok(r) => r,
        Err(e) => return Err(ws::close_reason(&mut ws, INVALID_REQUEST, e.to_string()).await),
    };
    let reply = if on.kind == ListenerKind::Tailnet {
        sink.report(peer.fingerprint(), report, now_ms()).await
    } else {
        // A LAN or loopback listener takes no report, and stores nothing.
        ReportReply::plain(ReportStatus::Disabled, RETRY_AFTER_DEFAULT)
    };
    tracing::info!(
        peer = %peer.fingerprint().short(),
        status = ?reply.status,
        "setup report answered"
    );
    ws::send_within(&mut ws, &reply.frame(), limit).await?;
    ws::close_quietly(&mut ws).await;
    Ok(())
}
