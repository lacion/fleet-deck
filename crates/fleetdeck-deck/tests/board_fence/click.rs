//! The board sign-out reuse for `pointer_click` (R13 P7b §12): an exposure
//! made only by a click row signs the session out, a sign-in is refused while
//! a click row exposes the address, and the restart recovery of an attempted
//! click exposes the address again. Without `pointer_click` in the tool lists
//! of the attempted step and of `read_exposed`, a click is outside all three.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use fleetdeck_core::Secret;
use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_deck::serve::{RunningDeck, StartOptions};
use fleetdeck_deck::store::LoginMethod;
use serde_json::json;
use tokio_rustls::rustls::pki_types::CertificateDer;

use super::{
    BLOCKED_TEXT, audit_rows, board, expose_tool, fast, fixture, get_with, now, signed_out,
};
use crate::common::board::Http;

#[tokio::test]
async fn an_exposure_made_only_by_a_click_signs_the_session_out_for_good() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, _) = c.login().await;
    let read = [("Cookie", cookie.as_str())];
    assert_eq!(c.get("/api/auth/session", &read).await.status, 200);
    // A click row alone exposes the address (the row of no other tool).
    expose_tool(
        &f.store,
        "pointer_click",
        "op1",
        (now(&f), None),
        r#"["127.0.0.1"]"#,
    )
    .await;
    let exposed = f.store.exposed_addresses(f.store.now_ms()).await.unwrap();
    assert!(exposed.contains("127.0.0.1"), "{exposed:?}");
    signed_out(&c.get("/api/auth/session", &read).await);
    let r = c.get("/api/auth/session", &read).await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
    let rows = audit_rows(&c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(!format!("{rows:?}").contains("127.0.0.1"));
}

#[tokio::test]
async fn a_sign_in_is_refused_while_a_click_exposes_the_address_until_the_tail_has_passed() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    f.store
        .set_password(Secret::new("a long enough password".to_owned()))
        .await
        .unwrap();
    expose_tool(
        &f.store,
        "pointer_click",
        "op1",
        (now(&f), Some(now(&f))),
        r#"["127.0.0.1"]"#,
    )
    .await;
    let blocked = |r: &Http| {
        assert_eq!(
            (r.status, r.code()),
            (403, "sign_in_blocked_for_input"),
            "{}",
            r.body
        );
        assert_eq!(r.body["message"], BLOCKED_TEXT);
        assert!(r.header("set-cookie").is_none());
    };
    blocked(
        &c.post(
            "/api/auth/login",
            &[],
            json!({ "password": "a long enough password" }),
        )
        .await,
    );
    // The built terminal timing: the exposure ends 5 s after the end and the
    // refusal 60 s later, so the sign-in works after 65 s.
    f.det.advance(Duration::from_secs(64));
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
async fn the_attempted_step_signs_out_the_sessions_for_a_click_row_and_not_for_a_prepared_one() {
    let f = fixture().await;
    let store = f.store.clone();
    let login = store
        .create_login_at(LoginMethod::Password, Some("10.7.7.7"))
        .await
        .unwrap();
    // A `prepared` click row: the attempted step must accept the tool name of
    // the click.
    let at = now(&f);
    let id = "click-op";
    store
        .db()
        .call(move |c| {
            c.execute(
                "INSERT INTO coordinator_desktop_ops
                 (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id,
                  generation, caller_station, caller_session, target_station, resource, state,
                  charged, created_at, updated_at)
                 VALUES (?1, 'g', ?1, 'h', 'pointer_click', 'input', 'b', 1, 'cs', 'cn', 'ts',
                         'r', 'prepared', '{}', 0, 0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let signed = store
        .desktop_op_attempted_input(id, &["10.7.7.7".to_owned()], at)
        .await
        .unwrap()
        .expect("a click row is an input operation");
    assert_eq!(signed.len(), 1);
    assert!(store.login_session(&login.cookie).await.unwrap().is_none());
    // The row is `attempted` with its snapshot, and the address is exposed.
    store.invalidate_exposure();
    let exposed = store.exposed_addresses(at).await.unwrap();
    assert!(exposed.contains("10.7.7.7"), "{exposed:?}");
}

#[tokio::test]
async fn a_restarted_deck_refuses_the_first_request_from_an_address_that_a_click_exposed() {
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
    // A click whose input started 10 s ago and has no result yet.
    let at = first.store.now_ms() - 10_000;
    expose_tool(
        &first.store,
        "pointer_click",
        "op1",
        (at, None),
        r#"["127.0.0.1"]"#,
    )
    .await;
    first.stop().await;

    // The restart recovery makes the attempted click `unknown` and keeps its
    // snapshot; the first request from the address is refused.
    let second = start().await;
    let state: String = second
        .store
        .db()
        .call(|c| {
            Ok(c.query_row(
                "SELECT state FROM coordinator_desktop_ops WHERE operation_id = 'op1'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(state, "unknown");
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
