//! The target station B of the P5b proofs (a copy of the one of the P6
//! proofs): a real station (store, manager, policy, artifact service, link
//! runtime) with the fake desktop and the fake recorder. It dials the deck
//! with a `NodeConfig` of its own, so a test can close its link and link it
//! again.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_link::Epochs;
use fleetdeck_link::control::Control;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::NodeConfig;
use fleetdeck_link::reconnect::{DialerEvent, spawn_dialer};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::{ShellConfig, Shells};
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_station::workspace::PrivatePaths;
use tokio::task::JoinHandle;

use super::rig::Rig;
use super::{clock_ids, fast};

/// The task of the dialer and the control of its newest link.
#[derive(Default)]
struct Live {
    task: Option<JoinHandle<()>>,
    control: Arc<Mutex<Option<Control>>>,
}

pub struct StationB {
    pub identity: Arc<Identity>,
    pub store: Store,
    pub manager: Arc<SessionManager>,
    pub rig: Rig,
    peers: Arc<StationPeers>,
    node: NodeConfig,
    live: Mutex<Live>,
}

impl StationB {
    /// The station, with the deck `deck` pinned. It dials nothing yet.
    pub async fn new(dir: &Path, rig: Rig, deck: &Arc<Identity>) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let (clock, ids) = clock_ids();
        let store = Store::open(
            &dir.join("station.db"),
            clock,
            Arc::clone(&ids),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
            .with_allow_fleet_control(true)
            .with_allow_desktop(true)
            .with_allow_desktop_capture(true);
        let private = Arc::new(PrivatePaths::default());
        let manager = Arc::new(
            SessionManager::start(store.clone(), policy, Adapters::new(), ids, private)
                .await
                .unwrap(),
        );
        manager.set_artifacts_dir(dir.join("artifacts"), manager.private_paths());
        manager.set_shells(Arc::new(Shells::new(ShellConfig::default())));
        store
            .pin_peer(PairedKey {
                key: deck.public_key(),
                role: Role::Deck,
                scopes: BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
                dial_url: None,
            })
            .await
            .unwrap();
        let peers = StationPeers::load(store.clone()).await.unwrap();
        let identity = Arc::new(Identity::generate().unwrap());
        let node = NodeConfig::new(
            Arc::clone(&identity),
            LocalHello::new(Role::Station, store.incarnation().clone()),
            Arc::new(Epochs::in_memory()),
        )
        .with_store(Arc::clone(&peers));
        peers.pin_all(&node.pins).await.unwrap();
        Self {
            identity,
            store,
            manager,
            rig,
            peers,
            node,
            live: Mutex::default(),
        }
    }

    fn context(&self, node: &NodeConfig) -> Arc<StationLink> {
        Arc::new(StationLink {
            manager: Arc::clone(&self.manager),
            node: node.clone(),
            peers: Arc::clone(&self.peers),
            name: "beta".to_owned(),
            shells: Arc::new(Shells::new(ShellConfig::default())),
            coordinator: fleetdeck_station::coordinator::LinkForward::default(),
            paste_dir: None,
            summary: Arc::default(),
            ready: Arc::default(),
            fleet_git: Arc::new(fleetdeck_station::fleet_git_runtime::FleetGitRuntime::new(
                Arc::new(fleetdeck_station::workspace::GitOptions::isolated),
            )),
        })
    }

    /// Dials the deck at `addr` and serves every link. The hello leaves out
    /// the features `without`. An earlier link of this station ends first.
    pub async fn link(&self, addr: std::net::SocketAddr, deck: PeerKey, without: &[&str]) {
        self.unlink();
        let mut node = self.node.clone();
        node.local
            .features
            .retain(|f| !without.contains(&f.as_str()));
        let ctx = self.context(&node);
        let (mut events, dialer) = spawn_dialer(node, addr, deck, fast());
        let control: Arc<Mutex<Option<Control>>> = Arc::default();
        let newest = Arc::clone(&control);
        let task = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let DialerEvent::Linked(link) = event {
                    *newest.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(link.control.clone());
                    tokio::spawn(link::serve(Arc::clone(&ctx), *link));
                }
            }
            dialer.abort();
        });
        *self.live.lock().unwrap_or_else(PoisonError::into_inner) = Live {
            task: Some(task),
            control,
        };
    }

    /// Stops the dialer and closes the live link.
    pub fn unlink(&self) {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(task) = live.task.take() {
            task.abort();
        }
        let control = live
            .control
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(control) = control {
            control.close("test", "the link is replaced");
        }
    }

    /// The settings of the station as `station.toml` values:
    /// `allow_fleet_control`, `allow_desktop`, `allow_desktop_capture`.
    pub fn policy(&self, fleet: bool, desktop: bool, capture: bool) {
        let env = fleetdeck_core::paths::Env {
            home: Some("/home/test".into()),
            ..fleetdeck_core::paths::Env::default()
        };
        let text = format!(
            "allow_fleet_control = {fleet}\nallow_desktop = {desktop}\nallow_desktop_capture = {capture}\n"
        );
        let config = fleetdeck_core::config::StationConfig::parse(&text, &env).unwrap();
        let prepared = self.manager.prepare(&config).unwrap();
        self.manager.publish(prepared);
    }
}
