//! The session holds (design §11.4, V30).

use fleetdeck_proto::{RejectCode, SessionId};
use fleetdeck_testkit::Det;

use super::{
    HELD, HELD_MOVE, HELD_MOVE_IN, KIND_LAND, KIND_MOVE, KIND_MOVE_IN, check_free, check_hold,
    delete, held, insert,
};
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

fn text_of(result: Result<(), DecideError>) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(DecideError::Reject(reject)) => {
            assert_eq!(reject.code, RejectCode::InvalidState);
            Some(reject.message)
        }
        Err(other) => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_refusal_text_follows_the_kind_of_the_hold() {
    let det = Det::new();
    let store = store(&det).await;
    for id in ["land", "move", "arriving"] {
        store.create_session(new_session(id)).await.unwrap();
    }
    let texts = call(&store.inner.db, |conn| {
        let (a, b, c) = (
            SessionId::from("land"),
            SessionId::from("move"),
            SessionId::from("arriving"),
        );
        insert(conn, &a, KIND_LAND, "l1", 1)?;
        insert(conn, &b, KIND_MOVE, "m1", 1)?;
        insert(conn, &c, KIND_MOVE_IN, "m2", 1)?;
        Ok([a, b, c].map(|s| (text_of(check_free(conn, &s)), text_of(check_hold(conn, &s)))))
    })
    .await
    .unwrap();
    let expect = |text: &str| (Some(text.to_owned()), Some(text.to_owned()));
    assert_eq!(
        texts,
        [expect(HELD), expect(HELD_MOVE), expect(HELD_MOVE_IN)]
    );
    assert_eq!(HELD_MOVE, "The session is being moved.");
}

#[tokio::test]
async fn a_moved_session_is_refused_by_check_free_but_not_by_the_hold_check() {
    let det = Det::new();
    let store = store(&det).await;
    store.create_session(new_session("s1")).await.unwrap();
    let moved: crate::store::NewEvent = fleetdeck_proto::Event::SessionMoved {
        move_id: "m1".to_owned(),
        to_station: "station-b".into(),
        to_session: "t1".into(),
    }
    .into();
    store.append("s1".into(), vec![moved]).await.unwrap();
    let (free, hold) = call(&store.inner.db, |conn| {
        let s = SessionId::from("s1");
        Ok((text_of(check_free(conn, &s)), text_of(check_hold(conn, &s))))
    })
    .await
    .unwrap();
    assert_eq!(
        free.as_deref(),
        Some("The session moved to station-b. Open it there.")
    );
    assert_eq!(hold, None, "the trash of a moved session is allowed");
    // An undone move is free again.
    let undone: crate::store::NewEvent = fleetdeck_proto::Event::SessionMoveUndone {
        move_id: "m1".to_owned(),
    }
    .into();
    store.append("s1".into(), vec![undone]).await.unwrap();
    let free = call(&store.inner.db, |conn| {
        Ok(text_of(check_free(conn, &SessionId::from("s1"))))
    })
    .await
    .unwrap();
    assert_eq!(free, None);
}
