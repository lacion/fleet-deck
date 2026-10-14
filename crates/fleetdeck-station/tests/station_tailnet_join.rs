//! `station join --tailnet` on the station node (DL-178, tailnet design 5.2
//! and 5.4 and the station CLI tests of 8): the host must be a peer of
//! `tailscale status`, the probe writes nothing, the request is sent only
//! after the retry file exists, and the deck pin, the grants and the dial
//! target are stored in one local transaction after a verified approval.
//! The deck is a bare `fleetdeck-link` node on loopback that has the rules
//! of a tailnet listener.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::tailscale::TailscaleCli;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, ListenerKind, NodeConfig};
use fleetdeck_link::pair_hub::{CommitError, Completion, HubConfig, PairApprover, PairRequests};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_link::retry_file;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::runtime::DECK_SCOPES;
use fleetdeck_station::link::{NodeError, StationNode, TailnetJoin, TailnetResult};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};

const T: Duration = Duration::from_secs(10);

const STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "box", "DNSName": "box.tail1234.ts.net.", "UserID": 1,
           "TailscaleIPs": ["100.64.0.30"]},
  "Peer": {
    "nodekey:d": {"HostName": "deck-box", "DNSName": "deck-box.tail1234.ts.net.", "UserID": 1,
                  "TailscaleIPs": ["100.64.0.20", "fd7a:115c:a1e0::14"], "Online": true}
  },
  "User": {"1": {"LoginName": "owner@example.com"}}
}"#;

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

#[derive(Default)]
struct Memory(Mutex<HashMap<String, Completion>>);

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Memory")
    }
}

#[async_trait::async_trait]
impl PairApprover for Memory {
    async fn commit(&self, c: &Completion) -> Result<(), CommitError> {
        self.0
            .lock()
            .unwrap()
            .insert(c.request_id.clone(), c.clone());
        Ok(())
    }
    async fn completion(&self, id: &str) -> Result<Option<Completion>, String> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
}

struct Deck {
    identity: Arc<Identity>,
    node: NodeConfig,
    hub: Arc<PairRequests>,
    listener: LinkListener,
}

