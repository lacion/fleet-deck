#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::share::{ShareKind, ShareRole, Steering};
use fleetdeck_testkit::Det;

use super::{MemberAction, MemberId, ShareGrant, ShareScope, check_display_name, member_allows};
use crate::store::shares::NewShareRow;
use crate::store::{DeckError, DeckStore, Principal};

const DAY: u64 = 24 * 60 * 60 * 1000;

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn grant(role: ShareRole, scope: ShareScope) -> ShareGrant {
    ShareGrant {
        member_id: MemberId("m1".to_owned()),
        share_id: "sh1".to_owned(),
        display_name: "Ann".to_owned(),
        kind: ShareKind::Session,
        target: "st/s1".to_owned(),
        role,
        steering: Steering::Open,
        scope,
        session_hash: "h".to_owned(),
        csrf: "c".to_owned(),
        expires_at: i64::MAX,
    }
}

fn one(station: &str, session: &str) -> ShareScope {
    ShareScope::Sessions(BTreeSet::from([(station.to_owned(), session.to_owned())]))
}

fn board_share(role: ShareRole) -> NewShareRow {
    NewShareRow {
        kind: ShareKind::Board,
        target: String::new(),
        role,
        steering: Steering::Open,
        expires_in_days: None,
        uses: None,
    }
}

fn secret(s: &str) -> Secret<String> {
    Secret::new(s.to_owned())
}

#[test]
fn the_policy_table_gives_each_role_its_actions_on_covered_sessions_only() {
    use MemberAction::*;
    let all = [
        Read,
        Chat,
        Prompt,
        Steer,
        FollowUp,
        Interrupt,
        AnswerQuestion,
        AnswerApproval,
        Mail,
        Drive,
    ];
    // (role, the actions that it may do)
    let table: [(ShareRole, &[MemberAction]); 3] = [
        (ShareRole::Viewer, &[Read, Chat]),
        (
            ShareRole::Collaborator,
            &[
                Read,
                Chat,
                Prompt,
                Steer,
                FollowUp,
                Interrupt,
                AnswerQuestion,
                Mail,
                Drive,
            ],
        ),
        (ShareRole::Approver, &all),
    ];
    for (role, allowed) in table {
        for action in all {
            let g = grant(role, one("st", "s1"));
            assert_eq!(
                member_allows(&g, action, "st", "s1"),
                allowed.contains(&action),
                "{role:?} {action:?}"
            );
            // Another session, or another station: never.
            assert!(
                !member_allows(&g, action, "st", "s2"),
                "{role:?} {action:?}"
            );
            assert!(
                !member_allows(&g, action, "other", "s1"),
                "{role:?} {action:?}"
            );
            // A board share covers every session.
            let b = grant(role, ShareScope::Board);
            assert_eq!(
                member_allows(&b, action, "any", "any"),
                allowed.contains(&action),
                "{role:?} {action:?}"
            );
        }
    }
    // An empty space covers nothing.
    let empty = grant(ShareRole::Approver, ShareScope::Sessions(BTreeSet::new()));
    assert!(!member_allows(&empty, Read, "st", "s1"));
}

#[test]
fn the_actions_that_steer_are_the_ones_a_mob_driver_sends() {
    use MemberAction::*;
    for a in [Prompt, Steer, FollowUp, Interrupt, Mail] {
        assert!(a.steers(), "{a:?}");
    }
    for a in [Read, Chat, AnswerQuestion, AnswerApproval, Drive] {
        assert!(!a.steers(), "{a:?}");
    }
    assert_eq!(MemberAction::of_method("session.prompt"), Some(Prompt));
    assert_eq!(MemberAction::of_method("session.stop"), None);
    assert_eq!(MemberAction::of_method("approval.answer"), None);
}

#[test]
fn a_role_maps_to_api_scopes_and_the_actor_is_the_name() {
    let scopes = |r| grant(r, ShareScope::Board).scopes();
    assert_eq!(scopes(ShareRole::Viewer), BTreeSet::from([Scope::Read]));
    assert_eq!(
        scopes(ShareRole::Collaborator),
        BTreeSet::from([Scope::Read, Scope::Drive])
    );
    assert_eq!(
        scopes(ShareRole::Approver),
        BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve])
    );
    assert_eq!(
        grant(ShareRole::Viewer, ShareScope::Board).actor(),
        "member:Ann"
    );
    assert!(!scopes(ShareRole::Approver).contains(&Scope::Admin));
}

