//! Helpers of the settings, elevation, secret relay and restart tests
//! (DL-172).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fleetdeck_core::Secret;
use fleetdeck_core::paths::Env;
use fleetdeck_core::restart::Restart;
use fleetdeck_core::settings::{DeckHolder, SettingsWriter};
use fleetdeck_deck::http::settings_routes::DeckSettings;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use crate::common::board::{BoardClient, Http};
use crate::common::next_request;

pub const PASSWORD: &str = "a long owner password for the tests";

pub type Headers = Vec<(String, String)>;

pub fn refs(h: &Headers) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

pub async fn owner(c: &BoardClient) -> Headers {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

pub async fn post(c: &BoardClient, h: &Headers, path: &str, body: Value) -> Http {
    c.post(path, &refs(h), body).await
}

pub async fn get(c: &BoardClient, h: &Headers, path: &str) -> Http {
    c.get(path, &refs(h)).await
}

pub fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

/// Sets the owner password (this ends every login: call it before `owner`).
pub async fn set_password(c: &BoardClient) {
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
}

/// An elevation for the login `h`, made with the owner password.
pub async fn elevate(c: &BoardClient, h: &Headers) -> String {
    let r = post(c, h, "/api/auth/elevate", json!({ "password": PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["expires_at"].as_i64().unwrap() > 0);
    r.body["elevation"].as_str().unwrap().to_owned()
}

/// A `deck.toml` (mode 0600) and its settings.
pub fn deck_settings(dir: &Path, text: &str) -> DeckSettings {
    let path: PathBuf = dir.join("deck.toml");
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    DeckSettings {
        writer: Arc::new(SettingsWriter::<DeckHolder>::new(
            &path,
            None,
            &Env::default(),
        )),
        path,
        overrides: vec![("listen".to_owned(), "--listen".to_owned())],
        gate: Arc::default(),
    }
}

/// Runs `call` while the fake station answers its next request. Returns the
/// HTTP answer and the request that the station got.
pub async fn with_station(
    station: &mut LiveLink,
    call: impl Future<Output = Http>,
    answer: Outcome,
) -> (Http, InboundRequest) {
    let answering = async {
        let req = next_request(station).await;
        station.control.respond(req.id, answer).await.unwrap();
        req
    };
    tokio::join!(call, answering)
}

/// A restart handle that counts the requests.
#[derive(Debug, Default)]
pub struct CountingRestart(pub AtomicUsize);

impl Restart for CountingRestart {
    fn request(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl CountingRestart {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

pub fn saved(revision: &str) -> Value {
    json!({
        "values": {}, "applied": [], "restart_needed": [], "unapplied": [],
        "revision": revision, "durability": "confirmed"
    })
}

/// A fixture like `common::fixture`, with a `deck.db` file instead of an
/// in-memory database, so that a test can read every table and every file.
pub async fn file_fixture(db: &Path) -> crate::common::Fixture {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use fleetdeck_core::identity::Identity;
    use fleetdeck_deck::link::DeckLink;
    use fleetdeck_deck::link::runtime::STATION_SCOPES;
    use fleetdeck_deck::store::stations::NewStation;
    use fleetdeck_deck::{DeckPeers, DeckStore};
    use fleetdeck_link::hello::LocalHello;
    use fleetdeck_link::node::{self, NodeConfig};
    use fleetdeck_link::tls::Role;
    use fleetdeck_link::{Epochs, Grants};
    use fleetdeck_proto::IncarnationId;
    use fleetdeck_proto::link::Scope;
    use fleetdeck_testkit::Det;

    let det = Det::new();
    let store = DeckStore::open(db, det.clock(), det.ids()).await.unwrap();
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
    crate::common::Fixture {
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

/// True when `needle` is in a text or blob column of any table of the
/// SQLite file `db`, or in the bytes of the file or its WAL.
pub fn database_holds(db: &Path, needle: &str) -> bool {
    use rusqlite::types::ValueRef;
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut found = false;
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let n = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..n {
                let hit = match row.get_ref(i).unwrap() {
                    ValueRef::Text(t) | ValueRef::Blob(t) => {
                        String::from_utf8_lossy(t).contains(needle)
                    }
                    _ => false,
                };
                found |= hit;
            }
        }
    }
    for suffix in ["", "-wal"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        if let Ok(bytes) = std::fs::read(PathBuf::from(name)) {
            found |= String::from_utf8_lossy(&bytes).contains(needle);
        }
    }
    found
}
