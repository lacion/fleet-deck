//! The store reads the owner knobs (K2): password, login, member, display,
//! token and audit limits, and rule 11 (a lowered limit ends the sessions).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::share::{ShareKind, ShareRole, Steering};
use fleetdeck_testkit::Det;

use super::DeckStore;
use crate::board_fence::registry::{SessionKind, SocketKind};
use crate::knobs::{DeckKnobs, end_sessions_over_lowered};
use crate::store::DeckError;
use crate::store::LoginMethod;
use crate::store::auth::secret_hash;
use crate::store::displays::NewDisplay;
use crate::store::shares::NewShareRow;
use crate::store::stations::NewStation;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn days(n: i64) -> Duration {
    Duration::from_millis(u64::try_from(n * DAY_MS).unwrap())
}

fn knobs(change: impl FnOnce(&mut DeckKnobs)) -> DeckKnobs {
    let mut k = DeckKnobs::default();
    change(&mut k);
    k
}

fn secret(s: &str) -> Secret<String> {
    Secret::new(s.to_owned())
}

async fn rows(st: &DeckStore, table: &'static str) -> i64 {
    st.db()
        .call(move |c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_password_shorter_than_the_configured_minimum_is_refused() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| k.min_password_chars = 8));
    let err = st.set_password(secret("1234567")).await.unwrap_err();
    assert!(
        matches!(&err, DeckError::Invalid(m) if m == "the password needs at least 8 characters"),
        "{err:?}"
    );
    st.set_password(secret("12345678")).await.unwrap();
    // A raised minimum refuses a password that the default accepts.
    st.set_knobs(knobs(|k| k.min_password_chars = 20));
    let err = st
        .set_password(secret("a password of 15!"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DeckError::Invalid(m) if m == "the password needs at least 20 characters"),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_login_idle_limit_is_the_configured_one() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| k.login_idle_ms = DAY_MS));
    let idle = st.create_login(LoginMethod::Link).await.unwrap();
    det.advance(days(2));
    assert_eq!(st.login_session(&idle.cookie).await.unwrap(), None);
    let fresh = st.create_login(LoginMethod::Link).await.unwrap();
    det.advance(Duration::from_secs(3600));
    let session = st.login_session(&fresh.cookie).await.unwrap().unwrap();
    assert!(st.login_alive(session.session_hash).await.unwrap());
}

#[tokio::test]
async fn a_new_login_expires_after_the_configured_longest_time() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| k.login_max_ms = 3 * DAY_MS));
    let now = st.now_ms();
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    assert_eq!(login.expires_at, now + 3 * DAY_MS);
}

#[tokio::test]
async fn a_lowered_login_max_deletes_the_older_session_and_revokes_its_sockets() {
    let det = Det::new();
    let st = store(&det).await;
    let old = knobs(|k| k.login_idle_ms = 30 * DAY_MS);
    st.set_knobs(old);
    let older = st.create_login(LoginMethod::Link).await.unwrap();
    let older_hash = secret_hash(older.cookie.expose());
    det.advance(days(10));
    let newer = st.create_login(LoginMethod::Link).await.unwrap();
    let ticket = st.registry().register(
        &older_hash,
        SessionKind::Owner,
        SocketKind::Board,
        "10.0.0.1",
    );
    let kept = st.registry().register(
        &secret_hash(newer.cookie.expose()),
        SessionKind::Owner,
        SocketKind::Board,
        "10.0.0.2",
    );
    let lowered = knobs(|k| {
        k.login_idle_ms = 30 * DAY_MS;
        k.login_max_ms = 5 * DAY_MS;
    });
    st.set_knobs(lowered);
    end_sessions_over_lowered(&st, &old, &lowered).await;
    assert!(ticket.is_revoked());
    assert!(!kept.is_revoked());
    assert_eq!(st.login_session(&older.cookie).await.unwrap(), None);
    assert!(
        !st.session_row_exists(SessionKind::Owner, &older_hash)
            .await
            .unwrap()
    );
    assert!(st.login_session(&newer.cookie).await.unwrap().is_some());
    // A raised limit does not bring the session back.
    st.set_knobs(old);
    end_sessions_over_lowered(&st, &lowered, &old).await;
    assert!(
        !st.session_row_exists(SessionKind::Owner, &older_hash)
            .await
            .unwrap()
    );
    assert_eq!(rows(&st, "login_sessions").await, 1);
}

#[tokio::test]
async fn a_lowered_login_idle_deletes_the_idle_session_only() {
    let det = Det::new();
    let st = store(&det).await;
    let idle = st.create_login(LoginMethod::Link).await.unwrap();
    det.advance(days(3));
    let busy = st.create_login(LoginMethod::Link).await.unwrap();
    let lowered = knobs(|k| k.login_idle_ms = 2 * DAY_MS);
    st.set_knobs(lowered);
    let ended = st
        .end_sessions_over_limits(true, false, st.now_ms())
        .await
        .unwrap();
    assert_eq!(
        ended,
        vec![(SessionKind::Owner, secret_hash(idle.cookie.expose()))]
    );
    assert_eq!(st.login_session(&idle.cookie).await.unwrap(), None);
    assert!(st.login_session(&busy.cookie).await.unwrap().is_some());
}

