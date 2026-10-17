//! `desktop.info` and `desktop.act` on the link (R13 P3a, feature
//! `desktop.v1`): the method rows, and a station without a desktop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Identity;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::methods::{authorize, code, method};
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope, StationInfo};
use fleetdeck_proto::version::{Negotiated, PROTO_MAX, feature};
use fleetdeck_station::desktop::{self, Desktop};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);

fn negotiated(features: &[&str]) -> Negotiated {
    Negotiated {
        version: PROTO_MAX,
        features: features.iter().map(|f| (*f).to_owned()).collect(),
    }
}

#[test]
fn the_method_rows_are_deck_to_station_rows_of_the_desktop_feature() {
    let info = method("desktop.info").unwrap();
    assert_eq!(info.scope, Scope::Read);
    assert_eq!(info.feature, Some(feature::DESKTOP));
    assert!(!info.mutating);
    let act = method("desktop.act").unwrap();
    assert_eq!(act.scope, Scope::Drive);
    // The deck authorizes each action (R13 §5E item 1).
    assert_eq!(act.feature, Some(feature::DESKTOP_AUTHORITY));
    assert!(!act.mutating);
    assert_eq!(feature::DESKTOP, "desktop.v1");
    assert_eq!(feature::DESKTOP_AUTHORITY, "desktop_authority.v1");
    assert!(feature::ALL.contains(&feature::DESKTOP));
    assert!(feature::ALL.contains(&feature::DESKTOP_AUTHORITY));
}

#[test]
fn the_methods_need_the_feature_and_the_scope() {
    let without = negotiated(&[feature::CONTROL]);
    let with = negotiated(&[
        feature::CONTROL,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
    ]);
    let read: BTreeSet<Scope> = [Scope::Read].into_iter().collect();
    let drive: BTreeSet<Scope> = [Scope::Read, Scope::Drive].into_iter().collect();
    let call = |name: &str, n: &Negotiated, scopes: &BTreeSet<Scope>| {
        authorize(name, Role::Deck, Role::Station, None, n, Some(scopes))
            .map(|_| ())
            .map_err(|e| e.code)
    };
    for name in ["desktop.info", "desktop.act"] {
        assert_eq!(
            call(name, &without, &drive),
            Err(code::FEATURE_NOT_NEGOTIATED.to_owned())
        );
    }
    assert_eq!(call("desktop.info", &with, &read), Ok(()));
    assert_eq!(call("desktop.act", &with, &drive), Ok(()));
    assert_eq!(
        call("desktop.act", &with, &read),
        Err(code::FORBIDDEN.to_owned())
    );
    // A station never sends them.
    let wrong = authorize(
        "desktop.act",
        Role::Station,
        Role::Deck,
        None,
        &with,
        Some(&drive),
    );
    assert_eq!(wrong.unwrap_err().code, code::WRONG_ROLE);
}

struct Fixture {
    _det: Det,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
}

/// A station on a loopback link, and a process that has no desktop: the
/// adapter gets an empty environment.
async fn fixture() -> Fixture {
    desktop::install(Arc::new(Desktop::with_env(ChildEnv::empty())));
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
    let deck_node = NodeConfig::new(
        Arc::clone(&deck),
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Grants::new());
    deck_node.pins.pin(station_id.public_key(), Role::Station);
    let ctx = Arc::new(StationLink {
        manager: Arc::new(manager),
        node,
        peers,
        name: "box".to_owned(),
        shells: Arc::new(fleetdeck_station::shell::Shells::new(ShellConfig::default())),
        coordinator: fleetdeck_station::coordinator::LinkForward::default(),
        paste_dir: None,
    });
    Fixture {
        _det: det,
        ctx,
        listener,
        deck_node,
    }
}

impl Fixture {
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
}

async fn call(deck: &LiveLink, method: &str, params: Value) -> Outcome {
    tokio::time::timeout(T, deck.control.request(method, None, params))
        .await
        .expect("response")
        .unwrap()
}

fn refused(o: Outcome) -> (String, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

// The fixtures replace the process-global adapter: one test at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn a_station_without_a_desktop_refuses_the_methods_and_has_no_desktop_field() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    let (code, message) = refused(call(&deck, "desktop.info", json!({})).await);
    assert_eq!(code, "unsupported");
    assert_eq!(message, "This station has no desktop.");
    // A `desktop.act` without a valid authority is refused first, also on a
    // station without a desktop (R13 §5E item 1, DL-230).
    let (code, message) =
        refused(call(&deck, "desktop.act", json!({"action": "screen_lock"})).await);
    assert_eq!(code, "forbidden");
    assert_eq!(message, "The deck did not authorize this desktop action.");
    // `station.info` carries no `desktop` field.
    let Outcome::Ok(raw) = call(&deck, "station.info", json!({})).await else {
        panic!("station.info failed");
    };
    assert!(raw.get("desktop").is_none(), "{raw}");
    let info: StationInfo = serde_json::from_value(raw).unwrap();
    assert!(info.desktop.is_none());
}

#[tokio::test]
async fn a_bad_action_is_invalid_params() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    for params in [
        json!({"action": "exec", "cmd": "id"}),
        json!({"action": "screen_dnd", "on": true, "extra": 1}),
        json!({"action": "screen_dnd"}),
        json!({}),
    ] {
        let (code, _) = refused(call(&deck, "desktop.act", params.clone()).await);
        assert_eq!(code, "invalid_params", "{params}");
    }
}

#[tokio::test]
async fn the_step_one_stub_refuses_an_action_with_an_authority_and_a_fresh_info() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut f = fixture().await;
    let deck = f.connect().await;
    let authority = json!({
        "action_id": "01J00000000000000000000000",
        "action_hash": "ab".repeat(32),
        "deadline_ms": 1,
        "deck_now_ms": 1,
        "desktop_epoch": "cd".repeat(16),
    });
    let (code, message) = refused(
        call(
            &deck,
            "desktop.act",
            json!({"action": "screen_lock", "authority": authority}),
        )
        .await,
    );
    assert_eq!(code, "forbidden");
    assert_eq!(message, "The deck did not authorize this desktop action.");
    // A malformed authority is a parse error, not an authorization.
    let (code, _) = refused(
        call(
            &deck,
            "desktop.act",
            json!({"action": "screen_lock", "authority": {"action_id": 1}}),
        )
        .await,
    );
    assert_eq!(code, "invalid_params");
    // `desktop.info` takes `fresh`; the stub refuses it until step 2.
    let (code, _) = refused(call(&deck, "desktop.info", json!({"fresh": true})).await);
    assert_eq!(code, "unsupported");
    let (code, _) = refused(call(&deck, "desktop.info", json!({"other": 1})).await);
    assert_eq!(code, "invalid_params");
}