#[test]
fn display_names_are_trimmed_and_checked() {
    assert_eq!(check_display_name("  Ann Lee ").unwrap(), "Ann Lee");
    for bad in ["", "   ", "a\nb", "a\u{7}b", &"x".repeat(41)] {
        assert!(
            matches!(check_display_name(bad), Err(DeckError::Invalid(_))),
            "{bad:?}"
        );
    }
    assert!(check_display_name(&"x".repeat(40)).is_ok());
}

#[tokio::test]
async fn an_invite_makes_a_member_login_once_and_stores_only_hashes() {
    let det = Det::new();
    let st = store(&det).await;
    let created = st
        .create_share(board_share(ShareRole::Collaborator))
        .await
        .unwrap();
    let token = created.token.expose().clone();
    let login = st.join(&secret(&token), " Ann ").await.unwrap().unwrap();
    assert_eq!(login.grant.display_name, "Ann");
    assert_eq!(login.grant.role, ShareRole::Collaborator);
    assert_eq!(login.grant.scope, ShareScope::Board);
    assert_eq!(login.grant.share_id, created.share_id);
    // The cookie finds the login; the stored value is a hash.
    let found = st.member_login(&login.cookie).await.unwrap().unwrap();
    assert_eq!(found.member_id, login.grant.member_id);
    assert_eq!(found.csrf, login.grant.csrf);
    let (cookie, csrf) = (login.cookie.expose().clone(), login.grant.csrf.clone());
    let stored: String = st
        .db()
        .call(|c| {
            Ok(c.query_row(
                "SELECT group_concat(session_hash || csrf_token) FROM member_sessions",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(!stored.contains(&cookie), "the cookie is stored as a hash");
    assert!(stored.contains(&csrf));
    let invites: String = st
        .db()
        .call(|c| {
            Ok(
                c.query_row("SELECT group_concat(token_hash) FROM invites", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert!(!invites.contains(&token));
    // The single use is gone.
    assert!(st.join(&secret(&token), "Bob").await.unwrap().is_none());
    // A wrong token is nothing.
    assert!(
        st.join(&secret("fdjoin1_nope"), "Bob")
            .await
            .unwrap()
            .is_none()
    );
    // The principal of the login is alive until the login ends.
    let principal = Principal::Member(found.member_id.clone(), Box::new(found.clone()));
    assert!(st.principal_alive(&principal).await.unwrap());
    assert!(
        st.end_member_login(found.session_hash.clone())
            .await
            .unwrap()
    );
    assert!(!st.principal_alive(&principal).await.unwrap());
    assert!(st.member_login(&login.cookie).await.unwrap().is_none());
}

#[tokio::test]
async fn the_same_name_in_a_share_is_the_same_member() {
    let det = Det::new();
    let st = store(&det).await;
    let mut row = board_share(ShareRole::Viewer);
    row.uses = Some(3);
    let created = st.create_share(row).await.unwrap();
    let t = secret(created.token.expose());
    let a = st.join(&t, "Ann").await.unwrap().unwrap();
    let a2 = st.join(&t, "ann").await.unwrap().unwrap();
    let b = st.join(&t, "Bob").await.unwrap().unwrap();
    assert_eq!(a.grant.member_id, a2.grant.member_id);
    assert_ne!(a.grant.member_id, b.grant.member_id);
    assert_ne!(a.grant.session_hash, a2.grant.session_hash);
    let shares = st.shares().await.unwrap();
    assert_eq!(shares[0].members, ["Ann", "Bob"]);
    assert_eq!(shares[0].uses_left, 0);
    // The actor `member:<name>` authorizes command reads and the mob driver,
    // so another share cannot take the name, and the refusal keeps the invite.
    let other = st
        .create_share(board_share(ShareRole::Viewer))
        .await
        .unwrap();
    let t2 = secret(other.token.expose());
    assert!(matches!(
        st.join(&t2, "ANN").await,
        Err(DeckError::NameTaken(_))
    ));
    let c = st.join(&t2, "Cy").await.unwrap().unwrap();
    assert_ne!(c.grant.member_id, a.grant.member_id);
}

#[tokio::test]
async fn a_bad_name_does_not_use_up_the_invite() {
    let det = Det::new();
    let st = store(&det).await;
    let created = st
        .create_share(board_share(ShareRole::Viewer))
        .await
        .unwrap();
    let t = secret(created.token.expose());
    assert!(matches!(st.join(&t, " ").await, Err(DeckError::Invalid(_))));
    assert!(st.join(&t, "Ann").await.unwrap().is_some());
}

#[tokio::test]
async fn a_revoke_or_the_expiry_of_the_share_ends_every_login() {
    let det = Det::new();
    let st = store(&det).await;
    let mut row = board_share(ShareRole::Viewer);
    row.uses = Some(2);
    row.expires_in_days = Some(2);
    let created = st.create_share(row).await.unwrap();
    let t = secret(created.token.expose());
    let a = st.join(&t, "Ann").await.unwrap().unwrap();
    let b = st.join(&t, "Bob").await.unwrap().unwrap();
    // A login never outlives its share.
    assert_eq!(a.grant.expires_at, created.expires_at);
    let view = st
        .revoke_share(created.share_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(view.revoked_at.is_some());
    assert_eq!(view.uses_left, 0);
    assert!(st.member_login(&a.cookie).await.unwrap().is_none());
    assert!(st.member_login(&b.cookie).await.unwrap().is_none());
    assert!(st.revoke_share("ghost".to_owned()).await.unwrap().is_none());
    // A second revoke changes nothing.
    let again = st
        .revoke_share(created.share_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.revoked_at, view.revoked_at);

    let fresh = st
        .create_share(board_share(ShareRole::Viewer))
        .await
        .unwrap();
    let c = st
        .join(&secret(fresh.token.expose()), "Cy")
        .await
        .unwrap()
        .unwrap();
    assert!(st.member_login(&c.cookie).await.unwrap().is_some());
    det.advance(Duration::from_millis(8 * DAY));
    assert!(st.member_login(&c.cookie).await.unwrap().is_none());
    assert!(
        st.join(&secret(fresh.token.expose()), "Di")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_space_share_covers_the_sessions_of_the_space_now() {
    let det = Det::new();
    let st = store(&det).await;
    let space = st.create_space("auth refactor").await.unwrap();
    let row = NewShareRow {
        kind: ShareKind::Space,
        target: space.space_id.clone(),
        role: ShareRole::Viewer,
        steering: Steering::Open,
        expires_in_days: None,
        uses: None,
    };
    let created = st.create_share(row).await.unwrap();
    let login = st
        .join(&secret(created.token.expose()), "Ann")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(login.grant.scope, ShareScope::Sessions(BTreeSet::new()));
    st.add_space_session(space.space_id.clone(), "st".into(), "s1".into())
        .await
        .unwrap()
        .unwrap();
    let fresh = st.member_refresh(&login.grant).await.unwrap().unwrap();
    assert!(fresh.scope.covers("st", "s1"));
    assert!(!fresh.scope.covers("st", "s2"));
    assert!(
        st.add_space_session("ghost".to_owned(), "st".into(), "s1".into())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_share_has_a_bounded_lifetime_and_use_count() {
    let det = Det::new();
    let st = store(&det).await;
    for (days, uses) in [
        (Some(0), None),
        (Some(91), None),
        (None, Some(0)),
        (None, Some(101)),
    ] {
        let mut row = board_share(ShareRole::Viewer);
        row.expires_in_days = days;
        row.uses = uses;
        assert!(
            matches!(st.create_share(row).await, Err(DeckError::Invalid(_))),
            "{days:?} {uses:?}"
        );
    }
    let mut row = board_share(ShareRole::Viewer);
    row.expires_in_days = Some(90);
    row.uses = Some(100);
    assert!(st.create_share(row).await.is_ok());
}
