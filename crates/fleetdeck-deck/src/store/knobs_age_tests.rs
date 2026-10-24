//! Knobs pre-PR round 1: the sign-in checks enforce the current lifetimes
//! themselves (a lowered longest time refuses an older session at once,
//! whether the cleanup ran or not, also after a restart), and an idle
//! deadline stops a raised idle limit from reviving an expired session.

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
use super::LoginMethod;
use super::auth::{LoginSession, Principal};
use super::displays::{DisplayLogin, NewDisplay};
use super::members::NewMemberLogin;
use super::shares::NewShareRow;
use super::stations::NewStation;
use crate::knobs::DeckKnobs;

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

async fn collaborator_share(st: &DeckStore) -> Secret<String> {
    st.create_share(NewShareRow {
        kind: ShareKind::Board,
        target: String::new(),
        role: ShareRole::Collaborator,
        steering: Steering::Open,
        expires_in_days: Some(30),
        uses: None,
        history: false,
    })
    .await
    .unwrap()
    .token
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

async fn display_cookie(st: &DeckStore, station_name: &str) -> Secret<String> {
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

/// One session of each kind, each used once so that the values are those of a
/// session in use.
struct Sessions {
    owner_cookie: Secret<String>,
    owner: LoginSession,
    member: NewMemberLogin,
    display_cookie: Secret<String>,
    display: DisplayLogin,
}

async fn open_sessions(st: &DeckStore, station_name: &str, member_name: &str) -> Sessions {
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let owner = st.login_session(&login.cookie).await.unwrap().unwrap();
    let token = collaborator_share(st).await;
    let member = st.join(&token, member_name).await.unwrap().unwrap();
    let display_cookie = display_cookie(st, station_name).await;
    let display = st.display_session(&display_cookie).await.unwrap().unwrap();
    Sessions {
        owner_cookie: login.cookie,
        owner,
        member,
        display_cookie,
        display,
    }
}

/// Every sign-in check of the three kinds of session says "no".
async fn assert_refused(st: &DeckStore, s: &Sessions) {
    assert_eq!(st.login_session(&s.owner_cookie).await.unwrap(), None);
    assert!(!st.login_alive(s.owner.session_hash.clone()).await.unwrap());
    assert!(
        !st.principal_alive(&Principal::Owner(s.owner.clone()))
            .await
            .unwrap()
    );
    assert!(st.member_login(&s.member.cookie).await.unwrap().is_none());
    assert!(st.member_refresh(&s.member.grant).await.unwrap().is_none());
    assert!(
        !st.principal_alive(&Principal::Member(
            s.member.grant.member_id.clone(),
            Box::new(s.member.grant.clone())
        ))
        .await
        .unwrap()
    );
    assert!(
        !st.member_can_drive("st".to_owned(), "se".to_owned(), "Ann".to_owned())
            .await
            .unwrap()
    );
    assert!(
        st.display_session(&s.display_cookie)
            .await
            .unwrap()
            .is_none()
    );
    assert!(st.display_refresh(&s.display).await.unwrap().is_none());
    assert!(
        !st.display_session_alive(s.display.session_hash.clone())
            .await
            .unwrap()
    );
    assert!(
        !st.principal_alive(&Principal::Display(s.display.clone()))
            .await
            .unwrap()
    );
}

async fn assert_alive(st: &DeckStore, s: &Sessions) {
    assert!(st.login_session(&s.owner_cookie).await.unwrap().is_some());
    assert!(st.member_login(&s.member.cookie).await.unwrap().is_some());
    assert!(
        st.display_session(&s.display_cookie)
            .await
            .unwrap()
            .is_some()
    );
}

/// The knobs of a deck whose sessions live long enough for the tests.
fn long_lived() -> DeckKnobs {
    knobs(|k| k.login_idle_ms = 30 * DAY_MS)
}

/// A lowered longest time of 5 days.
fn lowered() -> DeckKnobs {
    knobs(|k| {
        k.login_idle_ms = 30 * DAY_MS;
        k.login_max_ms = 5 * DAY_MS;
        k.display_max_ms = 5 * DAY_MS;
    })
}

async fn count(st: &DeckStore, table: &'static str) -> i64 {
    st.db()
        .call(move |c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_lowered_longest_time_refuses_an_older_session_when_no_deletion_ran() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(long_lived());
    let older = open_sessions(&st, "alpha", "Ann").await;
    det.advance(days(10));
    // The settings publish without the cleanup (the deletion failed or never
    // ran): the rows are still there.
    st.set_knobs(lowered());
    assert_eq!(count(&st, "login_sessions").await, 1);
    assert_eq!(count(&st, "member_sessions").await, 1);
    assert_eq!(count(&st, "display_sessions").await, 1);
    assert_refused(&st, &older).await;
    // A session that starts after the change is alive.
    let newer = open_sessions(&st, "beta", "Bob").await;
    assert_alive(&st, &newer).await;
}

#[tokio::test]
async fn a_lowered_longest_time_still_refuses_an_older_session_after_a_restart() {
    let det = Det::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let st = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    st.set_knobs(long_lived());
    let older = open_sessions(&st, "alpha", "Ann").await;
    det.advance(days(10));
    drop(st);
    // The deck starts again with the lowered knobs of its config and no
    // cleanup.
    let st = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    st.set_knobs(lowered());
    assert_eq!(count(&st, "login_sessions").await, 1);
    assert_refused(&st, &older).await;
}

fn idle_one_day() -> DeckKnobs {
    knobs(|k| {
        k.login_idle_ms = DAY_MS;
        k.display_idle_ms = DAY_MS;
    })
}

fn idle_thirty_days() -> DeckKnobs {
    knobs(|k| {
        k.login_idle_ms = 30 * DAY_MS;
        k.display_idle_ms = 30 * DAY_MS;
    })
}

#[tokio::test]
async fn raising_an_idle_limit_does_not_revive_an_idle_expired_session() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(idle_one_day());
    let owner_login = st.create_login(LoginMethod::Link).await.unwrap();
    let owner_hash = super::auth::secret_hash(owner_login.cookie.expose());
    let display = display_cookie(&st, "alpha").await;
    let display_login = st.display_session(&display).await.unwrap().unwrap();
    det.advance(days(2));
    // The owner session fails a check before the raise; the display session
    // is not checked at all before the raise.
    assert_eq!(st.login_session(&owner_login.cookie).await.unwrap(), None);
    st.set_knobs(idle_thirty_days());
    assert_eq!(st.login_session(&owner_login.cookie).await.unwrap(), None);
    assert!(!st.login_alive(owner_hash).await.unwrap());
    assert!(st.display_session(&display).await.unwrap().is_none());
    assert!(
        !st.display_session_alive(display_login.session_hash)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_session_used_within_the_old_idle_limit_keeps_working_after_a_raise() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(idle_one_day());
    let owner = st.create_login(LoginMethod::Link).await.unwrap();
    let display = display_cookie(&st, "alpha").await;
    det.advance(Duration::from_secs(12 * 3600));
    assert!(st.login_session(&owner.cookie).await.unwrap().is_some());
    assert!(st.display_session(&display).await.unwrap().is_some());
    st.set_knobs(idle_thirty_days());
    // 18 hours later: inside the deadline that the last use set.
    det.advance(Duration::from_secs(18 * 3600));
    assert!(st.login_session(&owner.cookie).await.unwrap().is_some());
    assert!(st.display_session(&display).await.unwrap().is_some());
    // That use gave the longer window: 20 idle days are alive.
    det.advance(days(20));
    assert!(st.login_session(&owner.cookie).await.unwrap().is_some());
    assert!(st.display_session(&display).await.unwrap().is_some());
}

async fn session_times(st: &DeckStore, table: &'static str) -> (i64, i64) {
    st.db()
        .call(move |c| {
            Ok(c.query_row(
                &format!("SELECT last_seen, idle_until FROM {table}"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_session_gets_the_idle_deadline_and_a_failed_check_does_not_move_it() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(idle_one_day());
    let start = st.now_ms();
    let owner = st.create_login(LoginMethod::Link).await.unwrap();
    let display = display_cookie(&st, "alpha").await;
    assert_eq!(
        session_times(&st, "login_sessions").await,
        (start, start + DAY_MS)
    );
    assert_eq!(
        session_times(&st, "display_sessions").await,
        (start, start + DAY_MS)
    );
    // A successful use moves both.
    det.advance(Duration::from_secs(3600));
    let used = st.now_ms();
    assert!(st.login_session(&owner.cookie).await.unwrap().is_some());
    assert!(st.display_session(&display).await.unwrap().is_some());
    assert_eq!(
        session_times(&st, "login_sessions").await,
        (used, used + DAY_MS)
    );
    assert_eq!(
        session_times(&st, "display_sessions").await,
        (used, used + DAY_MS)
    );
    // A failed check (idle) moves nothing.
    det.advance(days(2));
    assert_eq!(st.login_session(&owner.cookie).await.unwrap(), None);
    assert!(st.display_session(&display).await.unwrap().is_none());
    assert_eq!(
        session_times(&st, "login_sessions").await,
        (used, used + DAY_MS)
    );
    assert_eq!(
        session_times(&st, "display_sessions").await,
        (used, used + DAY_MS)
    );
}

#[tokio::test]
async fn a_failed_age_check_does_not_move_the_idle_times() {
    let det = Det::new();
    let st = store(&det).await;
    st.set_knobs(long_lived());
    let owner = st.create_login(LoginMethod::Link).await.unwrap();
    let display = display_cookie(&st, "alpha").await;
    let start = st.now_ms();
    det.advance(days(10));
    st.set_knobs(lowered());
    assert_eq!(st.login_session(&owner.cookie).await.unwrap(), None);
    assert!(st.display_session(&display).await.unwrap().is_none());
    assert_eq!(
        session_times(&st, "login_sessions").await,
        (start, start + 30 * DAY_MS)
    );
    assert_eq!(
        session_times(&st, "display_sessions").await,
        (start, start + 30 * DAY_MS)
    );
}