async fn deck() -> Deck {
    let identity = Arc::new(Identity::generate().unwrap());
    let grants = Grants::new();
    let mut node = NodeConfig::new(
        Arc::clone(&identity),
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(grants);
    let hub = PairRequests::new(
        identity.public_key(),
        [Scope::Admin].into(),
        node.pins.clone(),
        Arc::new(Memory::default()),
        HubConfig::default(),
    );
    node = node.with_pair_requests(Arc::clone(&hub));
    node.listener_kind = Some(ListenerKind::Tailnet);
    let listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    hub.open_window().await.unwrap();
    Deck {
        identity,
        node,
        hub,
        listener,
    }
}

struct Station {
    dir: tempfile::TempDir,
    store: Store,
    node: Arc<StationNode>,
    identity: Arc<Identity>,
    cli: TailscaleCli,
}

async fn station(status: &str) -> Station {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    let store = Store::open(
        &dir.path().join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store.clone(),
        policy,
        Adapters::new(),
        ids,
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let node = StationNode::start(
        Arc::new(manager),
        Arc::clone(&identity),
        "box".to_owned(),
        &[],
        fast(),
    )
    .await
    .unwrap();
    node.set_data_dir(dir.path());
    let cli = fake_tailscale(dir.path(), status);
    Station {
        dir,
        store,
        node,
        identity,
        cli,
    }
}

fn fake_tailscale(dir: &Path, status: &str) -> TailscaleCli {
    std::fs::write(dir.join("status.json"), status).unwrap();
    let path = dir.join("tailscale");
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncat '{}/status.json'\n", dir.display()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    TailscaleCli::new(path)
}

impl Station {
    fn join(&self, deck: &Deck, host: &str, fingerprint: bool) -> TailnetJoin {
        TailnetJoin {
            host: host.to_owned(),
            port: deck.listener.local_addr.port(),
            deck_fingerprint: fingerprint.then(|| deck.identity.fingerprint()),
            scopes: BTreeSet::from([Scope::Read, Scope::Drive]),
            name: "laptop".to_owned(),
            cli: self.cli.clone(),
            dial: Some(vec![deck.listener.local_addr]),
        }
    }
}

async fn pending(deck: &Deck) -> fleetdeck_link::pair_hub::PendingView {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        if let Some(v) = deck.hub.list().await.into_iter().next() {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "no request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn digest(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

async fn approve(deck: &Deck) {
    let view = pending(deck).await;
    deck.hub
        .approve(
            &view.request_id,
            &view.station_fingerprint.parse().unwrap(),
            &digest(&view.digest),
        )
        .await
        .unwrap();
}

fn refused(r: Result<TailnetResult, NodeError>) -> (String, String) {
    match r {
        Err(NodeError::Tailnet { code, detail }) => (code, detail),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn an_approved_request_stores_the_deck_pin_the_grants_and_the_dial_target_together() {
    let mut deck = deck().await;
    let station = station(STATUS).await;
    let join = station.join(&deck, "deck-box.tail1234.ts.net", true);
    let node = Arc::clone(&station.node);
    let task = tokio::spawn(async move { node.join_tailnet(&join).await });

    let view = pending(&deck).await;
    assert_eq!(view.requested_name, "laptop");
    // The retry file exists while the request waits.
    let file = retry_file::read(station.dir.path())
        .unwrap()
        .expect("retry file");
    assert_eq!(file.request_id, view.request_id);
    assert_eq!(file.host, "deck-box.tail1234.ts.net");
    // Nothing is stored before the approval.
    assert!(station.store.paired_keys().await.unwrap().is_empty());

    approve(&deck).await;
    let result = tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        result,
        TailnetResult::Paired {
            device: "deck-box.tail1234.ts.net".into(),
            deck: deck.identity.fingerprint(),
        }
    );
    let keys = station.store.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, deck.identity.public_key());
    assert_eq!(keys[0].role, Role::Deck);
    assert_eq!(keys[0].scopes, BTreeSet::from([Scope::Read, Scope::Drive]));
    assert_eq!(
        keys[0].dial_url.as_deref(),
        Some(deck.listener.local_addr.to_string().as_str())
    );
    assert!(retry_file::read(station.dir.path()).unwrap().is_none());

    // The station now dials the deck.
    let open = tokio::time::timeout(T, deck.listener.next_link())
        .await
        .expect("the station dials the deck")
        .unwrap();
    assert_eq!(open.peer, station.identity.public_key());
    let _ = &deck.node;
}

#[tokio::test]
async fn a_host_that_is_not_a_peer_of_tailscale_status_is_refused_before_anything_is_sent() {
    let deck = deck().await;
    let station = station(STATUS).await;
    for host in ["example.com", "100.64.0.99", "box.tail1234.ts.net", "box"] {
        let join = station.join(&deck, host, true);
        let (code, _) = refused(station.node.join_tailnet(&join).await);
        assert_eq!(code, "not_in_tailnet", "{host}");
    }
    assert!(deck.hub.list().await.is_empty());
    assert!(retry_file::read(station.dir.path()).unwrap().is_none());
    assert!(station.store.paired_keys().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_missing_tailscale_is_refused() {
    let deck = deck().await;
    let station = station(STATUS).await;
    let mut join = station.join(&deck, "deck-box.tail1234.ts.net", true);
    join.cli = TailscaleCli::new("/nonexistent/tailscale");
    let (code, detail) = refused(station.node.join_tailnet(&join).await);
    assert_eq!(code, "tailscale_unavailable");
    assert!(detail.contains("Tailscale is not available"), "{detail}");
}

#[tokio::test]
async fn the_probe_shows_the_key_of_the_deck_and_writes_and_sends_nothing() {
    let deck = deck().await;
    let station = station(STATUS).await;
    let join = station.join(&deck, "deck-box.tail1234.ts.net", false);
    let result = station.node.join_tailnet(&join).await.unwrap();
    assert_eq!(result, TailnetResult::Observed(deck.identity.fingerprint()));
    assert!(deck.hub.list().await.is_empty(), "no request was sent");
    assert!(retry_file::read(station.dir.path()).unwrap().is_none());
    assert!(station.store.paired_keys().await.unwrap().is_empty());
    // The probe also works when the window is closed: it only reads the key.
    deck.hub.close_window().await;
    let again = station.node.join_tailnet(&join).await.unwrap();
    assert_eq!(again, TailnetResult::Observed(deck.identity.fingerprint()));
}

#[tokio::test]
async fn a_wrong_deck_fingerprint_fails_without_writes() {
    let deck = deck().await;
    let station = station(STATUS).await;
    let mut join = station.join(&deck, "deck-box.tail1234.ts.net", true);
    join.deck_fingerprint = Some(Identity::generate().unwrap().fingerprint());
    let (code, _) = refused(station.node.join_tailnet(&join).await);
    assert_eq!(code, "fingerprint_mismatch");
    assert!(deck.hub.list().await.is_empty());
    assert!(station.store.paired_keys().await.unwrap().is_empty());
    assert!(
        retry_file::read(station.dir.path()).unwrap().is_none(),
        "the request was never sent, so no file"
    );
}

#[tokio::test]
async fn a_denied_request_ends_with_denied_and_a_cancel_removes_the_file() {
    let deck = deck().await;
    let station = station(STATUS).await;
    let join = station.join(&deck, "deck-box.tail1234.ts.net", true);
    let node = Arc::clone(&station.node);
    let j = join.clone();
    let task = tokio::spawn(async move { node.join_tailnet(&j).await });
    let view = pending(&deck).await;
    deck.hub.deny(&view.request_id).await.unwrap();
    let r = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert_eq!(refused(r).0, "denied");
    assert!(station.store.paired_keys().await.unwrap().is_empty());

    // A file of another deck blocks a new join until `--cancel`.
    let req =
        fleetdeck_link::pair_request::CanonicalRequest::generate("laptop", [Scope::Read].into())
            .unwrap();
    retry_file::write(
        station.dir.path(),
        &retry_file::RetryRecord::new("other.tail1234.ts.net", 7421, &"ab".repeat(32), &req),
    )
    .unwrap();
    let (code, detail) = refused(station.node.join_tailnet(&join).await);
    assert_eq!(code, "pending");
    assert!(
        detail.contains("station join --tailnet --cancel"),
        "{detail}"
    );
    assert!(station.node.cancel_tailnet().unwrap());
    assert!(!station.node.cancel_tailnet().unwrap());
    assert!(retry_file::read(station.dir.path()).unwrap().is_none());
}

#[tokio::test]
async fn a_saved_request_of_the_same_deck_is_resumed_with_its_own_id() {
    let deck = deck().await;
    let station = station(STATUS).await;
    let req =
        fleetdeck_link::pair_request::CanonicalRequest::generate("old-name", [Scope::Read].into())
            .unwrap();
    retry_file::write(
        station.dir.path(),
        &retry_file::RetryRecord::new(
            "deck-box.tail1234.ts.net",
            deck.listener.local_addr.port(),
            &deck.identity.fingerprint().to_string(),
            &req,
        ),
    )
    .unwrap();
    let join = station.join(&deck, "DECK-BOX.tail1234.ts.net.", true);
    let node = Arc::clone(&station.node);
    let task = tokio::spawn(async move { node.join_tailnet(&join).await });
    let view = pending(&deck).await;
    assert_eq!(view.request_id, req.id_hex());
    assert_eq!(view.requested_name, "old-name");
    approve(&deck).await;
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let keys = station.store.paired_keys().await.unwrap();
    assert_eq!(
        keys[0].scopes,
        BTreeSet::from([Scope::Read]),
        "the saved scopes"
    );
}

#[tokio::test]
async fn a_station_with_a_deck_refuses_a_second_join() {
    let deck = deck().await;
    let station = station(STATUS).await;
    let join = station.join(&deck, "deck-box.tail1234.ts.net", true);
    let node = Arc::clone(&station.node);
    let j = join.clone();
    let task = tokio::spawn(async move { node.join_tailnet(&j).await });
    pending(&deck).await;
    approve(&deck).await;
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let r = station.node.join_tailnet(&join).await;
    assert!(matches!(r, Err(NodeError::AlreadyPaired(_))), "{r:?}");
    let _ = DECK_SCOPES;
    let _: Option<SocketAddr> = None;
}
