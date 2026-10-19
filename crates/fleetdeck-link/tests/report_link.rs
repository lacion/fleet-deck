//! Setup reports of an unpaired key on the pair channel (BG-3, design
//! `board-gaps.md` section 4): the report window decides, the deck stores
//! nothing for a closed window or a bad frame, and the reporting window never
//! opens the pairing window (nor the reverse).
//!
//! The deck listener is a loopback socket that has the rules of a tailnet
//! listener (`NodeConfig::listener_kind`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod pair_support;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{T, node};
use fleetdeck_core::identity::{Fingerprint, Identity};
use fleetdeck_link::hello::{ReportStatus, feature};
use fleetdeck_link::pair_hub::HubConfig;
use fleetdeck_link::report_accept::{ReportRefusal, ReportReply, ReportSink, ValidReport};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{PairMode, open_pair, send_report};
use futures_util::{SinkExt, StreamExt};
use pair_support::{Deck, Node, ask, code, deck_with, request, result};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const TERMINAL: &str = "0123456789abcdef0123456789abcdef";
const DIGEST: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

/// A sink that records what it gets. The window is a switch.
#[derive(Default)]
struct Recorder {
    open: AtomicBool,
    seen: Mutex<Vec<(Fingerprint, ValidReport, i64)>>,
}

#[async_trait::async_trait]
impl ReportSink for Recorder {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    async fn report(&self, peer: Fingerprint, report: ValidReport, now_ms: i64) -> ReportReply {
        if !self.is_open() {
            return ReportReply::plain(ReportStatus::Disabled, 600);
        }
        self.seen.lock().unwrap().push((peer, report, now_ms));
        ReportReply::plain(ReportStatus::Stored, 600)
    }
}

impl Recorder {
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

async fn reporting_deck(open: bool) -> (Deck, Arc<Recorder>) {
    let sink = Arc::new(Recorder::default());
    sink.open.store(open, Ordering::SeqCst);
    let held = Arc::clone(&sink);
    let deck = deck_with(HubConfig::default(), move |c| {
        c.provision_reports = Some(held as Arc<dyn ReportSink>);
    })
    .await;
    (deck, sink)
}

fn report(name: &str) -> ValidReport {
    ValidReport::validate(TERMINAL, DIGEST, "service_conflict", "validated", name).unwrap()
}

/// A raw frame on a fresh pair connection; returns the next text frame of the
/// deck as JSON, or `None` when the deck closed without one.
async fn raw(station: &Node, deck: &Deck, frames: &[String]) -> Option<Value> {
    let (mut ws, _peer, _est) = open_pair(&station.config, deck.addr(), deck.fp())
        .await
        .unwrap();
    for f in frames {
        if ws.send(Message::text(f.clone())).await.is_err() {
            break;
        }
    }
    loop {
        match tokio::time::timeout(T, ws.next()).await.expect("an answer") {
            Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(t.as_str()).unwrap()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            _ => return None,
        }
    }
}

fn frame(name: &str) -> String {
    json!({
        "type": "provision_report",
        "terminal_id": TERMINAL,
        "state_digest": DIGEST,
        "code": "service_conflict",
        "phase": "validated",
        "name": name,
    })
    .to_string()
}

async fn send(
    station: &Node,
    deck: &Deck,
    r: &ValidReport,
) -> Result<ReportReply, fleetdeck_link::LinkError> {
    send_report(&station.config, deck.addr(), deck.fp(), r, T).await
}

#[tokio::test]
async fn a_report_with_an_open_window_is_stored_for_the_authenticated_key() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    let reply = send(&station, &deck, &report("desk-west")).await.unwrap();
    assert_eq!(reply, ReportReply::plain(ReportStatus::Stored, 600));
    let seen = sink.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    // The fingerprint comes from the key, never from the frame.
    assert_eq!(seen[0].0, station.config.identity.fingerprint());
    assert_eq!(seen[0].1, report("desk-west"));
    assert!(seen[0].2 > 0);
    // A report creates no pairing, pin, grant or invitation.
    assert!(deck.node.config.pins.is_empty());
    assert!(deck.hub.list().await.is_empty());
    assert_eq!(deck.invitations.open_count(), 0);
}

