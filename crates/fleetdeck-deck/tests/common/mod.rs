//! A fake station linked to an in-process deck link context, with the
//! router tick at 100 ms (M2 P3-7). Used by the router, outbox and
//! coordinator tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_deck::link::{self, CommandKind, DeckLink};
use fleetdeck_deck::store::DeckCommand;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::{DeckPeers, DeckStore};
use fleetdeck_link::control::{Inbound, InboundRequest};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::command::{
    CommandOrigin, DeckCommandState, ReceiptState, Reject, SessionCommand,
};
use fleetdeck_proto::link::{ControlFrame, Outcome, Scope, StreamMode, StreamStart};
use fleetdeck_proto::{CommandId, IncarnationId, Receipt, RejectCode, StationId};
use fleetdeck_testkit::Det;
use serde_json::json;

pub mod board;

pub const T: Duration = Duration::from_secs(10);

pub struct Fixture {
    pub det: Det,
    pub store: DeckStore,
    pub ctx: Arc<DeckLink>,
    pub deck_node: NodeConfig,
    pub deck_key: PeerKey,
    pub listener: LinkListener,
    pub station_node: NodeConfig,
    pub station: StationId,
    pub inc: IncarnationId,
    pub synced: bool,
}

pub async fn fixture() -> Fixture {
    fixture_with(STATION_SCOPES.into_iter().collect()).await
}

/// A fake station that the deck grants `scopes`.
pub async fn fixture_with(scopes: BTreeSet<Scope>) -> Fixture {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let deck_id = Arc::new(Identity::generate().unwrap());
    let station_id = Arc::new(Identity::generate().unwrap());
    let rec = store
        .pin_station(NewStation {
            key: station_id.public_key(),
            name: "fake".to_owned(),
            scopes,
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
    let listener = node::listen(deck_node.clone(), "127.0.0.1:0".parse().unwrap())
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
    let ctx = Arc::new(DeckLink::new(store.clone(), peers, deck_node.clone()));
    // The router tick of `DeckNode`, faster.
    let ticker = Arc::clone(&ctx);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            ticker.tick().await;
        }
    });
    Fixture {
        det,
        store,
        ctx,
        deck_node,
        deck_key: deck_id.public_key(),
        listener,
        station_node,
        station: rec.station_id,
        inc,
        synced: false,
    }
}

pub async fn next_request(station: &mut LiveLink) -> InboundRequest {
    loop {
        match tokio::time::timeout(T, station.inbound.recv())
            .await
            .expect("a request")
        {
            // The deck asks every new link for `station.info` (P4-2) on its
            // own task; a fake station leaves it unanswered.
            Some(Inbound::Request(req)) if req.method == "station.info" => {}
            Some(Inbound::Request(req)) => return req,
            Some(Inbound::Frame(_)) => {}
            None => panic!("the link ended"),
        }
    }
}

/// No request arrives within `wait`.
pub async fn no_request(station: &mut LiveLink, wait: Duration) {
    let got = tokio::time::timeout(wait, async {
        loop {
            match station.inbound.recv().await {
                Some(Inbound::Request(req)) if req.method == "station.info" => {}
                Some(Inbound::Request(req)) => return Some(req),
                Some(Inbound::Frame(_)) => {}
                None => return None,
            }
        }
    })
    .await;
    if let Ok(Some(req)) = got {
        panic!("unexpected request {} {}", req.method, req.params);
    }
}

pub fn accepted(id: &CommandId) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Accepted,
        station_seq: Some(1),
        result: None,
        reject: None,
        at: 0,
    }))
}

pub fn rejected(id: &CommandId, code: RejectCode) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Rejected,
        station_seq: None,
        result: None,
        reject: Some(Reject {
            code,
            message: "no".to_owned(),
            unsupported: None,
        }),
        at: 0,
    }))
}

pub fn interrupt() -> CommandKind {
    CommandKind::Session {
        command: SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    }
}

impl Fixture {
    /// Links the fake station to the deck. The first link answers the
    /// deck's replay with an empty snapshot (the deck learns the
    /// incarnation); later links answer with a replay.
    pub async fn connect(&mut self) -> LiveLink {
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&self.station_node, self.listener.local_addr, self.deck_key),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_deck = fleetdeck_link::start(&self.deck_node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_deck));
        let mut station = fleetdeck_link::start(&self.station_node, d.unwrap());
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "events.replay");
        let mode = if self.synced {
            StreamMode::Replay
        } else {
            StreamMode::Snapshot
        };
        let start = StreamStart {
            mode,
            incarnation: self.inc.clone(),
            from: 0,
        };
        station
            .control
            .respond(req.id, Outcome::Ok(json!(start)))
            .await
            .unwrap();
        if !self.synced {
            for frame in [
                ControlFrame::SnapshotBegin {
                    incarnation: self.inc.clone(),
                    watermark: 0,
                    session_count: 0,
                },
                ControlFrame::SnapshotEnd {
                    incarnation: self.inc.clone(),
                    watermark: 0,
                    coordinator: None,
                },
            ] {
                station.control.send(frame).await.unwrap();
            }
            self.wait("the snapshot", |f| {
                let store = f.store.clone();
                let id = f.station.clone();
                async move {
                    store
                        .station(id)
                        .await
                        .unwrap()
                        .unwrap()
                        .incarnation
                        .is_some()
                }
            })
            .await;
            self.synced = true;
        }
        station
    }

    /// Closes the fake station's link and waits until the deck sees it.
    pub async fn drop_link(&self, station: LiveLink) {
        station.control.close("test", "the link drops");
        let _ = station.control.closed().await;
        let store = self.store.clone();
        let id = self.station.clone();
        self.wait("the station unreachable", move |_| {
            let (store, id) = (store.clone(), id.clone());
            async move { !store.is_reachable(&id) }
        })
        .await;
    }

    pub async fn wait<F, Fut>(&self, what: &str, f: F)
    where
        F: Fn(&Self) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + T;
        while !f(self).await {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn submit(&self) -> DeckCommand {
        self.ctx
            .submit(self.station.clone(), CommandOrigin::Board, interrupt())
            .await
            .unwrap()
    }

    pub async fn command(&self, id: &CommandId) -> DeckCommand {
        self.store.command(id.clone()).await.unwrap().unwrap()
    }

    pub async fn settled(&self, id: &CommandId) -> DeckCommand {
        let cmd = self.ctx.wait_command(id.clone(), T).await.unwrap();
        assert_ne!(cmd.state, DeckCommandState::Pending, "{cmd:?}");
        cmd
    }

    /// Answers one request and returns it.
    pub async fn answer(
        station: &mut LiveLink,
        outcome: impl FnOnce(&CommandId) -> Outcome,
    ) -> InboundRequest {
        let req = next_request(station).await;
        let id = req.command_id.clone().expect("a command id");
        station.control.respond(req.id, outcome(&id)).await.unwrap();
        req
    }
}
