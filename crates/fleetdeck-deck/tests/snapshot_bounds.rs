//! The deck bounds an unfinished snapshot batch (M2 round 23 review;
//! DL-051): a `snapshot_begin` above `MAX_SNAPSHOT_SESSIONS` and a
//! `snapshot_session` beyond the announced count close the link with
//! `protocol_error` and store nothing. A fake station sends raw frames.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_deck::link::{self, DeckLink};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::{DeckPeers, DeckStore};
use fleetdeck_link::control::Inbound;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{
    ControlFrame, MAX_SNAPSHOT_SESSIONS, Outcome, Scope, SessionSnapshot, StreamMode, StreamStart,
};
use fleetdeck_proto::session::SessionSummary;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

struct Fixture {
    store: DeckStore,
    /// The fake station's side of the link.
    station: LiveLink,
    inc: IncarnationId,
}

async fn fixture() -> Fixture {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    let store = DeckStore::open_in_memory(clock, ids).await.unwrap();
    let deck_id = Arc::new(Identity::generate().unwrap());
    let station_id = Arc::new(Identity::generate().unwrap());
    store
        .pin_station(NewStation {
            key: station_id.public_key(),
            name: "fake".to_owned(),
            scopes: STATION_SCOPES.into_iter().collect(),
            dial_url: None,
        })
        .await
        .unwrap();
    let peers = DeckPeers::load(store.clone()).await.unwrap();
    let deck_node = NodeConfig::new(
        Arc::clone(&deck_id),
        LocalHello::new(Role::Deck, store.incarnation().clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&peers));
    peers.pin_all(&deck_node.pins).await.unwrap();
    let mut listener = node::listen(deck_node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    let inc = IncarnationId::from("fake-station-db");
    let grants = Grants::new();
    grants.grant(
        deck_id.fingerprint(),
        BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
    );
    let station_node = NodeConfig::new(
        station_id,
        LocalHello::new(Role::Station, inc.clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(grants);
    station_node.pins.pin(deck_id.public_key(), Role::Deck);

    let (d, a) = tokio::time::timeout(T, async {
        tokio::join!(
            node::dial(&station_node, listener.local_addr, deck_id.public_key()),
            listener.next_link()
        )
    })
    .await
    .expect("link setup");
    let ctx = Arc::new(DeckLink::new(store.clone(), peers, deck_node.clone()));
    let at_deck = fleetdeck_link::start(&deck_node, a.expect("listener"));
    tokio::spawn(link::serve(ctx, at_deck));
    let mut station = fleetdeck_link::start(&station_node, d.unwrap());

    // The deck asks for a replay; the fake station accepts it. It leaves the
    // deck's `station.info` request (P4-2) unanswered.
    let req = loop {
        let Some(Inbound::Request(req)) = tokio::time::timeout(T, station.inbound.recv())
            .await
            .expect("the replay request")
        else {
            panic!("expected a request");
        };
        if req.method != "station.info" {
            break req;
        }
    };
    assert_eq!(req.method, "events.replay");
    let start = StreamStart {
        mode: StreamMode::Snapshot,
        incarnation: inc.clone(),
        from: 0,
    };
    station
        .control
        .respond(req.id, Outcome::Ok(json!(start)))
        .await
        .unwrap();
    Fixture {
        store,
        station,
        inc,
    }
}

fn begin(inc: &IncarnationId, count: u64) -> ControlFrame {
    ControlFrame::SnapshotBegin {
        incarnation: inc.clone(),
        watermark: 7,
        session_count: count,
    }
}

fn session(inc: &IncarnationId, id: &str) -> ControlFrame {
    ControlFrame::SnapshotSession {
        incarnation: inc.clone(),
        snapshot: SessionSnapshot {
            summary: SessionSummary::new(id.into()),
        },
    }
}

impl Fixture {
    async fn send(&self, frame: ControlFrame) {
        self.station.control.send(frame).await.unwrap();
    }

    async fn closed_with_protocol_error(&self, what: &str) {
        let closing = tokio::time::timeout(T, self.station.control.closed())
            .await
            .expect("the deck closes the link");
        assert_eq!(
            closing.reason, "protocol_error",
            "{what}: {}",
            closing.detail
        );
        assert!(self.store.sessions().await.unwrap().is_empty(), "{what}");
    }
}

#[tokio::test]
async fn a_snapshot_begin_above_the_session_limit_closes_the_link() {
    let f = fixture().await;
    f.send(begin(&f.inc, MAX_SNAPSHOT_SESSIONS + 1)).await;
    f.closed_with_protocol_error("count above the limit").await;
}

#[tokio::test]
async fn a_snapshot_session_beyond_the_announced_count_closes_the_link() {
    let f = fixture().await;
    f.send(begin(&f.inc, 1)).await;
    f.send(session(&f.inc, "s1")).await;
    f.send(session(&f.inc, "s2")).await;
    f.closed_with_protocol_error("a session beyond the count")
        .await;
}

#[tokio::test]
async fn a_complete_batch_at_the_announced_count_is_applied() {
    let f = fixture().await;
    f.send(begin(&f.inc, 2)).await;
    f.send(session(&f.inc, "s1")).await;
    f.send(session(&f.inc, "s2")).await;
    f.send(ControlFrame::SnapshotEnd {
        incarnation: f.inc.clone(),
        watermark: 7,
        coordinator: None,
    })
    .await;
    let Some(Inbound::Frame(ack)) = tokio::time::timeout(T, async {
        let mut station = f.station;
        station.inbound.recv().await
    })
    .await
    .expect("an ack") else {
        panic!("expected a frame");
    };
    assert_eq!(
        *ack,
        ControlFrame::Ack {
            incarnation: f.inc.clone(),
            station_seq: 7
        }
    );
    let ids: Vec<String> = f
        .store
        .sessions()
        .await
        .unwrap()
        .iter()
        .map(|s| s.summary.session_id.as_str().to_owned())
        .collect();
    assert_eq!(ids, ["s1", "s2"]);
}
