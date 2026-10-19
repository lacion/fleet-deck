//! The network side of a setup report (BG-3, design `board-gaps.md` 4.3),
//! for [`super::real::RealEffects`]: load the key of the station, resolve the
//! deck, and call `fleetdeck_link::send_report` pinned to the deck
//! fingerprint. There is no fallback to an unpinned connection and no key is
//! created.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::{Fingerprint, Identity};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, LinkError, NodeConfig, ReportReply, ValidReport, send_report};
use fleetdeck_proto::ids::IncarnationId;
use ring::rand::{SecureRandom, SystemRandom};

use super::effects::ReportFailure;

/// The reasons of a link error that may be named on stderr.
const KNOWN: [&str; 4] = [
    "fingerprint_mismatch",
    "timeout",
    "unsupported",
    "invalid_request",
];

/// The fixed word of a link error. A reason outside [`KNOWN`] is `failed`.
#[must_use]
pub fn failure(error: &LinkError) -> ReportFailure {
    let word = error
        .reason()
        .and_then(|reason| KNOWN.into_iter().find(|known| *known == reason))
        .unwrap_or("failed");
    ReportFailure(word)
}

/// A random incarnation of this one dial.
fn incarnation() -> Option<IncarnationId> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Some(IncarnationId::from(text))
}

/// Sends `report` to `host:port` (the first address that the name gives).
///
/// # Errors
/// [`ReportFailure`] with a fixed word.
pub async fn send(
    station_dir: &Path,
    host: &str,
    port: u16,
    deck_fingerprint: &str,
    report: &ValidReport,
    limit: Duration,
) -> Result<ReportReply, ReportFailure> {
    let identity = Identity::load(station_dir)
        .map(Arc::new)
        .map_err(|_| ReportFailure("no_identity"))?;
    let deck: Fingerprint = deck_fingerprint
        .parse()
        .map_err(|_| ReportFailure("failed"))?;
    let addr = tokio::time::timeout(limit, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| ReportFailure("timeout"))?
        .map_err(|_| ReportFailure("no_address"))?
        .next()
        .ok_or(ReportFailure("no_address"))?;
    let incarnation = incarnation().ok_or(ReportFailure("failed"))?;
    let node = NodeConfig::new(
        identity,
        LocalHello::new(Role::Station, incarnation),
        Arc::new(Epochs::in_memory()),
    );
    send_report(&node, addr, deck, report, limit)
        .await
        .map_err(|e| failure(&e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_known_reason_is_named_and_any_other_is_failed() {
        for known in KNOWN {
            let error = LinkError::Closed {
                reason: known.to_owned(),
                detail: "peer text".to_owned(),
            };
            assert_eq!(failure(&error), ReportFailure(known));
        }
        let other = LinkError::Closed {
            reason: "peer says anything".to_owned(),
            detail: String::new(),
        };
        assert_eq!(failure(&other), ReportFailure("failed"));
        assert_eq!(
            failure(&LinkError::Protocol("x".to_owned())),
            ReportFailure("failed")
        );
    }

    #[tokio::test]
    async fn a_station_without_a_key_sends_nothing_and_creates_none() {
        let dir = tempfile::tempdir().unwrap();
        let report = ValidReport::validate(
            &"ab".repeat(16),
            &"cd".repeat(32),
            "denied",
            "validated",
            "laptop",
        )
        .unwrap();
        let outcome = send(
            dir.path(),
            "127.0.0.1",
            1,
            &"ef".repeat(32),
            &report,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(outcome.unwrap_err(), ReportFailure("no_identity"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
