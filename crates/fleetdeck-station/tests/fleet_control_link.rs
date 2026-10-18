//! The two new station methods of ADR-008 over a loopback link:
//! `coordinator.policy_now` (§6.2) and `files.coordinator_diff` (§4.4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::PolicyNowAnswer;
use fleetdeck_station::coordinator::{Binding, BindingState, ToolClass};
use fleetdeck_station::link::{self, StationLink, StationPeers};
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

async fn fixture(fleet_control: bool) -> Fixture {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
        .with_allow_fleet_control(fleet_control);
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
            scopes: BTreeSet::from([Scope::Read]),
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
        deck,
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

fn grant(session: &str, state: BindingState) -> Binding {
    Binding {
        binding_id: format!("{session}:g1"),
        grant_id: "g1".to_owned(),
        session_id: session.into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        generation: 1,
        classes: BTreeSet::from([ToolClass::Inspect]),
        scope: fleetdeck_proto::command::GrantScope::Fleet,
        state,
        control_skill: "skill".to_owned(),
    }
}

#[tokio::test]
async fn policy_now_answers_the_live_setting_and_whether_the_caller_is_bound() {
    for fleet_control in [true, false] {
        let mut f = fixture(fleet_control).await;
        let bindings = f.ctx.manager.coordinator_bindings().clone();
        bindings.insert("d1".to_owned(), grant("coord-1", BindingState::Active));
        bindings.insert("d2".to_owned(), grant("coord-2", BindingState::Pending));
        let deck = f.connect().await;
        for (session, bound) in [("coord-1", true), ("coord-2", false), ("nobody", false)] {
            let Outcome::Ok(v) = call(
                &deck,
                "coordinator.policy_now",
                json!({"caller_session": session}),
            )
            .await
            else {
                panic!("policy_now failed");
            };
            let answer: PolicyNowAnswer = serde_json::from_value(v).unwrap();
            assert_eq!(
                answer,
                PolicyNowAnswer {
                    fleet_control,
                    bound
                },
                "{session}"
            );
        }
        // The value is live: a revoked binding is not bound on the next ask.
        bindings.revoke_session(&"coord-1".into());
        let Outcome::Ok(v) = call(
            &deck,
            "coordinator.policy_now",
            json!({"caller_session": "coord-1"}),
        )
        .await
        else {
            panic!("policy_now failed");
        };
        assert!(!serde_json::from_value::<PolicyNowAnswer>(v).unwrap().bound);
        // Bad params are a protocol error, not an answer.
        let Outcome::Err(e) = call(&deck, "coordinator.policy_now", json!({"x": 1})).await else {
            panic!("expected an error");
        };
        assert_eq!(e.code, "protocol_error");
    }
}

#[tokio::test]
async fn the_coordinator_diff_method_takes_only_a_session_id_and_keeps_the_file_error_codes() {
    let mut f = fixture(true).await;
    let deck = f.connect().await;
    let Outcome::Err(e) = call(
        &deck,
        "files.coordinator_diff",
        json!({"session_id": "nope"}),
    )
    .await
    else {
        panic!("expected an error");
    };
    assert_eq!(e.code, "unknown_session");
    // `max_bytes` is not a parameter of this method.
    let Outcome::Err(e) = call(
        &deck,
        "files.coordinator_diff",
        json!({"session_id": "nope", "max_bytes": 10}),
    )
    .await
    else {
        panic!("expected an error");
    };
    assert_eq!(e.code, "protocol_error");
}
