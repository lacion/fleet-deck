//! The local socket op `fleet_summary` (R13 P4 task B2): the answer for
//! `fleetdeck desktop feed` and `desktop open`.
//!
//! The op holds the request until `seq > after` or the hold limit passes,
//! then answers with the stored summary. When the live setting
//! `fleet_summary` is off, it answers `FleetSummaryOff` at once.

use std::sync::Arc;
use std::time::Duration;

use crate::fleet_summary::SummaryCell;
use crate::link::StationNode;
use crate::local::LocalResponse;
use crate::manager::SessionManager;

/// How long the op holds a request in production.
pub const HOLD: Duration = Duration::from_secs(25);

/// The op as `local::handle` calls it.
pub(crate) async fn fleet_summary(
    manager: &SessionManager,
    node: Option<&StationNode>,
    after: u64,
) -> LocalResponse {
    answer(
        || manager.policy().fleet_summary(),
        node.map(|node| &node.link().summary),
        after,
        HOLD,
    )
    .await
}

/// `enabled` reads the live setting: once before the hold and once at its
/// end. Without a `cell` (no link node) the answer is `down` with no summary
/// after the full hold.
pub(crate) async fn answer(
    enabled: impl Fn() -> bool,
    cell: Option<&Arc<SummaryCell>>,
    after: u64,
    hold: Duration,
) -> LocalResponse {
    if !enabled() {
        return LocalResponse::FleetSummaryOff {};
    }
    let Some(cell) = cell else {
        tokio::time::sleep(hold).await;
        return if enabled() {
            LocalResponse::FleetSummary {
                seq: 0,
                link: "down".to_owned(),
                held_s: 0,
                summary: None,
            }
        } else {
            LocalResponse::FleetSummaryOff {}
        };
    };
    cell.wait_after(after, hold).await;
    if !enabled() {
        return LocalResponse::FleetSummaryOff {};
    }
    let read = cell.read();
    LocalResponse::FleetSummary {
        seq: read.seq,
        link: if read.link_up { "up" } else { "down" }.to_owned(),
        held_s: read.held_s,
        summary: read.summary,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::local::LocalRequest;
    use fleetdeck_proto::fleet_summary::FleetSummary;

    fn summary(at: &str) -> FleetSummary {
        let text = format!(
            r#"{{"v":1,"at":"{at}","board":null,"deck":"up",
 "counts":{{"working":1,"idle":0,"needs_you":0,"blocked":0,"lost":0}},
 "needs_you":[],"usage":null,"stations":[]}}"#
        );
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn local_summary_hold_is_25_seconds() {
        assert_eq!(HOLD, Duration::from_secs(25));
    }

    #[test]
    fn local_summary_wire_names() {
        let request = serde_json::to_string(&LocalRequest::FleetSummary { after: 3 }).unwrap();
        assert_eq!(request, r#"{"op":"fleet_summary","after":3}"#);
        let off = serde_json::to_string(&LocalResponse::FleetSummaryOff {}).unwrap();
        assert_eq!(off, r#"{"status":"fleet_summary_off"}"#);
        let some = serde_json::to_value(LocalResponse::FleetSummary {
            seq: 2,
            link: "up".to_owned(),
            held_s: 4,
            summary: None,
        })
        .unwrap();
        assert_eq!(some["status"], "fleet_summary");
        assert_eq!(some["seq"], 2);
        assert_eq!(some["link"], "up");
        assert_eq!(some["held_s"], 4);
        assert!(some["summary"].is_null());
    }

    #[tokio::test]
    async fn local_summary_answers_at_once_when_seq_is_above_after() {
        let cell = Arc::new(SummaryCell::new());
        cell.store(summary("a"));
        let started = Instant::now();
        let got = answer(|| true, Some(&cell), 0, Duration::from_secs(20)).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        let LocalResponse::FleetSummary {
            seq,
            link,
            summary: stored,
            ..
        } = got
        else {
            panic!("{got:?}");
        };
        assert_eq!((seq, link.as_str()), (1, "down"));
        assert_eq!(stored, Some(summary("a")));
    }

    #[tokio::test]
    async fn local_summary_holds_until_a_change() {
        let cell = Arc::new(SummaryCell::new());
        cell.store(summary("a"));
        let waiter = {
            let cell = Arc::clone(&cell);
            tokio::spawn(
                async move { answer(|| true, Some(&cell), 1, Duration::from_secs(20)).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiter.is_finished(), "no change: the op still holds");
        cell.store(summary("b"));
        let got = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the change ends the hold")
            .unwrap();
        let LocalResponse::FleetSummary {
            seq, summary: s, ..
        } = got
        else {
            panic!("{got:?}");
        };
        assert_eq!(seq, 2);
        assert_eq!(s, Some(summary("b")));
    }

    #[tokio::test]
    async fn local_summary_answers_at_the_end_of_the_hold_with_the_same_seq() {
        let cell = Arc::new(SummaryCell::new());
        cell.store(summary("a"));
        let started = Instant::now();
        let got = answer(|| true, Some(&cell), 1, Duration::from_millis(200)).await;
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(190), "{took:?}");
        assert!(took < Duration::from_secs(5), "{took:?}");
        let LocalResponse::FleetSummary { seq, .. } = got else {
            panic!("{got:?}");
        };
        assert_eq!(seq, 1);
    }

    #[tokio::test]
    async fn local_summary_without_a_node_holds_and_says_down() {
        let started = Instant::now();
        let got = answer(|| true, None, 0, Duration::from_millis(200)).await;
        assert!(started.elapsed() >= Duration::from_millis(190));
        assert_eq!(
            got,
            LocalResponse::FleetSummary {
                seq: 0,
                link: "down".to_owned(),
                held_s: 0,
                summary: None,
            }
        );
    }

    #[tokio::test]
    async fn local_summary_off_answers_at_once() {
        let cell = Arc::new(SummaryCell::new());
        cell.store(summary("a"));
        let started = Instant::now();
        let got = answer(|| false, Some(&cell), 0, Duration::from_secs(20)).await;
        assert_eq!(got, LocalResponse::FleetSummaryOff {});
        assert!(started.elapsed() < Duration::from_secs(5));
        let got = answer(|| false, None, 0, Duration::from_secs(20)).await;
        assert_eq!(got, LocalResponse::FleetSummaryOff {});
    }

    #[tokio::test]
    async fn local_summary_link_change_ends_the_hold() {
        let cell = Arc::new(SummaryCell::new());
        let waiter = {
            let cell = Arc::clone(&cell);
            tokio::spawn(
                async move { answer(|| true, Some(&cell), 0, Duration::from_secs(20)).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let guard = cell.link_up();
        let got = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("a link change ends the hold")
            .unwrap();
        let LocalResponse::FleetSummary {
            seq, link, summary, ..
        } = got
        else {
            panic!("{got:?}");
        };
        assert_eq!((seq, link.as_str(), summary), (1, "up", None));
        drop(guard);
    }
}
