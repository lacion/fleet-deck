#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_proto::link::Scope;
use fleetdeck_testkit::Det;

use super::{LOGIN_LINK_TTL_MS, LoginMethod, secret_hash};
use crate::knobs::DeckKnobs;
use crate::store::audit::{AuditResult, NewAudit};
use crate::store::{DeckError, DeckStore};

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn s(v: &str) -> Secret<String> {
    Secret::new(v.to_owned())
}

fn idle_ms() -> i64 {
    DeckKnobs::default().login_idle_ms
}

fn ms(v: i64) -> Duration {
    Duration::from_millis(u64::try_from(v).unwrap())
}

#[tokio::test]
async fn a_password_is_stored_as_argon2id_checked_and_a_new_one_ends_every_login() {
    let det = Det::new();
    let st = store(&det).await;
    assert!(!st.password_set().await.unwrap());
    assert!(!st.check_password(s("anything at all")).await.unwrap());
    assert!(matches!(
        st.set_password(s("short")).await,
        Err(DeckError::Invalid(_))
    ));

    st.set_password(s("correct horse battery")).await.unwrap();
    let phc: String = st
        .db()
        .call(|c| {
            c.query_row("SELECT password_hash FROM owner", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .await
        .unwrap();
    assert!(phc.starts_with("$argon2id$"), "{phc}");
    assert!(st.check_password(s("correct horse battery")).await.unwrap());
    assert!(
        !st.check_password(s("correct horse battery!"))
            .await
            .unwrap()
    );

    let login = st.create_login(LoginMethod::Password).await.unwrap();
    assert!(st.login_session(&login.cookie).await.unwrap().is_some());
    st.set_password(s("another long password")).await.unwrap();
    assert_eq!(st.login_session(&login.cookie).await.unwrap(), None);
}

#[tokio::test]
async fn a_login_session_stores_only_a_hash_and_ends_when_idle_or_too_old() {
    let det = Det::new();
    let st = store(&det).await;
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let stored: String = st
        .db()
        .call(|c| {
            c.query_row("SELECT session_hash FROM login_sessions", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(stored, secret_hash(login.cookie.expose()));
    assert_ne!(&stored, login.cookie.expose());
    let session = st.login_session(&login.cookie).await.unwrap().unwrap();
    assert_eq!(session.csrf, login.csrf);
    assert_eq!(st.login_session(&s("0000")).await.unwrap(), None);

    // Each use refreshes the idle time; 7 idle days end the session.
    det.advance(ms(idle_ms() - 1000));
    assert!(st.login_session(&login.cookie).await.unwrap().is_some());
    det.advance(ms(idle_ms() - 1000));
    assert!(st.login_session(&login.cookie).await.unwrap().is_some());
    det.advance(ms(idle_ms()));
    assert_eq!(st.login_session(&login.cookie).await.unwrap(), None);

    // 30 days after the login the session ends, also when in use.
    let login = st.create_login(LoginMethod::Link).await.unwrap();
    for _ in 0..4 {
        det.advance(ms(idle_ms() - 1000));
        assert!(st.login_session(&login.cookie).await.unwrap().is_some());
    }
    // Day 28 + 3 idle days: past the 30-day limit, not idle long enough.
    det.advance(ms(3 * 24 * 60 * 60 * 1000));
    assert_eq!(st.login_session(&login.cookie).await.unwrap(), None);

    let login = st.create_login(LoginMethod::Link).await.unwrap();
    let session = st.login_session(&login.cookie).await.unwrap().unwrap();
    assert!(st.end_login(session.session_hash).await.unwrap());
    assert_eq!(st.login_session(&login.cookie).await.unwrap(), None);
}

#[tokio::test]
async fn a_login_link_works_once_and_expires_after_five_minutes() {
    let det = Det::new();
    let st = store(&det).await;
    let link = st.create_login_link().await.unwrap();
    assert!(link.expose().starts_with("fdlogin1_"));
    assert!(st.redeem_login_link(&link).await.unwrap());
    assert!(!st.redeem_login_link(&link).await.unwrap());

    let link = st.create_login_link().await.unwrap();
    det.advance(ms(LOGIN_LINK_TTL_MS));
    assert!(!st.redeem_login_link(&link).await.unwrap());
}

#[tokio::test]
async fn an_api_token_has_a_scope_set_an_expiry_and_can_be_revoked() {
    let det = Det::new();
    let st = store(&det).await;
    let read = BTreeSet::from([Scope::Read]);
    let day = 24 * 60 * 60 * 1000;
    for (name, scopes, ttl) in [
        ("", read.clone(), day),
        ("bad name", read.clone(), day),
        ("t", BTreeSet::new(), day),
        ("t", BTreeSet::from([Scope::Coordinate]), day),
        ("t", read.clone(), 91 * day),
    ] {
        let r = st.create_token(name.to_owned(), scopes, ttl).await;
        assert!(matches!(r, Err(DeckError::Invalid(_))), "{name}");
    }
    let (token, value) = st
        .create_token("phone".to_owned(), read.clone(), day)
        .await
        .unwrap();
    assert!(value.expose().starts_with("fdtok1_"));
    assert!(matches!(
        st.create_token("phone".to_owned(), read.clone(), day).await,
        Err(DeckError::Invalid(_))
    ));
    let found = st.token(&value).await.unwrap().unwrap();
    assert_eq!((found.name.as_str(), &found.scopes), ("phone", &read));
    assert_eq!(found.last_used, Some(token.created_at));
    assert_eq!(st.token(&s("fdtok1_00")).await.unwrap(), None);
    assert_eq!(st.tokens().await.unwrap().len(), 1);

    det.advance(ms(day));
    assert_eq!(st.token(&value).await.unwrap(), None);
    assert!(st.revoke_token("phone".to_owned()).await.unwrap());
    assert!(st.tokens().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_audit_log_keeps_rows_newest_first() {
    let det = Det::new();
    let st = store(&det).await;
    assert!(!st.owner_id().await.unwrap().is_empty());
    st.audit(NewAudit::new("anonymous", "auth.login", AuditResult::Refused).detail("bad password"))
        .await
        .unwrap();
    st.audit(
        NewAudit::new("owner", "commands.submit", AuditResult::Ok)
            .target("alpha")
            .command("c1".into()),
    )
    .await
    .unwrap();
    let log = st.audit_log(10).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].action, "commands.submit");
    assert_eq!(log[0].command_id.as_ref().unwrap().as_str(), "c1");
    assert_eq!(log[1].result, AuditResult::Refused);
    assert_eq!(st.audit_log(1).await.unwrap().len(), 1);
}
