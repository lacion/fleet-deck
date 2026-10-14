//! The stable turn selector of `files.diff` (DL-179): a finalized turn of
//! one session by the exact pair of its checkpoint commits, even when the
//! provider reuses a turn id.

use std::path::Path;
use std::sync::Arc;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::files::{CheckpointRef, TurnDiff, TurnDiffQuery, code};
use fleetdeck_testkit::Det;

use super::files_tests::policy;
use super::{FileError, diff};
use crate::manager::turns::turns_tests::{S1, completed, dispatch, input, repo, setup, started};
use crate::store::{RowSide, Store};
use crate::workspace::{GitOptions, PrivatePaths};

fn query(turn: Option<&str>, pair: Option<(&str, &str)>) -> TurnDiffQuery {
    TurnDiffQuery {
        session_id: S1.into(),
        turn_id: turn.map(TurnId::from),
        checkpoint: pair.map(|(from, to)| CheckpointRef {
            from_commit: from.to_owned(),
            to_commit: to.to_owned(),
        }),
        max_bytes: None,
    }
}

async fn ask(s: &Store, dir: &Path, q: TurnDiffQuery) -> Result<TurnDiff, FileError> {
    diff(
        s,
        &policy(dir),
        &PrivatePaths::default(),
        &GitOptions::isolated(),
        q,
    )
    .await
}

type Pair = (String, String);

/// Two finalized turns of one session that both carry the provider turn id
/// `turn-1` (a provider restart reuses the ids): the first writes `first
/// turn`, the second `second turn`.
async fn two_turns_with_one_turn_id(dir: &Path) -> (Store, Pair, Pair) {
    let det = Det::new();
    let (s, turns) = setup(&det, dir).await;
    for (n, text) in [(1, "first turn\n"), (2, "second turn\n")] {
        // The provider restarts between the turns and starts its ids again.
        turns.set_process(format!("proc-{n}").into());
        dispatch(&turns, &format!("in-{n}")).await;
        turns.observe(&[started("turn-1")]).await;
        std::fs::write(dir.join("a.txt"), text).unwrap();
        turns
            .observe(&[
                input(&format!("in-{n}"), Some("turn-1"), InputState::Started),
                completed("turn-1"),
            ])
            .await;
        Arc::clone(&turns).capture_due().await;
    }
    // The rows in the order that the store gives them (oldest first).
    let rows = s.turn_checkpoints(S1.into()).await.unwrap();
    assert_eq!(rows.len(), 2);
    let pair = |row: &crate::store::TurnCheckpointRow| {
        let (RowSide::Available(from), RowSide::Available(to)) = (&row.start, &row.end) else {
            panic!("a pair of checkpoints");
        };
        (from.clone(), to.clone())
    };
    let (first, second) = (pair(&rows[0]), pair(&rows[1]));
    (s, first, second)
}

#[tokio::test]
async fn the_selector_returns_the_turn_with_exactly_that_commit_pair() {
    let (_t, dir) = repo();
    let (s, first, second) = two_turns_with_one_turn_id(&dir).await;
    assert_ne!(first, second);
    // The turn id alone finds the newest row, as before.
    let by_id = ask(&s, &dir, query(Some("turn-1"), None)).await.unwrap();
    assert_eq!((by_id.from_commit, by_id.to_commit), second.clone());
    assert!(by_id.patch.contains("+second turn"), "{}", by_id.patch);
    // The pair finds each row, also the older one that the id cannot reach.
    let one = ask(&s, &dir, query(None, Some((&first.0, &first.1))))
        .await
        .unwrap();
    assert_eq!((one.from_commit.clone(), one.to_commit.clone()), first);
    assert!(one.patch.contains("+first turn"), "{}", one.patch);
    assert!(!one.patch.contains("second turn"), "{}", one.patch);
    assert_eq!(one.turn_id.as_str(), "turn-1");
    let two = ask(&s, &dir, query(None, Some((&second.0, &second.1))))
        .await
        .unwrap();
    assert!(two.patch.contains("+second turn"), "{}", two.patch);
    // The selector wins over a turn id.
    let both = ask(&s, &dir, query(Some("turn-9"), Some((&first.0, &first.1))))
        .await
        .unwrap();
    assert_eq!(both.from_commit, first.0);
}

#[tokio::test]
async fn a_pair_that_is_not_a_finalized_row_of_the_session_is_unavailable() {
    let (_t, dir) = repo();
    let (s, first, second) = two_turns_with_one_turn_id(&dir).await;
    // The halves of two different rows are not a row.
    for pair in [
        (first.0.clone(), second.1.clone()),
        (first.1.clone(), first.0.clone()),
        ("0".repeat(40), first.1.clone()),
        (first.0.clone(), "f".repeat(40)),
    ] {
        let e = ask(&s, &dir, query(None, Some((&pair.0, &pair.1))))
            .await
            .unwrap_err();
        assert_eq!(e.code, code::CHECKPOINT_UNAVAILABLE, "{pair:?}");
    }
    // The pair of another session: a second store with its own session and
    // its own commits.
    let (_t2, dir2) = repo();
    let (other, o_first, _) = two_turns_with_one_turn_id(&dir2).await;
    assert_ne!(o_first, first);
    let e = ask(&s, &dir, query(None, Some((&o_first.0, &o_first.1))))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::CHECKPOINT_UNAVAILABLE);
    // It is a row in its own store.
    assert!(
        ask(&other, &dir2, query(None, Some((&o_first.0, &o_first.1))))
            .await
            .is_ok()
    );
    // A query that names neither a turn id nor a pair is a bad query.
    let e = ask(&s, &dir, query(None, None)).await.unwrap_err();
    assert_eq!(e.code, code::INVALID_QUERY);
    // An unknown session keeps its code.
    let mut q = query(None, Some((&first.0, &first.1)));
    q.session_id = "nope".into();
    assert_eq!(
        ask(&s, &dir, q).await.unwrap_err().code,
        code::UNKNOWN_SESSION
    );
}

#[test]
fn the_query_accepts_the_old_and_the_new_wire_forms() {
    let old: TurnDiffQuery =
        serde_json::from_str(r#"{"session_id":"s1","turn_id":"turn-1"}"#).unwrap();
    assert_eq!(old.turn_id, Some(TurnId::from("turn-1")));
    assert_eq!(old.checkpoint, None);
    let new: TurnDiffQuery = serde_json::from_str(
        r#"{"session_id":"s1","checkpoint":{"from_commit":"a","to_commit":"b"}}"#,
    )
    .unwrap();
    assert_eq!(new.turn_id, None);
    assert_eq!(new.checkpoint.clone().unwrap().to_commit, "b");
    assert!(
        serde_json::from_str::<TurnDiffQuery>(
            r#"{"session_id":"s1","checkpoint":{"from_commit":"a","to_commit":"b","x":1}}"#
        )
        .is_err()
    );
    // The deck sends no `turn_id` with a pair.
    let sent = serde_json::to_value(&new).unwrap();
    assert!(sent.get("turn_id").is_none());
}
