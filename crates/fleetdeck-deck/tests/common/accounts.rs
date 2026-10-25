//! The shared harness of the accounts tests (DL-173): a fake station link
//! and a board over a deck whose database is a file, so that a test can
//! search the file.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::common::board::{BoardClient, Http, board};
use super::common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::link::{DeckLink, runtime::STATION_SCOPES};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::{DeckPeers, DeckStore};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

pub const DAY: i64 = 24 * 60 * 60 * 1000;

pub type Headers = Vec<(String, String)>;

pub fn refs(h: &Headers) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

pub fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

/// The fixture of `common`, with a deck database on disk, so that a test
/// can search the file.
pub async fn fixture_on_disk(dir: &Path) -> Fixture {
    let det = Det::new();
    let store = DeckStore::open(&dir.join("deck.db"), det.clock(), det.ids())
        .await
        .unwrap();
    let deck_id = Arc::new(Identity::generate().unwrap());
    let station_id = Arc::new(Identity::generate().unwrap());
    let rec = store
        .pin_station(NewStation {
            key: station_id.public_key(),
            name: "fake".to_owned(),
            scopes: STATION_SCOPES.into_iter().collect(),
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

pub struct Setup {
    pub f: Fixture,
    pub station: LiveLink,
    pub c: BoardClient,
    pub h: Headers,
    pub dir: tempfile::TempDir,
}

pub async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let mut f = fixture_on_disk(dir.path()).await;
    let station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    Setup {
        f,
        station,
        c,
        h,
        dir,
    }
}

pub async fn owner(c: &BoardClient) -> Headers {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
        ("Origin".to_owned(), c.origin()),
    ]
}

pub fn token_headers(token: &str) -> Headers {
    vec![("Authorization".to_owned(), format!("Bearer {token}"))]
}

/// Sends `call` and answers the station request with `reply`. Returns the
/// HTTP answer and the request that the station got.
pub async fn exchange(
    station: &mut LiveLink,
    call: impl std::future::Future<Output = Http>,
    reply: Outcome,
) -> (Http, fleetdeck_link::control::InboundRequest) {
    tokio::join!(call, async {
        let req = next_request(station).await;
        station.control.respond(req.id, reply).await.unwrap();
        req
    })
}

pub fn ok(v: Value) -> Outcome {
    Outcome::Ok(v)
}

pub fn account(kind: &str) -> Value {
    json!({
        "account_id": "acc-1", "kind": kind, "site": "acme.atlassian.net",
        "user": "ana", "method": "oauth", "status": "ok", "message": "connected",
    })
}

/// Every text of the database file and its WAL: all columns of all tables,
/// and the raw bytes of the files.
pub fn database_text(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("deck.db"))
        {
            text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    let conn = rusqlite::Connection::open_with_flags(
        dir.join("deck.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let n = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..n {
                match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                        text.push_str(&String::from_utf8_lossy(t));
                    }
                    rusqlite::types::ValueRef::Integer(v) => text.push_str(&v.to_string()),
                    _ => {}
                }
                text.push('\n');
            }
        }
    }
    text
}
