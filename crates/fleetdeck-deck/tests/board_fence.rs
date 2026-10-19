//! The board sign-out on the target machine (R13 P7 §7.6) over loopback
//! HTTPS: an input operation row that exposes an address signs the owner and
//! member sessions of that address out, refuses a sign-in from it, closes its
//! sockets, and keeps doing so after a restart of the deck.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "board_fence/race.rs"]
mod race;
mod share_support;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use common::board::{BoardClient, Http, Ws, board, board_with_node};
use common::*;
use fleetdeck_core::Secret;
use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::identity::Identity;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::board_fence::{SessionKind, SignedOut, revoke_before_dispatch};
use fleetdeck_deck::http::tls::pinned_client;
use fleetdeck_deck::serve::{RunningDeck, StartOptions};
use fleetdeck_deck::store::{AuditResult, LoginMethod};
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::{ShellConfig, Shells};
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use futures_util::{SinkExt, StreamExt};
use rusqlite::params;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_tungstenite::tungstenite::Message;

const SIGNED_OUT_TEXT: &str = "You were signed out because an agent used desktop input on the station where this board is open.";
const BLOCKED_TEXT: &str =
    "An agent is using desktop input on this station. Sign in again in a minute.";
const T: Duration = Duration::from_secs(10);

/// Inserts an `attempted` input operation row that exposes `addrs` (a JSON
/// array text) since `attempted_at`, and clears the cache of the deck.
async fn expose(
    store: &DeckStore,
    id: &str,
    attempted_at: i64,
    finished_at: Option<i64>,
    addrs: &str,
) {
    let (id, addrs) = (id.to_owned(), addrs.to_owned());
    store
        .db()
        .call(move |c| {
            c.execute(
                "INSERT INTO coordinator_desktop_ops
                 (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id,
                  generation, caller_station, caller_session, target_station, resource, state,
                  charged, created_at, updated_at, attempted_at, finished_at, exposure_addrs)
                 VALUES (?1, 'g', ?1, 'h', 'type_text', 'input', 'b', 1, 'cs', 'cn', 'ts', 'r',
                         'attempted', '{}', 0, 0, ?2, ?3, ?4)",
                params![id, attempted_at, finished_at, addrs],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store.invalidate_exposure();
}

fn now(f: &Fixture) -> i64 {
    i64::try_from(f.det.now_ms()).unwrap()
}

fn signed_out(r: &Http) {
    assert_eq!(
        (r.status, r.code()),
        (401, "signed_out_for_input"),
        "{}",
        r.body
    );
    assert_eq!(r.body["message"], SIGNED_OUT_TEXT);
}

async fn audit_rows(c: &BoardClient) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = c
        .store
        .audit_log(100)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "board.signed_out_for_input")
        .map(|e| {
            assert_eq!(e.result, AuditResult::Ok);
            (e.actor, e.target.unwrap(), e.detail.unwrap())
        })
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn a_request_from_an_exposed_address_signs_the_session_out_for_good() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = c.login().await;
    let read = [("Cookie", cookie.as_str())];
    let write = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    assert_eq!(c.get("/api/auth/session", &read).await.status, 200);
    let (cookie2, _) = c.login().await;

    expose(&f.store, "op1", now(&f), None, r#"["127.0.0.1"]"#).await;
    // A mutating request is refused the same way; the first request deleted
    // the session, so the next one finds no login at all.
    signed_out(&c.post("/api/auth/logout", &write, json!({})).await);
    let r = c.get("/api/auth/session", &read).await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
    // A read from a second session is refused the same way.
    signed_out(
        &c.get("/api/auth/session", &[("Cookie", cookie2.as_str())])
            .await,
    );
    // The audit row: one row for the one deleted session, no address.
    let rows = audit_rows(&c).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "deck");
    assert!(rows[0].1.starts_with("owner:") && rows[0].1.len() == "owner:".len() + 8);
    assert_eq!(rows[0].2, "rule=2 op=-");
    assert!(!format!("{rows:?}").contains("127.0.0.1"));

    // After the window (45 s + 60 s) the old cookie stays refused: the
    // session is gone.
    f.det.advance(Duration::from_secs(200));
    f.store.invalidate_exposure();
    let r = c.get("/api/auth/session", &read).await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
}

#[tokio::test]
async fn a_request_from_another_address_passes_and_records_its_address() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, _) = c.login().await;
    let read = [("Cookie", cookie.as_str())];
    expose(&f.store, "op1", now(&f), None, r#"["10.9.9.9","fd7a::1"]"#).await;
    assert_eq!(c.get("/api/auth/session", &read).await.status, 200);
    assert!(audit_rows(&c).await.is_empty());
    // The session carries the address of the board connection.
    let addr: Option<String> = f
        .store
        .db()
        .call(|c| Ok(c.query_row("SELECT last_addr FROM login_sessions", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(addr.as_deref(), Some("127.0.0.1"));
}

#[tokio::test]
async fn an_ipv4_mapped_address_in_the_snapshot_is_not_the_canonical_form() {
    // A snapshot holds canonical text only; a mapped spelling is not found.
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, _) = c.login().await;
    let read = [("Cookie", cookie.as_str())];
    expose(&f.store, "op1", now(&f), None, r#"["::ffff:127.0.0.1"]"#).await;
    assert_eq!(c.get("/api/auth/session", &read).await.status, 200);
}

#[tokio::test]
async fn a_sign_in_from_an_exposed_address_is_refused_with_no_cookie_until_the_tail_has_passed() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    f.store
        .set_password(Secret::new("a long enough password".to_owned()))
        .await
        .unwrap();
    let link = f.store.create_login_link().await.unwrap();
    expose(&f.store, "op1", now(&f), None, r#"["127.0.0.1"]"#).await;

    let blocked = |r: &Http| {
        assert_eq!(
            (r.status, r.code()),
            (403, "sign_in_blocked_for_input"),
            "{}",
            r.body
        );
        assert_eq!(r.body["message"], BLOCKED_TEXT);
        assert!(r.header("set-cookie").is_none());
        assert!(r.body.get("csrf").is_none());
    };
    blocked(
        &c.post(
            "/api/auth/login",
            &[],
            json!({ "password": "a long enough password" }),
        )
        .await,
    );
    blocked(
        &c.post("/api/auth/link", &[], json!({ "token": link.expose() }))
            .await,
    );
    blocked(
        &c.post(
            "/api/auth/join",
            &[],
            json!({ "token": "t", "display_name": "Ann" }),
        )
        .await,
    );
    blocked(
        &c.post("/api/auth/display-link", &[], json!({ "token": "t" }))
            .await,
    );
    blocked(&c.post("/api/displays/pair", &[], json!({})).await);
    // A wrong password from the exposed address is refused the same way and
    // does not count as a failed attempt.
    for _ in 0..8 {
        blocked(
            &c.post("/api/auth/login", &[], json!({ "password": "wrong" }))
                .await,
        );
    }
    let sessions: i64 = f
        .store
        .db()
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM login_sessions", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(sessions, 0);

    // The tail: exposure ends at 45 s, the refusal at 105 s.
    f.det.advance(Duration::from_secs(104));
    f.store.invalidate_exposure();
    blocked(
        &c.post(
            "/api/auth/login",
            &[],
            json!({ "password": "a long enough password" }),
        )
        .await,
    );
    f.det.advance(Duration::from_secs(2));
    f.store.invalidate_exposure();
    // The login link was not used up by the refusal, and the limiter did not
    // count the refusals.
    let r = c
        .post("/api/auth/link", &[], json!({ "token": link.expose() }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.header("set-cookie").is_some());
    let r = c
        .post(
            "/api/auth/login",
            &[],
            json!({ "password": "a long enough password" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn a_member_session_and_the_owner_session_are_signed_out_the_same_way() {
    let (f, _station, c, owner_h) = share_support::setup().await;
    let (member_h, _, _) = share_support::join(
        &c,
        &owner_h,
        json!({ "kind": "board", "role": "approver" }),
        "Ann",
    )
    .await;
    let m = share_support::refs(&member_h);
    assert_eq!(c.get("/api/auth/session", &m).await.status, 200);
    // The member session records its address at the join.
    let addr: Option<String> = f
        .store
        .db()
        .call(|c| Ok(c.query_row("SELECT last_addr FROM member_sessions", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(addr.as_deref(), Some("127.0.0.1"));

    expose(&f.store, "op1", now(&f), Some(now(&f)), r#"["127.0.0.1"]"#).await;
    signed_out(&c.get("/api/auth/session", &m).await);
    signed_out(
        &c.get("/api/auth/session", &share_support::refs(&owner_h))
            .await,
    );
    let rows = audit_rows(&c).await;
    let kinds: Vec<&str> = rows
        .iter()
        .map(|r| r.1.split(':').next().unwrap())
        .collect();
    assert_eq!(kinds, ["member", "owner"], "{rows:?}");
    // The member cookie stays dead after the window.
    f.det.advance(Duration::from_secs(300));
    f.store.invalidate_exposure();
    let r = c.get("/api/auth/session", &m).await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
}

fn socket_headers<'a>(
    c: &'a BoardClient,
    cookie: &'a str,
    origin: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let _ = c;
    vec![("Cookie", cookie), ("Origin", origin)]
}

#[tokio::test]
async fn a_board_socket_closes_with_4401_when_its_session_is_signed_out() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, _) = c.login().await;
    let origin = c.origin();
    let mut ws = c
        .ws("/api/ws", &socket_headers(&c, &cookie, &origin))
        .await
        .unwrap();
    ws.next_of("hello").await;
    ws.next_of("snapshot").await;

    // K1 deletes the session rows in its transaction, then revokes.
    let hash: String = f
        .store
        .db()
        .call(|c| Ok(c.query_row("SELECT session_hash FROM login_sessions", [], |r| r.get(0))?))
        .await
        .unwrap();
    f.store
        .db()
        .call(|c| Ok(c.execute("DELETE FROM login_sessions", []).map(|_| ())?))
        .await
        .unwrap();
    let rows = [SignedOut {
        kind: SessionKind::Owner,
        session_hash: hash,
    }];
    assert!(revoke_before_dispatch(&f.store, &rows, &[], "op1").await);
    assert_eq!(ws.next().await.unwrap_err(), Some(4401));

    // An upgrade from an exposed address is refused before the socket.
    let (cookie, _) = c.login().await;
    expose(&f.store, "op1", now(&f), None, r#"["127.0.0.1"]"#).await;
    let refused = c.ws("/api/ws", &socket_headers(&c, &cookie, &origin)).await;
    assert_eq!(refused.err(), Some(401));
}

#[tokio::test]
async fn a_socket_from_a_snapshot_address_closes_and_its_session_is_deleted() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, _) = c.login().await;
    let origin = c.origin();
    let mut ws = c
        .ws("/api/ws", &socket_headers(&c, &cookie, &origin))
        .await
        .unwrap();
    ws.next_of("snapshot").await;
    assert!(revoke_before_dispatch(&f.store, &[], &["10.1.1.1".to_owned()], "op1").await);
    ws.quiet(Duration::from_millis(300)).await;
    // The session of a socket at another address is untouched.
    let read = [("Cookie", cookie.as_str())];
    assert_eq!(c.get("/api/auth/session", &read).await.status, 200);
    assert!(revoke_before_dispatch(&f.store, &[], &["127.0.0.1".to_owned()], "op1").await);
    assert_eq!(ws.next().await.unwrap_err(), Some(4401));
    // The session of the revoked socket is deleted, not only the socket.
    let r = c.get("/api/auth/session", &read).await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
}

// ---------------------------------------------------------------- shell

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

async fn station(dir: &std::path::Path, root: PathBuf) -> Arc<StationNode> {
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(vec![root], 2, OfflineApprovalPolicy::Wait, BTreeMap::new())
        .with_shell(true, 2);
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::new(),
        ids,
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let shells = Arc::new(Shells::new(ShellConfig {
        program: Some(("/bin/sh".into(), Vec::new())),
        instances: BTreeMap::new(),
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

#[tokio::test]
async fn a_shell_relay_ends_on_a_revoke_and_forwards_no_byte_after_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&tmp.path().join("d/deck.db"), clock, ids)
        .await
        .unwrap();
    let node = DeckNode::start(
        store,
        Arc::new(Identity::generate().unwrap()),
        &["127.0.0.1:0".parse().unwrap()],
        fast(),
    )
    .await
    .unwrap();
    let c = board_with_node(Arc::clone(&node)).await;
    let st = station(&tmp.path().join("s"), root.clone()).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, csrf) = c.login().await;
    let write = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let body = json!({ "command_id": "sh-open-1", "change": {
            "op": "open", "shell_id": "main", "cwd": root.to_str().unwrap(),
            "cols": 80, "rows": 24 } });
        let r = c.post("/api/stations/alpha/shells", &write, body).await;
        if r.status == 200 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let origin = c.origin();
    let headers = socket_headers(&c, &cookie, &origin);
    let mut ws = loop {
        match c
            .ws("/api/stations/alpha/shells/main/attach", &headers)
            .await
        {
            Ok(ws) => break ws,
            Err(409) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(status) => panic!("attach: {status}"),
        }
    };
    ws.0.send(Message::Binary(b"echo up-$((40+2))\n".to_vec().into()))
        .await
        .unwrap();
    read_until(&mut ws, "up-42").await;

    // The revoke ends the relay before the dispatch.
    assert!(revoke_before_dispatch(&c.store, &[], &["127.0.0.1".to_owned()], "op1").await);
    let marker = root.join("typed-after-revoke");
    let _ =
        ws.0.send(Message::Binary(
            format!("touch {}\n", marker.display()).into_bytes().into(),
        ))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !marker.exists(),
        "a byte reached the shell after the revoke"
    );
    // The socket ends with 4401 and the reason.
    let mut close = None;
    while let Ok(Some(m)) = tokio::time::timeout(T, ws.0.next()).await {
        match m {
            Ok(Message::Close(frame)) => {
                close = frame;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    if let Some(frame) = close {
        assert_eq!(u16::from(frame.code), 4401);
        assert_eq!(frame.reason.as_str(), "signed_out_for_input");
    }
    // The revoke deleted the session of the socket. The shell itself keeps
    // running: a new sign-in (no input is exposing the address) sees it.
    let gone = c
        .get("/api/stations/alpha/shells", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(gone.status, 401, "{}", gone.body);
    let (again, _) = c.login().await;
    let r = c
        .get("/api/stations/alpha/shells", &[("Cookie", again.as_str())])
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body[0]["state"], "running");
}

// -------------------------------------------------------------- restart

/// A GET over HTTPS to `addr` that trusts `cert` only.
async fn get_with(
    addr: &str,
    cert: CertificateDer<'static>,
    path: &str,
    cookie: &str,
) -> (u16, Value) {
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let tls = TlsConnector::from(pinned_client(cert));
    let mut s = tls
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let text = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nCookie: {cookie}\r\n\r\n"
    );
    s.write_all(text.as_bytes()).await.unwrap();
    s.flush().await.unwrap();
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(30), s.read_to_end(&mut raw)).await;
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_restarted_deck_refuses_the_first_request_from_an_exposed_address() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = NodeDirs {
        config: tmp.path().join("config"),
        data: tmp.path().join("data"),
    };
    std::fs::create_dir_all(&dirs.config).unwrap();
    let path = dirs.deck_config();
    std::fs::write(
        &path,
        "name = \"d\"\ntailnet_auto = false\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let start = || async {
        RunningDeck::start(
            &dirs,
            &DeckConfig::load(&path).unwrap(),
            fast(),
            StartOptions::default(),
        )
        .await
        .unwrap()
    };

    let first = start().await;
    let login = first
        .store
        .create_login_at(LoginMethod::Link, Some("127.0.0.1"))
        .await
        .unwrap();
    let cookie = format!("fd_session={}", login.cookie.expose());
    // An operation whose input started 10 s ago and has no result yet.
    let at = first.store.now_ms() - 10_000;
    expose(&first.store, "op1", at, None, r#"["127.0.0.1"]"#).await;
    first.stop().await;

    let second = start().await;
    let endpoint = second.endpoint.clone().unwrap();
    let pem = std::fs::read_to_string(dirs.deck().join("board-selfsigned.crt")).unwrap();
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    let addr = endpoint.url.trim_start_matches("https://").to_owned();
    let (status, body) = get_with(
        &addr,
        CertificateDer::from(der.clone()),
        "/api/auth/session",
        &cookie,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("signed_out_for_input")),
        "{body}"
    );
    // The session is gone for good.
    let (status, body) = get_with(
        &addr,
        CertificateDer::from(der),
        "/api/auth/session",
        &cookie,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("login_required")),
        "{body}"
    );
    second.stop().await;
}

#[tokio::test]
async fn a_shell_attach_paused_before_the_upgrade_never_relays_a_byte() {
    use fleetdeck_proto::link::Outcome;
    use fleetdeck_proto::shell::ShellAttached;

    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = Arc::new(board(Arc::clone(&f.ctx)).await);
    let (cookie, _) = c.login().await;
    let origin = c.origin();
    let (cookie_t, origin_t, client) = (cookie.clone(), origin.clone(), Arc::clone(&c));
    let attach = tokio::spawn(async move {
        client
            .ws(
                "/api/stations/fake/shells/main/attach",
                &[("Cookie", cookie_t.as_str()), ("Origin", origin_t.as_str())],
            )
            .await
    });
    // The handler waits for the station: the guard registered the socket and
    // `on_upgrade` has not run.
    let req = loop {
        let req = next_request(&mut station).await;
        if req.method == "shell.attach" {
            break req;
        }
    };
    let entries = c.store.registry().entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(
        (entries[0].state, entries[0].revoked),
        (
            fleetdeck_deck::board_fence::registry::SocketState::Pending,
            false
        )
    );

    // The sign-out commits while the attach is paused.
    let hash: String = f
        .store
        .db()
        .call(|c| Ok(c.query_row("SELECT session_hash FROM login_sessions", [], |r| r.get(0))?))
        .await
        .unwrap();
    f.store
        .db()
        .call(|c| Ok(c.execute("DELETE FROM login_sessions", []).map(|_| ())?))
        .await
        .unwrap();
    let rows = [SignedOut {
        kind: SessionKind::Owner,
        session_hash: hash,
    }];
    // A pending socket has no relay: nothing to wait for.
    assert!(revoke_before_dispatch(&f.store, &rows, &[], "op1").await);

    // The station answers and has a byte ready.
    let data = station.data.as_ref().expect("a data channel");
    let token = data.handle.issue_token("shell").unwrap();
    let answer = json!(ShellAttached {
        stream_token: token.as_str().to_owned(),
    });
    station
        .control
        .respond(req.id, Outcome::Ok(answer))
        .await
        .unwrap();
    let mut ws = attach.await.unwrap().expect("the upgrade is accepted");
    // The socket closes with 4401 and carries no data message.
    let mut binary = 0;
    let close = loop {
        match tokio::time::timeout(T, ws.0.next()).await.unwrap() {
            Some(Ok(Message::Binary(_))) => binary += 1,
            Some(Ok(Message::Close(frame))) => break frame,
            Some(Ok(_)) => {}
            other => panic!("the socket ended without a close frame: {other:?}"),
        }
    };
    assert_eq!(binary, 0);
    let close = close.expect("a close frame");
    assert_eq!(u16::from(close.code), 4401);
    assert_eq!(close.reason.as_str(), "signed_out_for_input");
    assert!(c.store.registry().entries().is_empty());
}
