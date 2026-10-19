//! The idle deadlines survive a limit that shrinks and grows again (the clamp
//! of `idle_until`), a new session is clamped by the newest knobs, and the
//! check of a socket applies the full session validity (knobs PRE-PR r2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::share::{ShareKind, ShareRole, Steering};
use fleetdeck_testkit::Det;

use super::{DeckStore, call};
use crate::board_fence::registry::{SessionKind, SocketKind};
use crate::knobs::{DeckKnobs, clamp_after_publish};
use crate::store::LoginMethod;
use crate::store::auth::secret_hash;
use crate::store::displays::NewDisplay;
use crate::store::displays_auth::issue_fenced;
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

async fn display(st: &DeckStore, station_name: &str) -> String {
    st.create_display(NewDisplay {
        name: "Wall".to_owned(),
        ceiling: vec![station(st, station_name).await],
    })
    .await
    .unwrap()
    .display_id
}

async fn display_cookie(st: &DeckStore, station_name: &str) -> Secret<String> {
    let id = display(st, station_name).await;
    st.create_display_session(id).await.unwrap().0
}

async fn idle_until(st: &DeckStore, table: &'static str, hash: String) -> i64 {
    call(&st.inner.db, move |conn| {
        Ok(conn.query_row(
            &format!("SELECT idle_until FROM {table} WHERE session_hash = ?1"),
            [hash],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

async fn rows(st: &DeckStore, table: &'static str) -> i64 {
    call(&st.inner.db, move |conn| {
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_raised_idle_limit_does_not_revive_a_session_that_idled_past_the_short_one() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let wall = display_cookie(&st, "alpha").await;
    // The limits shrink while both sessions are fresh: nothing is deleted.
    st.set_knobs(knobs(|k| {
        k.login_idle_ms = DAY_MS;
        k.display_idle_ms = DAY_MS;
    }));
    st.clamp_idle_deadlines(DAY_MS, DAY_MS).await.unwrap();
    assert_eq!(rows(&st, "login_sessions").await, 1);
    assert_eq!(rows(&st, "display_sessions").await, 1);
    // Both idle for 2 days, then the limits grow again.
    det.advance(days(2));
    st.set_knobs(DeckKnobs::default());
    assert_eq!(st.login_session(&login.cookie).await.unwrap(), None);
    assert!(st.display_session(&wall).await.unwrap().is_none());
}

#[tokio::test]
async fn a_session_used_within_the_short_limit_keeps_working_after_the_limit_grows() {
    let det = Det::new();
    let st = store(&det).await;
    let idle = st.create_login(LoginMethod::Link).await.unwrap();
    let busy = st.create_login(LoginMethod::Link).await.unwrap();
    st.set_knobs(knobs(|k| k.login_idle_ms = DAY_MS));
    st.clamp_idle_deadlines(DAY_MS, 30 * DAY_MS).await.unwrap();
    det.advance(Duration::from_secs(12 * 3600));
    assert!(st.login_session(&busy.cookie).await.unwrap().is_some());
    det.advance(Duration::from_secs(18 * 3600));
    st.set_knobs(DeckKnobs::default());
    assert_eq!(st.login_session(&idle.cookie).await.unwrap(), None);
    assert!(st.login_session(&busy.cookie).await.unwrap().is_some());
}

#[tokio::test]
async fn the_clamp_only_shortens_a_deadline() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let hash = secret_hash(login.cookie.expose());
    let issued = idle_until(&st, "login_sessions", hash.clone()).await;
    assert_eq!(issued, st.now_ms() + 7 * DAY_MS);
    // A longer limit leaves the deadline alone.
    st.clamp_idle_deadlines(30 * DAY_MS, 30 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(
        idle_until(&st, "login_sessions", hash.clone()).await,
        issued
    );
    st.clamp_idle_deadlines(2 * DAY_MS, 30 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(
        idle_until(&st, "login_sessions", hash).await,
        st.now_ms() + 2 * DAY_MS
    );
}

#[tokio::test]
async fn a_session_issued_with_the_old_knobs_is_clamped_after_the_publish() {
    let det = Det::new();
    let st = store(&det).await;
    // The sessions read the old knobs; the publish comes after the insert.
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let wall = display_cookie(&st, "alpha").await;
    let lowered = knobs(|k| {
        k.login_idle_ms = DAY_MS;
        k.display_idle_ms = DAY_MS;
    });
    st.set_knobs(lowered);
    clamp_after_publish(&st, &lowered).await;
    det.advance(days(2));
    st.set_knobs(DeckKnobs::default());
    assert_eq!(st.login_session(&login.cookie).await.unwrap(), None);
    assert!(st.display_session(&wall).await.unwrap().is_none());
}

#[tokio::test]
async fn a_new_login_row_takes_the_newest_idle_limit() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let hash = secret_hash(login.cookie.expose());
    // The limit that the helper gets is newer than the one of the insert.
    let row = hash.clone();
    call(&st.inner.db, move |conn| {
        super::clamp_session_row(conn, "login_sessions", &row, DAY_MS)
    })
    .await
    .unwrap();
    assert_eq!(
        idle_until(&st, "login_sessions", hash).await,
        st.now_ms() + DAY_MS
    );
}

#[tokio::test]
async fn a_new_display_session_takes_the_newest_idle_limit() {
    let det = Det::new();
    let st = store(&det).await;
    let id = display(&st, "alpha").await;
    let old = DeckKnobs::default();
    let newest = knobs(|k| k.display_idle_ms = DAY_MS);
    // The caller read the old knobs; a publish ran before the insert ended.
    let (row, expires) = call(&st.inner.db, move |conn| {
        let tx = conn.transaction()?;
        let issued = issue_fenced(&tx, &id, 0, 5, &old, &|| newest)?.unwrap();
        tx.commit()?;
        Ok((secret_hash(issued.cookie.expose()), issued.expires_at))
    })
    .await
    .unwrap();
    assert_eq!(expires, 5 + old.display_max_ms);
    assert_eq!(idle_until(&st, "display_sessions", row).await, 5 + DAY_MS);
}

#[tokio::test]
async fn the_session_check_of_a_socket_refuses_an_owner_over_a_lowered_maximum() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let hash = secret_hash(login.cookie.expose());
    let now = st.now_ms();
    assert!(
        st.session_valid(SessionKind::Owner, &hash, now)
            .await
            .unwrap()
    );
    det.advance(days(3));
    // The clean-up did not run: the check enforces the maximum itself.
    st.set_knobs(knobs(|k| k.login_max_ms = 2 * DAY_MS));
    assert!(
        !st.session_valid(SessionKind::Owner, &hash, st.now_ms())
            .await
            .unwrap()
    );
    assert_eq!(rows(&st, "login_sessions").await, 1);
}

#[tokio::test]
async fn the_session_check_of_a_socket_refuses_an_idle_owner() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let hash = secret_hash(login.cookie.expose());
    det.advance(days(2));
    assert!(
        st.session_valid(SessionKind::Owner, &hash, st.now_ms())
            .await
            .unwrap()
    );
    st.set_knobs(knobs(|k| k.login_idle_ms = DAY_MS));
    assert!(
        !st.session_valid(SessionKind::Owner, &hash, st.now_ms())
            .await
            .unwrap()
    );
    assert!(
        !st.session_valid(SessionKind::Owner, "missing", st.now_ms())
            .await
            .unwrap()
    );
    // The deadline alone ends it too, after the limit grew again.
    st.set_knobs(DeckKnobs::default());
    st.clamp_idle_deadlines(DAY_MS, DAY_MS).await.unwrap();
    assert!(
        !st.session_valid(SessionKind::Owner, &hash, st.now_ms())
            .await
            .unwrap()
    );
    // The check does not delete the row.
    assert_eq!(rows(&st, "login_sessions").await, 1);
}

#[tokio::test]
async fn the_session_check_of_a_socket_refuses_a_member_over_a_lowered_maximum() {
    let det = Det::new();
    let st = store(&det).await;
    let share = st
        .create_share(NewShareRow {
            kind: ShareKind::Board,
            target: String::new(),
            role: ShareRole::Viewer,
            steering: Steering::Open,
            expires_in_days: Some(30),
            uses: None,
            history: false,
        })
        .await
        .unwrap();
    let member = st.join(&share.token, "Ann").await.unwrap().unwrap();
    let hash = member.grant.session_hash.clone();
    assert!(
        st.session_valid(SessionKind::Member, &hash, st.now_ms())
            .await
            .unwrap()
    );
    det.advance(days(3));
    st.set_knobs(knobs(|k| k.login_max_ms = 2 * DAY_MS));
    assert!(
        !st.session_valid(SessionKind::Member, &hash, st.now_ms())
            .await
            .unwrap()
    );
    assert_eq!(rows(&st, "member_sessions").await, 1);
}

#[tokio::test]
async fn a_socket_of_a_session_over_a_lowered_limit_is_not_admitted_with_the_cleanup_skipped() {
    let det = Det::new();
    let st = store(&det).await;
    let old = st.create_login(LoginMethod::Link).await.unwrap();
    let old_hash = secret_hash(old.cookie.expose());
    det.advance(days(3));
    let fresh = st.create_login(LoginMethod::Link).await.unwrap();
    let fresh_hash = secret_hash(fresh.cookie.expose());
    // Both sockets passed the sign-in check; then the maximum shrank and the
    // clean-up never ran.
    let late = st
        .registry()
        .register(&old_hash, SessionKind::Owner, SocketKind::Board, "10.0.0.1");
    let fine = st.registry().register(
        &fresh_hash,
        SessionKind::Owner,
        SocketKind::Board,
        "10.0.0.2",
    );
    st.set_knobs(knobs(|k| k.login_max_ms = 2 * DAY_MS));
    assert!(!crate::board_fence::admit_socket(&st, &late).await);
    assert!(crate::board_fence::admit_socket(&st, &fine).await);
}
