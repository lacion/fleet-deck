//! The text of a scope refusal comes from the grant source (G6, design
//! `grant-idle.md` §3.6).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A source with no scope that counts its `refusal` calls.
#[derive(Debug)]
struct Why {
    text: bool,
    asked: AtomicUsize,
}

impl GrantSource for Why {
    fn scopes(&self, _peer: &Fingerprint) -> BTreeSet<Scope> {
        BTreeSet::new()
    }

    fn refusal(
        &self,
        _peer: &Fingerprint,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let caller = params["caller"].as_str()?;
        self.text.then(|| format!("why {method} {caller}"))
    }
}

/// A deck side with `source`; the test drives the raw station peer.
async fn with_source(source: &Arc<Why>) -> (Side, Raw) {
    let (a, raw) = ws_pair(1 << 20).await;
    let peer = Identity::generate().unwrap().fingerprint();
    let params = ChannelParams {
        local: Role::Deck,
        peer_role: Role::Station,
        peer,
        negotiated: negotiated(feature::ALL),
        local_desktop_epoch: None,
        peer_desktop_epoch: None,
        grants: Arc::clone(source) as Arc<dyn GrantSource>,
        config: ControlConfig::default(),
    };
    let (table, closing) = watch::channel(None);
    let (control, inbound) = spawn(a, params, closing, None);
    let side = Side {
        control,
        inbound,
        grants: Grants::new(),
        peer,
        table,
    };
    (side, raw)
}

fn query(id: u64, method: &str) -> ControlFrame {
    ControlFrame::Request {
        id,
        command_id: None,
        method: method.to_owned(),
        params: json!({ "caller": "c1" }),
    }
}

async fn refusal_of(raw: &mut Raw) -> WireError {
    match recv_raw(raw).await {
        Some(ControlFrame::Response {
            outcome: Outcome::Err(e),
            ..
        }) => e,
        other => panic!("expected an error response, got {other:?}"),
    }
}

#[tokio::test]
async fn a_scope_refusal_takes_the_text_of_the_grant_source() {
    let source = Arc::new(Why {
        text: true,
        asked: AtomicUsize::new(0),
    });
    let (_s, mut raw) = with_source(&source).await;
    send_raw(&mut raw, &query(1, "coordinator.query")).await;
    let e = refusal_of(&mut raw).await;
    assert_eq!(e.code, "forbidden");
    assert_eq!(e.message, "why coordinator.query c1");
}

#[tokio::test]
async fn a_source_without_a_text_keeps_the_generic_refusal() {
    let source = Arc::new(Why {
        text: false,
        asked: AtomicUsize::new(0),
    });
    let (_s, mut raw) = with_source(&source).await;
    send_raw(&mut raw, &query(1, "coordinator.query")).await;
    let e = refusal_of(&mut raw).await;
    assert_eq!(e.code, "forbidden");
    assert_eq!(
        e.message,
        "`coordinator.query` needs scope coordinate, which the pairing did not grant"
    );
    assert_eq!(source.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_source_is_asked_only_for_a_scope_refusal() {
    let source = Arc::new(Why {
        text: true,
        asked: AtomicUsize::new(0),
    });
    let (_s, mut raw) = with_source(&source).await;
    // `station.info` goes from the deck to the station: a station that sends
    // it to the deck has the wrong role.
    send_raw(&mut raw, &query(1, "station.info")).await;
    let e = refusal_of(&mut raw).await;
    assert_eq!(e.code, "wrong_role");
    assert_eq!(source.asked.load(Ordering::SeqCst), 0);
}
