//! `omarchy.status` and `omarchy.act` on the link (feature
//! `omarchy_admin.v1`): the method rows, the scope and feature checks, and
//! the dispatch of the station with and without an installed service.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::identity::Identity;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::methods::{authorize, code, method};
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::{Negotiated, PROTO_MAX, feature};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::omarchy_admin::{self, Config, Limits, OmarchyAdmin, SessionEnv};
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
fn the_omarchy_rows_are_admin_deck_to_station_rows_of_the_omarchy_feature() {
    assert_eq!(feature::OMARCHY_ADMIN, "omarchy_admin.v1");
    assert!(feature::ALL.contains(&feature::OMARCHY_ADMIN));
    for name in ["omarchy.status", "omarchy.act"] {
        let row = method(name).unwrap();
        assert_eq!(row.scope, Scope::Admin, "{name}");
        assert_eq!(row.feature, Some(feature::OMARCHY_ADMIN), "{name}");
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_methods_need_the_feature_and_the_admin_scope() {
    let without = negotiated(&[feature::CONTROL]);
    let with = negotiated(&[feature::CONTROL, feature::OMARCHY_ADMIN]);
    let drive: BTreeSet<Scope> = [Scope::Read, Scope::Drive].into_iter().collect();
    let admin: BTreeSet<Scope> = [Scope::Read, Scope::Drive, Scope::Admin]
        .into_iter()
        .collect();
    let call = |name: &str, n: &Negotiated, scopes: &BTreeSet<Scope>| {
        authorize(name, Role::Deck, Role::Station, None, n, Some(scopes))
            .map(|_| ())
            .map_err(|e| e.code)
    };
    for name in ["omarchy.status", "omarchy.act"] {
        assert_eq!(
            call(name, &without, &admin),
            Err(code::FEATURE_NOT_NEGOTIATED.to_owned())
        );
        assert_eq!(call(name, &with, &drive), Err(code::FORBIDDEN.to_owned()));
        assert_eq!(call(name, &with, &admin), Ok(()));
        // A station never sends them.
        let wrong = authorize(name, Role::Station, Role::Deck, None, &with, Some(&admin));
        assert_eq!(wrong.unwrap_err().code, code::WRONG_ROLE);
    }
}

struct Fixture {
    _det: Det,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
}

/// A station on a loopback link; the deck holds `admin`.
async fn fixture() -> Fixture {
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
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
            scopes: BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
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
        summary: std::sync::Arc::default(),
        ready: std::sync::Arc::default(),
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

/// One test: the service slot is process-global, and this file is its own
/// process. The station has no service first, then one with no Omarchy.
#[tokio::test]
async fn the_omarchy_station_serves_the_methods_only_with_an_installed_service() {
    let mut f = fixture().await;
    let deck = f.connect().await;
    for (method, params) in [
        ("omarchy.status", json!({})),
        (
            "omarchy.act",
            json!({"action": "install", "elevated": true}),
        ),
    ] {
        let (code, text) = refused(call(&deck, method, params).await);
        assert_eq!(code, "unavailable", "{method}");
        assert_eq!(text, "Omarchy management is not available on this station.");
    }
    let dir = tempfile::tempdir().unwrap();
    omarchy_admin::install(Arc::new(OmarchyAdmin::new(Config {
        program: dir.path().join("none"),
        data_dir: None,
        station_dir: dir.path().join("station"),
        config_home: Some(dir.path().join("config")),
        env: vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
        omarchy_env: ChildEnv::empty(),
        session: SessionEnv::Fixed(Vec::new()),
        limits: Limits::default(),
    })));
    // Status: no Omarchy, no provisioning state.
    let Outcome::Ok(status) = call(&deck, "omarchy.status", json!({})).await else {
        panic!("status refused");
    };
    assert_eq!(
        status,
        json!({
            "omarchy": {"present": false, "version": null},
            "install": null, "screensaver": null, "provision": null, "running": null
        })
    );
    // Act: the checks of the station run in their order.
    let (code, _) = refused(call(&deck, "omarchy.status", json!({"x": 1})).await);
    assert_eq!(code, "invalid_request");
    let (code, _) = refused(
        call(
            &deck,
            "omarchy.act",
            json!({"action": "install", "elevated": true, "extra": 1}),
        )
        .await,
    );
    assert_eq!(code, "invalid_request");
    let (code, _) = refused(
        call(
            &deck,
            "omarchy.act",
            json!({"action": "install", "elevated": false}),
        )
        .await,
    );
    assert_eq!(code, "elevation_required");
    let (code, _) = refused(
        call(
            &deck,
            "omarchy.act",
            json!({"action": "install", "elevated": true}),
        )
        .await,
    );
    assert_eq!(code, "omarchy_missing");
    let (code, _) = refused(
        call(
            &deck,
            "omarchy.act",
            json!({"action": "provision_retry", "elevated": true}),
        )
        .await,
    );
    assert_eq!(code, "not_provisioned");
}