#[tokio::test]
async fn a_closed_window_answers_disabled_and_stores_nothing() {
    let (deck, sink) = reporting_deck(false).await;
    let station = node(Role::Station);
    // The pairing window is open, so TLS lets the key in: the report window
    // is closed, and it stays closed.
    deck.hub.open_window().await.unwrap();
    let reply = send(&station, &deck, &report("a")).await.unwrap();
    assert_eq!(reply.status, ReportStatus::Disabled);
    assert_eq!(sink.count(), 0);
    assert!(!sink.is_open());
}

#[tokio::test]
async fn the_report_window_lets_the_key_through_tls_and_never_opens_pairing() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    assert!(!deck.hub.is_open());
    let reply = send(&station, &deck, &report("a")).await.unwrap();
    assert_eq!(reply.status, ReportStatus::Stored);
    // The same key cannot pair: the pairing window is closed.
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    assert_eq!(code(&result(asked).await), "window_closed");
    assert!(!deck.hub.is_open());
    assert!(deck.node.config.pins.is_empty());
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn with_both_windows_closed_tls_refuses_the_unpaired_key() {
    let (deck, sink) = reporting_deck(false).await;
    let station = node(Role::Station);
    let e = send(&station, &deck, &report("a")).await.unwrap_err();
    assert_eq!(e.reason(), Some("not_accepted"));
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn a_peer_without_the_feature_gets_unsupported() {
    let (deck, sink) = reporting_deck(true).await;
    let mut station = node(Role::Station);
    station
        .config
        .local
        .features
        .retain(|f| f != feature::PROVISION_REPORT);
    // The client function checks the feature first.
    let e = send(&station, &deck, &report("a")).await.unwrap_err();
    assert_eq!(e.reason(), Some("unsupported"));
    // A raw frame without the negotiated feature is refused by the deck.
    let answer = raw(&station, &deck, &[frame("a")]).await.unwrap();
    assert_eq!(answer["type"], "close");
    assert_eq!(answer["reason"], "unsupported");
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn a_large_frame_and_a_bad_field_close_with_invalid_request() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    let big = raw(&station, &deck, &[frame(&"a".repeat(5 * 1024))]).await;
    assert_eq!(big.unwrap()["reason"], "invalid_request");
    for (field, value) in [
        ("terminal_id", "XYZ"),
        ("state_digest", "ABCDEF"),
        ("phase", "later"),
        ("name", "\u{202e}\u{7}"),
    ] {
        let mut v: Value = serde_json::from_str(&frame("ok")).unwrap();
        v[field] = json!(value);
        let answer = raw(&station, &deck, &[v.to_string()]).await.unwrap();
        assert_eq!(answer["reason"], "invalid_request", "{field}");
    }
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn a_second_frame_on_the_connection_is_refused() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    let (mut ws, _peer, _est) = open_pair(&station.config, deck.addr(), deck.fp())
        .await
        .unwrap();
    ws.send(Message::text(frame("a"))).await.unwrap();
    let first = loop {
        match tokio::time::timeout(T, ws.next()).await.unwrap() {
            Some(Ok(Message::Text(t))) => break t,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("expected the reply, got {other:?}"),
        }
    };
    let first: Value = serde_json::from_str(first.as_str()).unwrap();
    assert_eq!(first["type"], "provision_report_reply");
    // The deck closed: a second frame gets no second reply.
    let _ = ws.send(Message::text(frame("b"))).await;
    loop {
        match tokio::time::timeout(T, ws.next()).await.unwrap() {
            Some(Ok(Message::Text(t))) => panic!("a second reply: {t}"),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            _ => break,
        }
    }
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn a_frame_with_a_field_that_is_missing_stores_nothing() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    let mut v: Value = serde_json::from_str(&frame("a")).unwrap();
    v.as_object_mut().unwrap().remove("phase");
    let answer = raw(&station, &deck, &[v.to_string()]).await;
    // The bad frame ends the connection; it never reaches the sink.
    assert!(answer.is_none_or(|a| a["type"] == "close"));
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn a_pinned_station_key_gets_already_paired() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    deck.node
        .config
        .pins
        .pin(station.config.identity.public_key(), Role::Station);
    let e = send(&station, &deck, &report("a")).await.unwrap_err();
    assert_eq!(e.reason(), Some("already_paired"));
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn a_wrong_deck_pin_is_fingerprint_mismatch() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    let other = Identity::generate().unwrap().fingerprint();
    let e = send_report(&station.config, deck.addr(), other, &report("a"), T)
        .await
        .unwrap_err();
    assert_eq!(e.reason(), Some("fingerprint_mismatch"));
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn a_listener_that_is_not_tailnet_answers_disabled() {
    let (deck, sink) = reporting_deck(true).await;
    let station = node(Role::Station);
    // An open invitation lets an unpaired key through TLS on a LAN listener.
    let lan = deck.listen_as(fleetdeck_link::ListenerKind::Other).await;
    let invitation = deck
        .invitations
        .create(
            lan.local_addr,
            deck.fp(),
            std::time::Duration::from_secs(60),
            [fleetdeck_proto::link::Scope::Read],
        )
        .unwrap();
    assert_eq!(invitation.fingerprint, deck.fp());
    let reply = send_report(&station.config, lan.local_addr, deck.fp(), &report("a"), T)
        .await
        .unwrap();
    assert_eq!(reply.status, ReportStatus::Disabled);
    assert_eq!(sink.count(), 0);
}

#[test]
fn the_validator_applies_the_rules_of_the_design() {
    let ok = ValidReport::validate(TERMINAL, DIGEST, "denied", "link", "desk").unwrap();
    assert_eq!(ok.code, "denied");
    // An unknown code is stored as `unknown_error`.
    let r = ValidReport::validate(TERMINAL, DIGEST, "made_up", "link", "desk").unwrap();
    assert_eq!(r.code, "unknown_error");
    // Control and direction characters go; the name is cut to 64 characters.
    let r = ValidReport::validate(TERMINAL, DIGEST, "denied", "link", "a\u{202e}b\nc").unwrap();
    assert_eq!(r.name, "abc");
    let long = "é".repeat(80);
    let r = ValidReport::validate(TERMINAL, DIGEST, "denied", "link", &long).unwrap();
    assert_eq!(r.name.chars().count(), 64);
    let bad = |t: &str, d: &str, p: &str, n: &str| {
        ValidReport::validate(t, d, "denied", p, n).unwrap_err()
    };
    assert_eq!(bad("0", DIGEST, "link", "n"), ReportRefusal::TerminalId);
    assert_eq!(
        bad(&TERMINAL.to_uppercase(), DIGEST, "link", "n"),
        ReportRefusal::TerminalId
    );
    assert_eq!(bad(TERMINAL, "ab", "link", "n"), ReportRefusal::StateDigest);
    assert_eq!(bad(TERMINAL, DIGEST, "done", "n"), ReportRefusal::Phase);
    assert_eq!(bad(TERMINAL, DIGEST, "link", "\u{7}"), ReportRefusal::Name);
    assert_eq!(bad(TERMINAL, DIGEST, "link", ""), ReportRefusal::Name);
}

#[test]
fn a_reply_needs_the_retry_fields_with_retry_only() {
    let retry = ReportReply::retry(TERMINAL.to_owned(), TERMINAL.to_owned(), DIGEST.to_owned());
    assert!(retry.clone().checked().is_some());
    let mut bad = retry;
    bad.retry_id = None;
    assert!(bad.checked().is_none());
    let mut stored = ReportReply::plain(ReportStatus::Stored, 600);
    assert!(stored.clone().checked().is_some());
    stored.retry_id = Some(TERMINAL.to_owned());
    assert!(stored.checked().is_none());
}
