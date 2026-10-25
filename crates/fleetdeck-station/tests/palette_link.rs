//! The station palette over a loopback link (R13 P4 task F1, design
//! `omarchy-p4.md` §3.7): the station keeps the palette of `theme-sync` in
//! memory, sends `station.palette` over the link that negotiated
//! `desktop_shell.v1`, and sends it again at the next link up.

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
use fleetdeck_proto::palette::{PaletteColors, PaletteMode, StationPalette};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::local_palette::store_and_send;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

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

fn palette(background: &str) -> StationPalette {
    StationPalette {
        v: 1,
        mode: PaletteMode::Dark,
        colors: PaletteColors {
            background: background.to_owned(),
            foreground: "#ffffff".to_owned(),
            accent: "#112233".to_owned(),
            muted: "#222222".to_owned(),
            dark_background: "#010101".to_owned(),
            dark_foreground: "#fefefe".to_owned(),
            bright_foreground: "#ffffff".to_owned(),
            selection: "#334455".to_owned(),
            red: "#ff0000".to_owned(),
            yellow: "#ffff00".to_owned(),
        },
    }
}

/// Answers the next `station.palette` request of the station with `outcome`
/// and returns its params. Other requests are skipped.
async fn answer_palette(deck: &mut LiveLink, outcome: Outcome) -> Value {
    loop {
        match tokio::time::timeout(T, deck.inbound.recv())
            .await
            .expect("a palette request")
        {
            Some(Inbound::Request(req)) if req.method == "station.palette" => {
                let params = req.params.clone();
                deck.control.respond(req.id, outcome).await.unwrap();
                return params;
            }
            Some(_) => {}
            None => panic!("the link ended"),
        }
    }
}

/// True when no `station.palette` request comes within `wait`.
async fn no_palette(deck: &mut LiveLink, wait: Duration) -> bool {
    let end = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(end, deck.inbound.recv()).await {
            Err(_) => return true,
            Ok(Some(Inbound::Request(req))) if req.method == "station.palette" => return false,
            Ok(Some(_)) => {}
            Ok(None) => return true,
        }
    }
}

fn ok() -> Outcome {
    Outcome::Ok(json!({"ok": true}))
}

#[tokio::test]
async fn the_op_keeps_the_palette_and_sends_it_over_the_link() {
    let mut f = fixture().await;
    let mut deck = f.connect().await;
    f.slot(true).await;
    let sent = tokio::spawn({
        let ctx = Arc::clone(&f.ctx);
        async move { store_and_send(&ctx.summary, palette("#000000")).await }
    });
    let params = answer_palette(&mut deck, ok()).await;
    assert_eq!(params, serde_json::to_value(palette("#000000")).unwrap());
    assert!(sent.await.unwrap(), "the deck answered ok");
    assert_eq!(f.ctx.summary.palette(), Some(palette("#000000")));
}

#[tokio::test]
async fn a_refusal_of_the_deck_is_not_sent_but_the_palette_is_kept() {
    let mut f = fixture().await;
    let mut deck = f.connect().await;
    f.slot(true).await;
    let sent = tokio::spawn({
        let ctx = Arc::clone(&f.ctx);
        async move { store_and_send(&ctx.summary, palette("#123456")).await }
    });
    let refusal = Outcome::Err(WireError::new(
        "invalid_params",
        "The palette is not valid.",
    ));
    answer_palette(&mut deck, refusal).await;
    assert!(!sent.await.unwrap());
    assert_eq!(f.ctx.summary.palette(), Some(palette("#123456")));
}

#[tokio::test]
async fn with_no_link_the_palette_is_kept_and_not_sent() {
    let f = fixture().await;
    assert!(!store_and_send(&f.ctx.summary, palette("#abcdef")).await);
    assert_eq!(f.ctx.summary.palette(), Some(palette("#abcdef")));
}

#[tokio::test]
async fn the_station_sends_the_kept_palette_again_at_the_next_link_up() {
    let mut f = fixture().await;
    // No palette is kept: a link up sends nothing.
    let mut first = f.connect().await;
    f.slot(true).await;
    assert!(no_palette(&mut first, Duration::from_millis(500)).await);
    // The palette is kept while the link is down.
    first.control.close("test", "the deck ends the link");
    drop(first);
    f.slot(false).await;
    assert!(!store_and_send(&f.ctx.summary, palette("#0a0b0c")).await);
    // The next link up sends it once, with the same shape.
    let mut second = f.connect().await;
    f.slot(true).await;
    let params = answer_palette(&mut second, ok()).await;
    assert_eq!(params, serde_json::to_value(palette("#0a0b0c")).unwrap());
    assert!(no_palette(&mut second, Duration::from_millis(500)).await);
    // A newer palette is the one that the following link up sends.
    second.control.close("test", "the deck ends the link");
    drop(second);
    f.slot(false).await;
    assert!(!store_and_send(&f.ctx.summary, palette("#0d0e0f")).await);
    let mut third = f.connect().await;
    f.slot(true).await;
    let params = answer_palette(&mut third, ok()).await;
    assert_eq!(params, serde_json::to_value(palette("#0d0e0f")).unwrap());
}