#[tokio::test]
async fn a_lowered_login_max_deletes_the_older_member_session() {
    let det = Det::new();
    let st = store(&det).await;
    let token = board_share(&st).await.token;
    let member = st.join(&token, "Ann").await.unwrap().unwrap();
    det.advance(days(10));
    let token = board_share(&st).await.token;
    let newer = st.join(&token, "Bob").await.unwrap().unwrap();
    let ticket = st.registry().register(
        &member.grant.session_hash,
        SessionKind::Member,
        SocketKind::Board,
        "10.0.0.3",
    );
    let old = DeckKnobs::default();
    let lowered = knobs(|k| k.login_max_ms = 5 * DAY_MS);
    st.set_knobs(lowered);
    end_sessions_over_lowered(&st, &old, &lowered).await;
    assert!(ticket.is_revoked());
    assert!(st.member_login(&member.cookie).await.unwrap().is_none());
    assert!(st.member_login(&newer.cookie).await.unwrap().is_some());
    assert_eq!(rows(&st, "member_sessions").await, 1);
}

#[tokio::test]
async fn a_new_member_session_lasts_no_longer_than_the_configured_login_max() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| k.login_max_ms = 2 * DAY_MS));
    let token = board_share(&st).await.token;
    let now = st.now_ms();
    let member = st.join(&token, "Ann").await.unwrap().unwrap();
    assert_eq!(member.grant.expires_at, now + 2 * DAY_MS);
}

async fn board_share(st: &DeckStore) -> crate::store::shares::CreatedShare {
    st.create_share(NewShareRow {
        kind: ShareKind::Board,
        target: String::new(),
        role: ShareRole::Viewer,
        steering: Steering::Open,
        expires_in_days: Some(30),
        uses: None,
        history: false,
    })
    .await
    .unwrap()
}

async fn station(st: &DeckStore, name: &str) -> StationId {
    st.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Read]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

async fn display_session(st: &DeckStore, station_name: &str) -> Secret<String> {
    let display = st
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![station(st, station_name).await],
        })
        .await
        .unwrap();
    st.create_display_session(display.display_id)
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn a_lowered_display_max_deletes_the_older_display_session() {
    let det = Det::new();
    let st = store(&det).await;
    let older = display_session(&st, "alpha").await;
    det.advance(days(10));
    let newer = display_session(&st, "beta").await;
    let old = DeckKnobs::default();
    let lowered = knobs(|k| k.display_max_ms = 5 * DAY_MS);
    st.set_knobs(lowered);
    end_sessions_over_lowered(&st, &old, &lowered).await;
    assert!(st.display_session(&older).await.unwrap().is_none());
    assert!(st.display_session(&newer).await.unwrap().is_some());
    assert_eq!(rows(&st, "display_sessions").await, 1);
    // Raising the limit again brings nothing back.
    st.set_knobs(old);
    end_sessions_over_lowered(&st, &lowered, &old).await;
    assert!(st.display_session(&older).await.unwrap().is_none());
}

#[tokio::test]
async fn the_display_idle_limit_and_the_new_session_expiry_are_the_configured_ones() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| {
        k.display_idle_ms = DAY_MS;
        k.display_max_ms = 4 * DAY_MS;
    }));
    let display = st
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![station(&st, "alpha").await],
        })
        .await
        .unwrap();
    let now = st.now_ms();
    let (cookie, expires_at) = st.create_display_session(display.display_id).await.unwrap();
    assert_eq!(expires_at, now + 4 * DAY_MS);
    det.advance(Duration::from_secs(3600));
    assert!(st.display_session(&cookie).await.unwrap().is_some());
    det.advance(days(2));
    assert!(st.display_session(&cookie).await.unwrap().is_none());
}

#[tokio::test]
async fn the_api_token_bound_and_its_text_follow_the_configured_days() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| k.api_token_max_ms = 10 * DAY_MS));
    let scopes = BTreeSet::from([Scope::Read]);
    let err = st
        .create_token("t".to_owned(), scopes.clone(), 10 * DAY_MS + 1)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DeckError::Invalid(m) if m == "a token lives for 1 ms to 10 days"),
        "{err:?}"
    );
    st.create_token("t".to_owned(), scopes.clone(), 10 * DAY_MS)
        .await
        .unwrap();
    // The default text names 90 days.
    st.set_knobs(DeckKnobs::default());
    let err = st
        .create_token("u".to_owned(), scopes, 91 * DAY_MS)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DeckError::Invalid(m) if m == "a token lives for 1 ms to 90 days"),
        "{err:?}"
    );
}

async fn add_audit(st: &DeckStore, n: usize) {
    for i in 0..n {
        st.audit(crate::store::NewAudit::new(
            "anonymous",
            format!("a{i}"),
            crate::store::AuditResult::Refused,
        ))
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn the_audit_prune_uses_the_configured_days_and_rows() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(knobs(|k| k.audit_retention_ms = 2 * DAY_MS));
    add_audit(&st, 4).await;
    det.advance(days(3));
    add_audit(&st, 6).await;
    assert_eq!(st.prune_audit().await.unwrap(), 4);
    assert_eq!(st.audit_log(100).await.unwrap().len(), 6);
    st.set_knobs(knobs(|k| k.audit_max_rows = 2));
    assert_eq!(st.prune_audit().await.unwrap(), 4);
    assert_eq!(st.audit_log(100).await.unwrap().len(), 2);
}
