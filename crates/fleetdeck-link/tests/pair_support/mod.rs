//! Helpers of the pairing request tests: a deck with a tailnet-kind
//! listener on loopback, an in-memory approval store, and a station.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::identity::{Fingerprint, Identity};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, ListenerKind, NodeConfig};
use fleetdeck_link::pair_hub::{
    CommitError, Completion, HubConfig, PairApprover, PairRequests, PendingView,
};
use fleetdeck_link::pair_request::CanonicalRequest;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{
    Epochs, Grants, Invitations, LinkError, LinkListener, PairApproved, PairMode, request_pairing,
};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::Scope;
use tokio::task::JoinHandle;

pub use crate::common::{Node, T, node};

/// The stored approvals of a deck, in memory. A switch makes a commit fail.
#[derive(Default)]
pub struct Memory {
    pub done: Mutex<HashMap<String, Completion>>,
    pub names: Mutex<BTreeSet<String>>,
    pub fail: Mutex<Option<CommitError>>,
    /// Runs inside the next commit (a station that vanishes mid-approval).
    pub on_commit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Memory")
    }
}

#[async_trait::async_trait]
impl PairApprover for Memory {
    async fn commit(&self, c: &Completion) -> Result<(), CommitError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        if !self.names.lock().unwrap().insert(c.requested_name.clone()) {
            return Err(CommitError::NameTaken(c.requested_name.clone()));
        }
        if let Some(hook) = self.on_commit.lock().unwrap().take() {
            hook();
        }
        self.done
            .lock()
            .unwrap()
            .insert(c.request_id.clone(), c.clone());
        Ok(())
    }

    async fn completion(&self, id: &str) -> Result<Option<Completion>, String> {
        Ok(self.done.lock().unwrap().get(id).cloned())
    }
}

pub struct Deck {
    pub node: Node,
    pub hub: Arc<PairRequests>,
    pub memory: Arc<Memory>,
    pub invitations: Arc<Invitations>,
    pub listener: LinkListener,
}

impl Deck {
    pub fn addr(&self) -> SocketAddr {
        self.listener.local_addr
    }

    pub fn fp(&self) -> Fingerprint {
        self.node.config.identity.fingerprint()
    }

    /// Another listener of the same node, of the given kind.
    pub async fn listen_as(&self, kind: ListenerKind) -> LinkListener {
        let mut config = self.node.config.clone();
        config.listener_kind = Some(kind);
        node::listen(config, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap()
    }
}

/// A deck whose listener has the rules of a tailnet listener.
pub async fn deck(cfg: HubConfig) -> Deck {
    deck_with(cfg, |_| {}).await
}

pub async fn deck_with(cfg: HubConfig, tweak: impl FnOnce(&mut NodeConfig)) -> Deck {
    deck_from(node(Role::Deck), Arc::new(Memory::default()), cfg, tweak).await
}

/// The deck process after a restart: the same key and the same store; the
/// keys of the stored approvals are pinned again.
pub async fn restart_deck(identity: Arc<Identity>, memory: Arc<Memory>) -> Deck {
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    let grants = Grants::new();
    let config = NodeConfig::new(
        identity,
        LocalHello::new(Role::Deck, inc),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&grants));
    for done in memory.done.lock().unwrap().values() {
        config.pins.pin(done.station_key, Role::Station);
    }
    deck_from(
        Node { config, grants },
        memory,
        HubConfig::default(),
        |_| {},
    )
    .await
}

async fn deck_from(
    mut node: Node,
    memory: Arc<Memory>,
    cfg: HubConfig,
    tweak: impl FnOnce(&mut NodeConfig),
) -> Deck {
    let invitations = Invitations::new();
    let hub = PairRequests::new(
        node.config.identity.public_key(),
        [Scope::Admin].into(),
        node.config.pins.clone(),
        Arc::clone(&memory) as Arc<dyn PairApprover>,
        cfg,
    );
    node.config = node
        .config
        .with_invitations(Arc::clone(&invitations))
        .with_pair_requests(Arc::clone(&hub));
    node.config.listener_kind = Some(ListenerKind::Tailnet);
    tweak(&mut node.config);
    let listener = node::listen(node.config.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    Deck {
        node,
        hub,
        memory,
        invitations,
        listener,
    }
}

/// Short times for the tests that wait for the clock.
pub fn quick() -> HubConfig {
    HubConfig {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(400),
        ..HubConfig::default()
    }
}

pub fn request(name: &str) -> CanonicalRequest {
    CanonicalRequest::generate(name, [Scope::Read, Scope::Drive].into()).unwrap()
}

pub type Asked = JoinHandle<Result<PairApproved, LinkError>>;

/// The station sends `req` (or resumes it) and waits for the answer.
pub fn ask(station: &Node, deck: &Deck, req: &CanonicalRequest, mode: PairMode) -> Asked {
    ask_at(station, deck.addr(), deck.fp(), req, mode)
}

pub fn ask_at(
    station: &Node,
    addr: SocketAddr,
    deck_fp: Fingerprint,
    req: &CanonicalRequest,
    mode: PairMode,
) -> Asked {
    let config = station.config.clone();
    let req = req.clone();
    tokio::spawn(async move {
        request_pairing(
            &config,
            addr,
            deck_fp,
            &req,
            mode,
            Duration::from_secs(30),
            || Ok(()),
        )
        .await
    })
}

pub async fn result(asked: Asked) -> Result<PairApproved, LinkError> {
    tokio::time::timeout(T, asked)
        .await
        .expect("the request ended")
        .expect("the task did not panic")
}

pub fn code(r: &Result<PairApproved, LinkError>) -> String {
    match r {
        Ok(_) => "ok".into(),
        Err(e) => e.reason().unwrap_or("none").to_owned(),
    }
}

/// Waits until `n` requests wait on the deck.
pub async fn pending(deck: &Deck, n: usize) -> Vec<PendingView> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let list = deck.hub.list().await;
        if list.len() == n {
            return list;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} requests wait, expected {n}",
            list.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub fn digest(view: &PendingView) -> [u8; 32] {
    let text = &view.digest;
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

/// Approves the only pending request with the values the board shows.
pub async fn approve_first(deck: &Deck) -> fleetdeck_link::pair_hub::Approved {
    let view = pending(deck, 1).await.remove(0);
    let fp = view.station_fingerprint.parse().unwrap();
    deck.hub
        .approve(&view.request_id, &fp, &digest(&view))
        .await
        .expect("the approval")
}

/// Opens the window of `deck` (its listener is a tailnet listener).
pub async fn open(deck: &Deck) {
    deck.hub
        .open_window()
        .await
        .expect("a tailnet listener runs");
}
