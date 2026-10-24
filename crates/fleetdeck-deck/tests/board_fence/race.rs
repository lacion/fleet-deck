//! The two races of the board sign-out (R13 P7 §7.6, PRE-PR round 1): a
//! session that the sockets name but the last address does not, and a sign-in
//! that comes after the attempted step.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::board_fence::{audit_signed_out, revoke_before_dispatch};
use futures_util::StreamExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use super::{BLOCKED_TEXT, T, audit_rows, board, fixture, now, share_support, socket_headers};
use crate::common::board::Http;
use crate::common::*;

/// Inserts a `prepared` input operation row that has no snapshot yet.
async fn prepared(store: &DeckStore, id: &str) {
    let id = id.to_owned();
    store
        .db()
        .call(move |c| {
            c.execute(
                "INSERT INTO coordinator_desktop_ops
                 (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id,
                  generation, caller_station, caller_session, target_station, resource, state,
                  charged, created_at, updated_at)
                 VALUES (?1, 'g', ?1, 'h', 'type_text', 'input', 'b', 1, 'cs', 'cn', 'ts', 'r',
                         'prepared', '{}', 0, 0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Moves the last address of every owner session away from the machine.
async fn move_sessions_away(store: &DeckStore) {
    store
        .db()
        .call(|c| {
            c.execute("UPDATE login_sessions SET last_addr = '10.9.9.9'", [])?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The order of the input dispatch (K1) for the snapshot `["127.0.0.1"]`.
async fn dispatch(store: &DeckStore, id: &str, at: i64) -> bool {
    let addrs = vec!["127.0.0.1".to_owned()];
    let signed = store
        .desktop_op_attempted_input(id, &addrs, at)
        .await
        .unwrap()
        .expect("the row was prepared");
    assert_eq!(signed.len(), 1, "{signed:?}");
    store.invalidate_exposure();
    audit_signed_out(store, &signed, id).await;
    revoke_before_dispatch(store, &signed, &addrs, id).await
}

fn blocked(r: &Http) {
    assert_eq!(
        (r.status, r.code()),
        (403, "sign_in_blocked_for_input"),
        "{}",
        r.body
    );
    assert_eq!(r.body["message"], BLOCKED_TEXT);
    assert!(r.header("set-cookie").is_none());
    assert!(r.body.get("csrf").is_none());
}

async fn count(store: &DeckStore, table: &'static str) -> i64 {
    store
        .db()
        .call(move |c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_live_board_socket_at_a_snapshot_address_signs_its_session_out_for_good() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, _) = c.login().await;
    let origin = c.origin();
    let mut ws = c
        .ws("/api/ws", &socket_headers(&c, &cookie, &origin))
        .await
        .unwrap();
    ws.next_of("snapshot").await;
    // The last address of the session is another one: only the socket tells
    // that the session is on the machine.
    move_sessions_away(&f.store).await;
    prepared(&f.store, "op-r").await;
    assert!(dispatch(&f.store, "op-r", now(&f)).await);
    assert_eq!(ws.next().await.unwrap_err(), Some(4401));

    // After the exposure and the tail the cookie stays refused: the session
    // row is gone.
    f.det.advance(Duration::from_secs(200));
    f.store.invalidate_exposure();
    let r = c
        .get("/api/auth/session", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
    let rows = audit_rows(&c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].2, "rule=1 op=op-r");
}

#[tokio::test]
async fn a_pending_shell_upgrade_signs_its_session_out_for_good() {
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
    // The guard registered the socket; `on_upgrade` has not run.
    let req = loop {
        let req = next_request(&mut station).await;
        if req.method == "shell.attach" {
            break req;
        }
    };
    assert_eq!(c.store.registry().entries().len(), 1);
    move_sessions_away(&f.store).await;
    prepared(&f.store, "op-p").await;
    assert!(dispatch(&f.store, "op-p", now(&f)).await);

    // The station answers; the revoked socket never relays a byte.
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
    assert_eq!(u16::from(close.expect("a close frame").code), 4401);
    // The session is gone for good.
    f.det.advance(Duration::from_secs(200));
    f.store.invalidate_exposure();
    let r = c
        .get("/api/auth/session", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
    let rows = audit_rows(&c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].2, "rule=1 op=op-p");
}

#[tokio::test]
async fn an_owner_sign_in_that_slips_past_the_early_check_is_refused_in_the_transaction() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    f.store
        .set_password(Secret::new("a long enough password".to_owned()))
        .await
        .unwrap();
    let link = f.store.create_login_link().await.unwrap();
    // The early check reads the one-second cache of the exposed set. Fill it
    // while nothing is exposed, then commit the attempted step without
    // clearing it: the early check passes, as for a sign-in whose password
    // check began before the input.
    prepared(&f.store, "op-s").await;
    assert!(f.store.exposed_addresses(now(&f)).await.unwrap().is_empty());
    f.store
        .desktop_op_attempted_input("op-s", &["127.0.0.1".to_owned()], now(&f))
        .await
        .unwrap()
        .expect("the row was prepared");
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
    assert_eq!(count(&f.store, "login_sessions").await, 0);
}

#[tokio::test]
async fn a_member_join_that_slips_past_the_early_check_is_refused_and_keeps_its_invite() {
    let (f, _station, c, owner_h) = share_support::setup().await;
    let r = share_support::post(
        &c,
        &owner_h,
        "/api/shares",
        json!({ "kind": "board", "role": "approver", "uses": 1 }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap().to_owned();
    let token = url.split("#join=").nth(1).unwrap().to_owned();
    prepared(&f.store, "op-j").await;
    assert!(f.store.exposed_addresses(now(&f)).await.unwrap().is_empty());
    f.store
        .desktop_op_attempted_input("op-j", &["127.0.0.1".to_owned()], now(&f))
        .await
        .unwrap()
        .expect("the row was prepared");
    let body = json!({ "token": token, "display_name": "Ann" });
    blocked(&c.post("/api/auth/join", &[], body.clone()).await);
    assert_eq!(count(&f.store, "member_sessions").await, 0);
    // The refusal cost no invite use: after the tail the one use serves.
    f.det.advance(Duration::from_secs(200));
    f.store.invalidate_exposure();
    let r = c.post("/api/auth/join", &[], body).await;
    assert_eq!(r.status, 200, "{}", r.body);
}
