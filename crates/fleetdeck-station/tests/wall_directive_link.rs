//! The deck control for `display.directive` (R13 P4 task E): the station
//! keeps the control of a link that negotiated `desktop_shell.v1` in its
//! summary cell, clears it when that link ends, and the wall op sends one
//! request for each wall over it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::Inbound;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::local_wall::{WallResult, send_all};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

struct Fixture {
    _det: Det,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
}

async fn fixture() -> Fixture {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store.clone(),
        policy,
        Adapters::new(),
        det.ids(),
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let deck = Arc::new(Identity::generate().unwrap());
    store
        .pin_peer(PairedKey {
            key: deck.public_key(),
            role: Role::Deck,
            scopes: BTreeSet::from([Scope::Read, Scope::Drive]),
            dial_url: None,
        })
        .await
        .unwrap();
    let peers = StationPeers::load(store.clone()).await.unwrap();
    let station_id = Arc::new(Identity::generate().unwrap());
    let node = NodeConfig::new(
        Arc::clone(&station_id),
        LocalHello::new(Role::Station, store.incarnation().clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&peers));
    peers.pin_all(&node.pins).await.unwrap();
    let listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    // Every paired station holds `admin` at the deck.
    let grants = Grants::new();
    grants.grant(station_id.fingerprint(), BTreeSet::from([Scope::Admin]));
    let deck_node = NodeConfig::new(
        Arc::clone(&deck),
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(grants);
    deck_node.pins.pin(station_id.public_key(), Role::Station);
    let ctx = Arc::new(StationLink {
        manager: Arc::new(manager),
        node,
        peers,
        name: "box".to_owned(),
        shells: Arc::new(fleetdeck_station::shell::Shells::new(ShellConfig::default())),
        coordinator: fleetdeck_station::coordinator::LinkForward::default(),
        paste_dir: None,
        summary: Arc::default(),
        ready: Arc::default(),
        fleet_git: Arc::new(fleetdeck_station::fleet_git_runtime::FleetGitRuntime::new(
            Arc::new(fleetdeck_station::workspace::GitOptions::isolated),
        )),
    });
    Fixture {
        _det: det,
        ctx,
        listener,
        deck_node,
    }
}

impl Fixture {
    /// The deck dials; the station serves the link. Returns the deck side.
    async fn connect(&mut self) -> LiveLink {
        let peer = self.ctx.node.identity.public_key();
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&self.deck_node, self.listener.local_addr, peer),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_station = fleetdeck_link::start(&self.ctx.node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_station));
        fleetdeck_link::start(&self.deck_node, d.unwrap())
    }

    /// Waits until the slot has (or has not) a control.
    async fn slot(&self, present: bool) {
        let deadline = tokio::time::Instant::now() + T;
        while self.ctx.summary.deck().is_some() != present {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the slot never became {present}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn the_wall_slot_follows_a_link_with_the_desktop_shell_feature() {
    let mut f = fixture().await;
    assert!(f.ctx.summary.deck().is_none());
    let deck = f.connect().await;
    f.slot(true).await;
    deck.control.close("test", "the deck ends the link");
    drop(deck);
    f.slot(false).await;
}

#[tokio::test]
async fn the_wall_op_sends_one_display_directive_for_each_wall_over_the_link() {
    let mut f = fixture().await;
    let mut deck = f.connect().await;
    f.slot(true).await;
    // The deck answers the first request, and refuses the second.
    let answers = tokio::spawn(async move {
        let mut seen = Vec::new();
        while seen.len() < 2 {
            match deck.inbound.recv().await {
                Some(Inbound::Request(req)) if req.method == "display.directive" => {
                    let outcome = if seen.is_empty() {
                        Outcome::Ok(json!({"generation": 1}))
                    } else {
                        Outcome::Err(WireError::new("not_bound", "No."))
                    };
                    seen.push(req.params.clone());
                    deck.control.respond(req.id, outcome).await.unwrap();
                }
                Some(_) => {}
                None => break,
            }
        }
        (seen, deck)
    });
    let walls = vec!["d1".to_owned(), "d2".to_owned()];
    let control = f.ctx.summary.deck();
    let results = tokio::time::timeout(T, send_all(&walls, control.as_ref(), "next_mode"))
        .await
        .expect("results");
    assert_eq!(
        results,
        vec![
            WallResult {
                display_id: "d1".to_owned(),
                code: None,
                text: None
            },
            WallResult {
                display_id: "d2".to_owned(),
                code: Some("not_bound".to_owned()),
                text: Some("No.".to_owned())
            },
        ]
    );
    let (seen, _deck) = answers.await.unwrap();
    assert_eq!(
        seen,
        vec![
            json!({"display_id": "d1", "intent": "next_mode"}),
            json!({"display_id": "d2", "intent": "next_mode"}),
        ]
    );
}

#[tokio::test]
async fn the_wall_op_reports_the_deck_down_when_the_link_has_ended() {
    let mut f = fixture().await;
    let deck = f.connect().await;
    f.slot(true).await;
    let control = f.ctx.summary.deck();
    deck.control.close("test", "the deck ends the link");
    drop(deck);
    f.slot(false).await;
    // A control that was read before the link ended, and no control at all.
    let walls = vec!["d1".to_owned()];
    let down = vec![WallResult {
        display_id: "d1".to_owned(),
        code: Some("deck_down".to_owned()),
        text: Some("The deck is not connected.".to_owned()),
    }];
    assert_eq!(send_all(&walls, None, "next_mode").await, down);
    assert_eq!(send_all(&walls, control.as_ref(), "next_mode").await, down);
}
