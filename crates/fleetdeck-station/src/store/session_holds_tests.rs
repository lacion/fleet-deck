//! The session holds (design §11.4, V30).

use fleetdeck_proto::{RejectCode, SessionId};
use fleetdeck_testkit::Det;

use super::{HELD, KIND_LAND, KIND_MOVE, check_free, delete, held, insert};
use crate::store::git_state::tests::{new_session, store};
use crate::store::{DecideError, call};

#[tokio::test]
async fn a_held_session_is_refused_and_a_free_one_passes() {
    let det = Det::new();
    let store = store(&det).await;
    store.create_session(new_session("s1")).await.unwrap();
    store.create_session(new_session("s2")).await.unwrap();
    let s1 = SessionId::from("s1");
    let s2 = SessionId::from("s2");
    call(&store.inner.db, {
        let s1 = s1.clone();
        move |conn| insert(conn, &s1, KIND_LAND, "land-1", 50)
    })
    .await
    .unwrap();
    let (free, refused) = call(&store.inner.db, {
        let (s1, s2) = (s1.clone(), s2.clone());
        move |conn| Ok((check_free(conn, &s2).is_ok(), check_free(conn, &s1)))
    })
    .await
    .unwrap();
    assert!(free);
    let Err(DecideError::Reject(reject)) = refused else {
        panic!("not refused");
    };
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert_eq!(reject.message, HELD);
    let hold = store.session_hold(s1.clone()).await.unwrap().unwrap();
    assert_eq!(hold.kind, "land");
    assert_eq!(hold.holder_id, "land-1");
    assert_eq!(hold.created_at, 50);
    assert_eq!(store.session_hold(s2).await.unwrap(), None);
}

#[tokio::test]
async fn a_session_has_one_hold_and_only_its_holder_deletes_it() {
    let det = Det::new();
    let store = store(&det).await;
    store.create_session(new_session("s1")).await.unwrap();
    let s1 = SessionId::from("s1");
    let outcome = call(&store.inner.db, move |conn| {
        insert(conn, &s1, KIND_LAND, "land-1", 1)?;
        let second = insert(conn, &s1, KIND_MOVE, "move-1", 2).is_err();
        let other = delete(conn, &s1, "move-1")?;
        let still = held(conn, &s1)?;
        let own = delete(conn, &s1, "land-1")?;
        let gone = !held(conn, &s1)?;
        Ok((second, other, still, own, gone))
    })
    .await
    .unwrap();
    assert_eq!(outcome, (true, false, true, true, true));
}

#[tokio::test]
async fn a_hold_needs_a_session_and_a_known_kind() {
    let det = Det::new();
    let store = store(&det).await;
    store.create_session(new_session("s1")).await.unwrap();
    let (no_session, bad_kind) = call(&store.inner.db, |conn| {
        let ghost = SessionId::from("ghost");
        let real = SessionId::from("s1");
        Ok((
            insert(conn, &ghost, KIND_LAND, "land-1", 1).is_err(),
            insert(conn, &real, "other", "x", 1).is_err(),
        ))
    })
    .await
    .unwrap();
    assert!(no_session && bad_kind);
}
