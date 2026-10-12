//! The human shell end to end (PLAN §6.8, D10; M3 item 6-11, DL-404): the
//! board opens, resizes and closes a station shell through audited commands,
//! and the shell's bytes travel on the link's data channel to a board
//! WebSocket. A board that reconnects receives the scrollback, then live
//! bytes; the shell survives a dropped board socket and a dropped link.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, Ws, board_with_node};
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
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const T: Duration = Duration::from_secs(10);

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

async fn station(dir: &std::path::Path, root: PathBuf, allow: bool) -> Arc<StationNode> {
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
    let manager = SessionManager::start(store, policy, Adapters::new(), ids)
        .await
        .unwrap();
    let shells = Arc::new(Shells::new(ShellConfig {
        allow,
        max_shells: 2,
        program: Some(("/bin/sh".into(), Vec::new())),
    }));
    StationNode::start_with_shells(
        Arc::new(manager),
        Arc::new(Identity::generate().unwrap()),
        "alpha".to_owned(),
        &[],
        fast(),
        shells,
    )
    .await
    .unwrap()
}

async fn deck(dir: &std::path::Path) -> (Arc<DeckNode>, BoardClient) {
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

struct Owner {
    cookie: String,
    csrf: String,
    origin: String,
}

impl Owner {
    fn read(&self) -> Vec<(&str, &str)> {
        vec![("Cookie", self.cookie.as_str())]
    }
    fn write(&self) -> Vec<(&str, &str)> {
        vec![
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ]
    }
    fn socket(&self) -> Vec<(&str, &str)> {
        vec![
            ("Cookie", self.cookie.as_str()),
            ("Origin", self.origin.as_str()),
        ]
    }
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

/// Sends one shell change; retries while the deck has no link or no
/// snapshot of `alpha` yet.
async fn change(c: &BoardClient, o: &Owner, id: &str, change: Value) -> Http {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let body = json!({ "command_id": id, "change": change.clone() });
        let r = c.post("/api/stations/alpha/shells", &o.write(), body).await;
        let waiting = [
            (404, "unknown_station"),
            (409, "station_unreachable"),
            (409, "not_synced"),
        ];
        if !waiting.contains(&(r.status, r.code())) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Opens the shell socket; retries a refusal (409) while the link comes
/// back up.
async fn attach(c: &BoardClient, o: &Owner, shell: &str) -> Ws {
    let path = format!("/api/stations/alpha/shells/{shell}/attach");
    let deadline = tokio::time::Instant::now() + T;
    loop {
        match c.ws(&path, &o.socket()).await {
            Ok(ws) => return ws,
            Err(409) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(status) => panic!("attach: {status}"),
        }
    }
}

/// Reads binary messages until the text holds `needle`.
async fn read_until(ws: &mut Ws, needle: &str) -> String {
    let mut seen = String::new();
    while !seen.contains(needle) {
        let m = tokio::time::timeout(T, ws.0.next())
            .await
            .unwrap_or_else(|_| panic!("no `{needle}` in {seen:?}"));
        match m {
            Some(Ok(Message::Binary(b))) => seen.push_str(&String::from_utf8_lossy(&b)),
            Some(Ok(_)) => {}
            other => panic!("socket ended ({other:?}) before `{needle}`: {seen:?}"),
        }
    }
    seen
}

async fn keys(ws: &mut Ws, text: &str) {
    ws.0.send(Message::Binary(text.as_bytes().to_vec().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_board_opens_a_station_shell_and_reattaches_to_its_scrollback_after_a_link_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), true).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, csrf) = board.login().await;
    let o = Owner {
        cookie,
        csrf,
        origin: board.origin(),
    };
    let cwd = root.to_str().unwrap();

    let r = change(
        &board,
        &o,
        "sh-open-1",
        json!({ "op": "open", "shell_id": "main", "cwd": cwd, "cols": 80, "rows": 24 }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "accepted");
    let r = board.get("/api/stations/alpha/shells", &o.read()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body[0]["shell_id"], "main");
    assert_eq!(r.body[0]["state"], "running");
    assert_eq!(r.body[0]["cwd"], cwd);

    // Live bytes both ways on the data channel.
    let mut ws = attach(&board, &o, "main").await;
    keys(&mut ws, "echo first-$((40+2))\n").await;
    read_until(&mut ws, "first-42").await;
    drop(ws);

    // The link drops; the station dials back. The shell keeps running.
    node.link().close_all("test");
    let mut ws = attach(&board, &o, "main").await;
    let backlog = read_until(&mut ws, "first-42").await;
    assert!(backlog.contains("echo first-"), "{backlog}");
    keys(&mut ws, "echo second-$((40+3))\n").await;
    read_until(&mut ws, "second-43").await;

    // Resize, and the refusals of the station keep their codes.
    let r = change(
        &board,
        &o,
        "sh-resize-1",
        json!({ "op": "resize", "shell_id": "main", "cols": 120, "rows": 40 }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = board.get("/api/stations/alpha/shells", &o.read()).await;
    assert_eq!(
        (r.body[0]["cols"].as_u64(), r.body[0]["rows"].as_u64()),
        (Some(120), Some(40))
    );
    let outside = tmp.path().to_str().unwrap();
    let r = change(
        &board,
        &o,
        "sh-open-2",
        json!({ "op": "open", "shell_id": "out", "cwd": outside, "cols": 80, "rows": 24 }),
    )
    .await;
    expect(&r, 400, "invalid_command");
    let r = change(
        &board,
        &o,
        "sh-resize-2",
        json!({ "op": "resize", "shell_id": "main", "cols": 0, "rows": 40 }),
    )
    .await;
    expect(&r, 400, "invalid_command");
    let r = change(
        &board,
        &o,
        "sh-open-3",
        json!({ "op": "open", "shell_id": "main", "cwd": cwd, "cols": 80, "rows": 24 }),
    )
    .await;
    expect(&r, 409, "invalid_state");

    // Close: the socket ends, the shell is gone, a new attach is refused.
    let r = change(
        &board,
        &o,
        "sh-close-1",
        json!({ "op": "close", "shell_id": "main" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let end = tokio::time::timeout(T, async {
        loop {
            match ws.0.next().await {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "the socket did not end after shell.close");
    let refused = board
        .ws("/api/stations/alpha/shells/main/attach", &o.socket())
        .await
        .err();
    assert_eq!(refused, Some(409));
    let r = board.get("/api/stations/alpha/shells", &o.read()).await;
    assert_eq!(r.body, json!([]));

    // Audited with the caller; keystrokes are not.
    for action in ["shell.open", "shell.resize", "shell.close"] {
        assert!(
            !board.audited(action, AuditResult::Ok).await.is_empty(),
            "{action}"
        );
    }
    // (The refusals also hold the `not_synced` waits of `change`.)
    let refused = board.audited("shell.open", AuditResult::Refused).await;
    for code in ["invalid_command", "invalid_state"] {
        assert!(refused.contains(&Some(code.to_owned())), "{refused:?}");
    }

    // The shell routes are owner-only (the admin scope).
    let day = 86_400_000;
    let token = board
        .store
        .create_token(
            "driver".into(),
            BTreeSet::from([Scope::Read, Scope::Drive]),
            day,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let r = board
        .get(
            "/api/stations/alpha/shells",
            &[("Authorization", bearer.as_str())],
        )
        .await;
    expect(&r, 403, "forbidden");

    st.shutdown();
    node.shutdown();
}

#[tokio::test]
async fn a_station_with_allow_shell_off_refuses_every_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), false).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, csrf) = board.login().await;
    let o = Owner {
        cookie,
        csrf,
        origin: board.origin(),
    };
    let r = change(
        &board,
        &o,
        "sh-open-1",
        json!({ "op": "open", "shell_id": "main", "cwd": root.to_str().unwrap(), "cols": 80, "rows": 24 }),
    )
    .await;
    expect(&r, 409, "unsupported");
    let refused = board.audited("shell.open", AuditResult::Refused).await;
    assert!(
        refused.contains(&Some("unsupported".to_owned())),
        "{refused:?}"
    );
    st.shutdown();
    node.shutdown();
}

/// Waits for the deck to close the shell socket with code 4401.
async fn closed_for_login(ws: &mut Ws) {
    loop {
        let m = tokio::time::timeout(T, ws.0.next())
            .await
            .expect("the socket stays open after the login ended");
        match m {
            Some(Ok(Message::Close(Some(frame)))) => {
                assert_eq!(u16::from(frame.code), 4401, "{frame:?}");
                return;
            }
            Some(Ok(_)) => {}
            other => panic!("socket ended without a 4401 close: {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_attached_shell_socket_closes_after_a_logout_and_a_token_revocation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), true).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, csrf) = board.login().await;
    let o = Owner {
        cookie,
        csrf,
        origin: board.origin(),
    };
    let r = change(
        &board,
        &o,
        "sh-open-1",
        json!({ "op": "open", "shell_id": "main", "cwd": root.to_str().unwrap(), "cols": 80, "rows": 24 }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);

    // The owner logs out: the attached socket closes with 4401.
    let mut ws = attach(&board, &o, "main").await;
    keys(&mut ws, "echo before-$((40+2))\n").await;
    read_until(&mut ws, "before-42").await;
    let r = board.post("/api/auth/logout", &o.write(), json!({})).await;
    assert_eq!(r.status, 204, "{}", r.body);
    closed_for_login(&mut ws).await;
    // The old cookie cannot attach again.
    let path = "/api/stations/alpha/shells/main/attach";
    assert_eq!(board.ws(path, &o.socket()).await.err(), Some(401));

    // An admin token is not the owner's board login (DL-138): its ticket,
    // its list and its open command are refused, and nothing changes.
    let day = 86_400_000;
    let token = board
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Admin]),
            day,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let h = [("Authorization", bearer.as_str())];
    let t = board.post("/api/ws/ticket", &h, json!({})).await;
    assert_eq!(t.status, 200, "{}", t.body);
    let with_ticket = format!("{path}?ticket={}", t.body["ticket"].as_str().unwrap());
    assert_eq!(board.ws(&with_ticket, &[]).await.err(), Some(403));
    let r = board.get("/api/stations/alpha/shells", &h).await;
    expect(&r, 403, "owner_only");
    let r = board
        .post(
            "/api/stations/alpha/shells",
            &h,
            json!({ "command_id": "sh-token-1", "change": { "op": "open", "shell_id": "other", "cwd": root.to_str().unwrap(), "cols": 80, "rows": 24 } }),
        )
        .await;
    expect(&r, 403, "owner_only");
    let refused = board.audited("shell.open", AuditResult::Refused).await;
    assert!(
        refused.contains(&Some("owner_only".to_owned())),
        "{refused:?}"
    );

    // The shell itself keeps running on the station.
    let (cookie, csrf) = board.login().await;
    let o = Owner {
        cookie,
        csrf,
        origin: board.origin(),
    };
    let r = board.get("/api/stations/alpha/shells", &o.read()).await;
    assert_eq!(r.body[0]["state"], "running", "{}", r.body);
    assert_eq!(r.body.as_array().map(Vec::len), Some(1), "{}", r.body);
    // The scrollback shows that the shell survived the first socket.
    let mut ws = attach(&board, &o, "main").await;
    read_until(&mut ws, "before-42").await;
    st.shutdown();
    node.shutdown();
}
