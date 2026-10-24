//! Helpers of the pairing request tests of the board: a deck node whose
//! listener has the rules of a tailnet listener, a board for it, a fake
//! `tailscale`, and a station that asks over loopback.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::common::T;
use crate::common::board::{BoardClient, Extras, board_with_extras};
use crate::settings_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::link::{DeckNode, NodeOptions};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::ListenerKind;
use fleetdeck_link::pair_hub::HubConfig;
use fleetdeck_link::pair_request::CanonicalRequest;
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, LinkError, NodeConfig, PairApproved, PairMode, request_pairing};
use fleetdeck_proto::IncarnationId;
use serde_json::{Value, json};

pub const TAILNET_STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "deck-box", "DNSName": "deck-box.tail1234.ts.net.", "OS": "linux",
           "UserID": 1, "TailscaleIPs": ["100.64.0.10", "fd7a:115c:a1e0::a"]},
  "Peer": {
    "nodekey:a": {"HostName": "alpha", "DNSName": "alpha.tail1234.ts.net.", "OS": "linux",
                  "UserID": 1, "TailscaleIPs": ["100.64.0.11"], "Online": true},
    "nodekey:b": {"HostName": "bravo", "DNSName": "bravo.tail1234.ts.net.", "OS": "macOS",
                  "UserID": 1, "TailscaleIPs": ["100.64.0.12"], "Online": true}
  },
  "User": {"1": {"LoginName": "owner@example.com"}}
}"#;

pub const NO_DNS_STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "deck-box", "DNSName": "", "UserID": 1,
           "TailscaleIPs": ["fd7a:115c:a1e0::a", "100.64.0.10"]},
  "Peer": {
    "nodekey:a": {"HostName": "alpha", "DNSName": "alpha.tail1234.ts.net.",
                  "UserID": 1, "TailscaleIPs": ["100.64.0.11"], "Online": true}
  },
  "User": {"1": {"LoginName": "owner@example.com"}}
}"#;

pub const WHOIS: &str = r#"{
  "Node": {"Name": "laptop.tail1234.ts.net.", "Addresses": ["100.64.0.20/32"], "Tags": ["tag:dev"]},
  "UserProfile": {"ID": 1, "LoginName": "owner@example.com"}
}"#;

/// A fake `tailscale` that answers `status` and `whois`.
pub fn fake_tailscale(dir: &Path, status: &str) -> String {
    std::fs::write(dir.join("status.json"), status).unwrap();
    std::fs::write(dir.join("whois.json"), WHOIS).unwrap();
    let path = dir.join("tailscale");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  status) cat '{0}/status.json' ;;\n  whois) cat '{0}/whois.json' ;;\n  *) exit 2 ;;\nesac\n",
        dir.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

pub fn unix_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

pub fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

pub struct Deck {
    pub _dir: tempfile::TempDir,
    pub db: std::path::PathBuf,
    pub identity: Arc<Identity>,
    pub node: Arc<DeckNode>,
    pub board: BoardClient,
    pub listen: SocketAddr,
}

pub async fn start_node(
    db: &Path,
    identity: &Arc<Identity>,
    kind: Option<ListenerKind>,
    hub: HubConfig,
) -> Arc<DeckNode> {
    let det = fleetdeck_testkit::Det::new();
    let store = DeckStore::open(db, det.clock(), det.ids()).await.unwrap();
    DeckNode::start_with(
        store,
        Arc::clone(identity),
        &["127.0.0.1:0".parse().unwrap()],
        fast(),
        NodeOptions {
            hub,
            tailnet: None,
            listener_kind: kind,
            optional_listen: Vec::new(),
        },
    )
    .await
    .unwrap()
}

pub async fn board_of(node: &Arc<DeckNode>, dir: &Path, status: &str) -> BoardClient {
    let bin = fake_tailscale(dir, status);
    let toml = format!(
        "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\"]\nlink_listen = [\"127.0.0.1:7421\"]\ntailscale_bin = \"{bin}\"\n"
    );
    let c = board_with_extras(
        Arc::clone(node.link()),
        Extras {
            settings: Some(deck_settings(dir, &toml)),
            pairing: Some(Arc::clone(node)),
            ..Extras::default()
        },
    )
    .await;
    set_password(&c).await;
    c
}

pub async fn deck(kind: Option<ListenerKind>) -> Deck {
    deck_with(kind, HubConfig::default(), TAILNET_STATUS).await
}

pub async fn deck_with(kind: Option<ListenerKind>, hub: HubConfig, status: &str) -> Deck {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("deck.db");
    let identity = Arc::new(Identity::generate().unwrap());
    let node = start_node(&db, &identity, kind, hub).await;
    let board = board_of(&node, dir.path(), status).await;
    let listen = node.listen_addrs()[0];
    Deck {
        _dir: dir,
        db,
        identity,
        node,
        board,
        listen,
    }
}

pub fn station_config() -> NodeConfig {
    let identity = Arc::new(Identity::generate().unwrap());
    NodeConfig::new(
        identity,
        LocalHello::new(Role::Station, IncarnationId::from("station-inc")),
        Arc::new(Epochs::in_memory()),
    )
}

pub type Asked = tokio::task::JoinHandle<Result<PairApproved, LinkError>>;

pub fn ask(deck: &Deck, station: &NodeConfig, req: &CanonicalRequest, mode: PairMode) -> Asked {
    let (config, req) = (station.clone(), req.clone());
    let (addr, fp) = (deck.listen, deck.identity.fingerprint());
    tokio::spawn(async move {
        request_pairing(
            &config,
            addr,
            fp,
            &req,
            mode,
            Duration::from_secs(30),
            || Ok(()),
        )
        .await
    })
}

pub async fn answer(asked: Asked) -> Result<PairApproved, LinkError> {
    tokio::time::timeout(T, asked).await.unwrap().unwrap()
}

pub fn code(r: &Result<PairApproved, LinkError>) -> String {
    r.as_ref()
        .map_or_else(|e| e.reason().unwrap_or("none").to_owned(), |_| "ok".into())
}

/// Waits until `n` requests are listed by the board; returns the list.
pub async fn listed(c: &BoardClient, h: &Headers, n: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = get(c, h, "/api/pairing/requests").await;
        assert_eq!(r.status, 200, "{}", r.body);
        let list = r.body["requests"].as_array().unwrap().clone();
        if list.len() == n {
            return list;
        }
        assert!(tokio::time::Instant::now() < deadline, "{list:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

pub async fn open_window(c: &BoardClient, h: &Headers) -> Value {
    let elevation = elevate(c, h).await;
    let r = post(
        c,
        h,
        "/api/pairing/window",
        json!({ "open": true, "elevation": elevation }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

pub fn approve_body(req: &Value, elevation: &str) -> Value {
    json!({
        "fingerprint": req["station_fingerprint"],
        "digest": req["digest"],
        "elevation": elevation,
    })
}

pub fn approve_path(req: &Value) -> String {
    format!(
        "/api/pairing/requests/{}/approve",
        req["request_id"].as_str().unwrap()
    )
}
