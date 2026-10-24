//! A pasted image over a real link on loopback (DL-147): `POST
//! /api/stations/{station}/paste` forwards the image to the station's
//! `files.write_temp` method; the owner-only rule, the body limit, the
//! refusals of the station and the audit row.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_node};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::{ShellConfig, Shells};
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(30);
const PATH: &str = "/api/stations/alpha/paste";

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

fn clock_ids() -> (SharedClock, SharedIds) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    (clock, ids)
}

async fn station(dir: &Path, root: PathBuf, paste_dir: Option<PathBuf>) -> Arc<StationNode> {
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(vec![root], 2, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::new(),
        ids,
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    StationNode::start_with_paste_dir(
        Arc::new(manager),
        Arc::new(Identity::generate().unwrap()),
        "alpha".to_owned(),
        &[],
        fast(),
        Arc::new(Shells::new(ShellConfig::default())),
        paste_dir,
    )
    .await
    .unwrap()
}

async fn deck(dir: &Path) -> (Arc<DeckNode>, BoardClient) {
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let node = DeckNode::start(store, identity, &["127.0.0.1:0".parse().unwrap()], fast())
        .await
        .unwrap();
    let board = board_with_node(Arc::clone(&node)).await;
    (node, board)
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

/// A PNG header and `extra` filler bytes.
fn png(extra: usize) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend(std::iter::repeat_n(7u8, extra));
    b
}

fn b64(bytes: &[u8]) -> String {
    // Standard base64 without a dependency: the test needs only an encoder.
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn body(id: &str, bytes: &[u8]) -> Value {
    json!({ "upload_id": id, "ext": "png", "data_base64": b64(bytes) })
}

/// Asks until the deck has a live link to `alpha`.
async fn paste(c: &BoardClient, h: &[(&str, &str)], body: Value) -> Http {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.post(PATH, h, body.clone()).await;
        let waiting = [(404, "unknown_station"), (409, "station_unreachable")];
        if !waiting.contains(&(r.status, r.code())) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_owner_pastes_an_image_through_a_real_station() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let paste_dir = tmp.path().join("s").join("paste");
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(
        &tmp.path().join("s"),
        root.canonicalize().unwrap(),
        Some(paste_dir.clone()),
    )
    .await;
    st.join(
        &node
            .invite(fleetdeck_deck::knobs::DeckKnobs::default().invite)
            .unwrap(),
    )
    .await
    .unwrap();
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    let bytes = png(1000);
    let r = paste(&board, &h, body("upload-0001", &bytes)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let path = PathBuf::from(r.body["path"].as_str().unwrap());
    assert!(path.is_absolute());
    assert_eq!(path.parent().unwrap(), paste_dir);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    // A retry with the same id and bytes answers the same path.
    let again = paste(&board, &h, body("upload-0001", &bytes)).await;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.body["path"], r.body["path"]);

    // The station's refusals map to HTTP statuses.
    expect(
        &paste(&board, &h, body("upload-0001", &png(5))).await,
        409,
        "conflict",
    );
    expect(
        &paste(&board, &h, body("upload-0002", b"not an image")).await,
        400,
        "invalid_params",
    );
    expect(
        &paste(&board, &h, body("upload-0003", &png(716_800))).await,
        413,
        "too_large",
    );

    // The audit row names the station, never the data.
    let rows = board.store.audit_log(100).await.unwrap();
    let ok = rows
        .iter()
        .find(|e| e.action == "files.paste" && e.result == AuditResult::Ok)
        .unwrap();
    assert_eq!(ok.target.as_deref(), Some("alpha"));
    assert_eq!(ok.detail, None);
    assert!(!format!("{rows:?}").contains(&b64(&bytes)));

    // A token is not the owner's board login (DL-138), whatever its scopes.
    let token = board
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let before = std::fs::read_dir(&paste_dir).unwrap().count();
    let r = board
        .post(
            PATH,
            &[("Authorization", bearer.as_str())],
            body("upload-0004", &bytes),
        )
        .await;
    expect(&r, 403, "owner_only");
    assert_eq!(std::fs::read_dir(&paste_dir).unwrap().count(), before);

    // The CSRF header is needed.
    let r = board
        .post(
            PATH,
            &[("Cookie", cookie.as_str())],
            body("upload-0005", &bytes),
        )
        .await;
    expect(&r, 403, "csrf");
}

#[tokio::test]
async fn a_body_above_one_mib_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (_node, board) = deck(&tmp.path().join("d")).await;
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let big = json!({
        "upload_id": "upload-0001",
        "ext": "png",
        "data_base64": "A".repeat(1_100_000),
    });
    let r = board.post(PATH, &h, big).await;
    assert_eq!(r.status, 413, "{}", r.text);
}

#[tokio::test]
async fn a_station_without_a_paste_directory_answers_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.canonicalize().unwrap(), None).await;
    st.join(
        &node
            .invite(fleetdeck_deck::knobs::DeckKnobs::default().invite)
            .unwrap(),
    )
    .await
    .unwrap();
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let r = paste(&board, &h, body("upload-0001", &png(10))).await;
    assert_eq!((r.status, r.code()), (502, "station_error"), "{}", r.body);
    assert!(
        r.body["message"]
            .as_str()
            .unwrap()
            .starts_with("unsupported:")
    );
}
