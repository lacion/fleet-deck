//! The move run rows, the tombstones and the publication transaction (design
//! §12.3, §12.9 revision 4 NEW-1, V31).

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MoveInInfo, MoveInState, MovePublication};
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_testkit::Det;

use super::{
    ImportedFile, MoveOutState, NewMoveIn, NewMoveOut, Note, Publish, Staging, Tombstone,
    advance_in, append_imported, get_in, get_out, holding_out, in_by_command, in_of_session,
    insert_in, insert_out, insert_tombstone, is_tombstoned, note_first_input, note_in,
    out_by_command, publish, set_staging, transition_out,
};
use crate::store::git_state::insert_session_git;
use crate::store::git_state::tests::{DEST, new_git, new_session, store};
use crate::store::land_runs::session_facts;
use crate::store::session_holds::{self, KIND_MOVE_IN};
use crate::store::{ReadResult, Store, call};

const OID: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn publication() -> MovePublication {
    MovePublication {
        local_branch: "fd/alpha/s1".to_owned(),
        dest: DEST.to_owned(),
        ref_name: "fd/alpha/s1".to_owned(),
        oid: OID.to_owned(),
    }
}

fn new_out(move_id: &str, command_id: &str, session: &str) -> NewMoveOut {
    NewMoveOut {
        move_id: move_id.to_owned(),
        command_id: command_id.to_owned(),
        session_id: session.into(),
        to_station: "bravo".to_owned(),
        publication: publication(),
        authority: None,
    }
}

fn new_in(move_id: &str, command_id: &str, session: &str) -> NewMoveIn {
    NewMoveIn {
        move_id: move_id.to_owned(),
        command_id: command_id.to_owned(),
        session_id: session.into(),
        provider: ProviderKind::Claude,
        native_session_id: Some("native-1".to_owned()),
        authority: None,
        source_station: Some("alpha".to_owned()),
    }
}

/// The documented admission rows of a moved-in session (design §12.3 step 4,
/// decision): `session_git` with `created_branch = 0` and no publication
/// ref, the `move_in_runs` row and the target hold.
async fn admitted(store: &Store, session: &str, move_id: &str) {
    store.create_session(new_session(session)).await.unwrap();
    let mut git = new_git(session, "p1");
    git.publish_ref = None;
    git.created_branch = false;
    let (session, move_id) = (session.to_owned(), move_id.to_owned());
    call(&store.inner.db, move |conn| {
        insert_session_git(conn, &git, 10)?;
        insert_in(
            conn,
            &new_in(&move_id, &format!("cmd-{move_id}"), &session),
            10,
        )?;
        session_holds::insert(conn, &SessionId::from(session), KIND_MOVE_IN, &move_id, 10)?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn move_in_events(store: &Store, session: &str) -> Vec<MoveInInfo> {
    let ReadResult::Events { events, .. } = store.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter(|e| e.session_id.as_str() == session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionMoveIn(info)) => Some(info),
            _ => None,
        })
        .collect()
}

fn publish_of<'a>(session: &'a SessionId, move_id: &'a str, publish_ref: &'a str) -> Publish<'a> {
    Publish {
        session_id: session,
        move_id,
        publish_ref,
        target: "origin",
        dest: DEST,
        oid: OID,
    }
}

#[tokio::test]
async fn the_source_row_round_trips_and_moves_by_conditional_transitions() {
    let det = Det::new();
    let store = store(&det).await;
    store.create_session(new_session("s1")).await.unwrap();
    let s1 = SessionId::from("s1");
    let outcome = call(&store.inner.db, {
        let s1 = s1.clone();
        move |conn| {
            insert_out(conn, &new_out("m1", "cmd-1", "s1"), 100)?;
            let by_move = get_out(conn, "m1")?;
            let by_command = out_by_command(conn, "cmd-1")?;
            let holding = holding_out(conn, &s1)?;
            // The wrong expected state changes nothing.
            let wrong = transition_out(
                conn,
                "m1",
                MoveOutState::Finished,
                MoveOutState::Undone,
                200,
            )?;
            let right = transition_out(
                conn,
                "m1",
                MoveOutState::Holding,
                MoveOutState::Finished,
                300,
            )?;
            let again = transition_out(
                conn,
                "m1",
                MoveOutState::Holding,
                MoveOutState::Aborted,
                400,
            )?;
            let after = get_out(conn, "m1")?;
            let none_holding = holding_out(conn, &s1)?;
            let duplicate = insert_out(conn, &new_out("m2", "cmd-1", "s1"), 500).is_err();
            Ok((
                by_move,
                by_command,
                holding,
                wrong,
                right,
                again,
                after,
                none_holding,
                duplicate,
            ))
        }
    })
    .await
    .unwrap();
    let (by_move, by_command, holding, wrong, right, again, after, none_holding, duplicate) =
        outcome;
    let row = by_move.unwrap();
    assert_eq!(row.state, MoveOutState::Holding);
    assert_eq!(row.publication, publication());
    assert_eq!(
        (row.to_station.as_str(), row.authority.clone()),
        ("bravo", None)
    );
    assert_eq!(by_command, Some(row.clone()));
    assert_eq!(holding, Some(row));
    assert!(!wrong);
    assert!(right);
    assert!(!again);
    let after = after.unwrap();
    assert_eq!(
        (after.state, after.updated_at),
        (MoveOutState::Finished, 300)
    );
    assert_eq!(none_holding, None);
    assert!(duplicate, "a command id makes one run");
    assert_eq!(
        store.move_out_run("m1".to_owned()).await.unwrap(),
        Some(after)
    );
}

#[tokio::test]
async fn the_admission_insert_passes_the_v28_checks_and_a_direct_branch_flag_does_not() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let facts = call(&store.inner.db, |conn| {
        session_facts(conn, &SessionId::from("s1"))
    })
    .await
    .unwrap()
    .unwrap();
    assert!(!facts.created_branch);
    assert_eq!(facts.publish_ref, None);
    assert_eq!(facts.branch.as_deref(), Some("fd/alpha/s1"));
    let row = store.move_in_run("m1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.state, MoveInState::Staged);
    assert_eq!(row.provider, ProviderKind::Claude);
    assert_eq!(row.native_session_id.as_deref(), Some("native-1"));
    let hold = store.session_hold("s1".into()).await.unwrap().unwrap();
    assert_eq!(
        (hold.kind.as_str(), hold.holder_id.as_str()),
        (KIND_MOVE_IN, "m1")
    );

    // The constraint stays: `created_branch = 1` needs a publication ref.
    store.create_session(new_session("s2")).await.unwrap();
    let mut bad = new_git("s2", "p1");
    bad.publish_ref = None;
    bad.created_branch = true;
    let refused = call(&store.inner.db, move |conn| {
        Ok(insert_session_git(conn, &bad, 10).is_err())
    })
    .await
    .unwrap();
    assert!(refused);
    let flipped = call(&store.inner.db, |conn| {
        Ok(conn
            .execute(
                "UPDATE session_git SET created_branch = 1 WHERE session_id = 's1'",
                [],
            )
            .is_err())
    })
    .await
    .unwrap();
    assert!(
        flipped,
        "the CHECK refuses created_branch = 1 without publish_ref"
    );
}

#[tokio::test]
async fn the_publication_transaction_sets_both_values_and_inserts_the_push_row() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let done = store
        .move_publish(
            "s1".into(),
            "m1".to_owned(),
            "fd/alpha/calm".to_owned(),
            "origin".to_owned(),
            DEST.to_owned(),
            OID.to_owned(),
        )
        .await
        .unwrap();
    assert!(done);
    let facts = call(&store.inner.db, |conn| {
        session_facts(conn, &SessionId::from("s1"))
    })
    .await
    .unwrap()
    .unwrap();
    assert!(facts.created_branch);
    assert_eq!(facts.publish_ref.as_deref(), Some("fd/alpha/calm"));
    assert_eq!(facts.branch.as_deref(), Some("fd/alpha/s1"));
    let push = store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.ref_name, "fd/alpha/calm");
    assert_eq!(push.target_dest, DEST);
    assert_eq!(
        push.last_pushed_oid, None,
        "the first push uses the empty lease"
    );
    assert_eq!(push.desired_oid.as_deref(), Some(OID));
    assert_eq!(push.state, fleetdeck_proto::fleet_git::PushState::Due);
    // A second run changes nothing: the session has a publication ref now.
    let again = store
        .move_publish(
            "s1".into(),
            "m1".to_owned(),
            "fd/alpha/other".to_owned(),
            "origin".to_owned(),
            DEST.to_owned(),
            OID.to_owned(),
        )
        .await
        .unwrap();
    assert!(!again);
}

#[tokio::test]
async fn a_tombstone_or_another_state_makes_the_publication_change_nothing() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    admitted(&store, "s2", "m2").await;
    let (tombstoned, wrong_state) = call(&store.inner.db, |conn| {
        insert_tombstone(conn, "m1", 20)?;
        let s1 = SessionId::from("s1");
        let s2 = SessionId::from("s2");
        let tombstoned = publish(conn, &publish_of(&s1, "m1", "fd/alpha/a"), 30)?;
        // `imported` is not `staged`: the transaction needs `staged`.
        advance_in(
            conn,
            "m2",
            MoveInState::Staged,
            MoveInState::Imported,
            None,
            Tombstone::Ignore,
            25,
        )?;
        let wrong_state = publish(conn, &publish_of(&s2, "m2", "fd/alpha/b"), 30)?;
        Ok((tombstoned, wrong_state))
    })
    .await
    .unwrap();
    assert!(!tombstoned);
    assert!(!wrong_state);
    for session in ["s1", "s2"] {
        let facts = call(&store.inner.db, move |conn| {
            session_facts(conn, &SessionId::from(session))
        })
        .await
        .unwrap()
        .unwrap();
        assert!(!facts.created_branch);
        assert_eq!(facts.publish_ref, None);
        assert_eq!(store.git_push(session.into()).await.unwrap(), None);
    }
}

#[tokio::test]
async fn a_failing_push_insert_rolls_the_session_update_back() {
    let det = Det::new();
    let store = store(&det).await;
    // Another session holds (DEST, fd/alpha/taken) already.
    store.create_session(new_session("other")).await.unwrap();
    let mut other = new_git("other", "p1");
    other.publish_ref = Some("fd/alpha/other".to_owned());
    store
        .git_record_session(other, Some(("origin".to_owned(), DEST.to_owned())))
        .await
        .unwrap();
    admitted(&store, "s1", "m1").await;
    let taken = store
        .move_publish(
            "s1".into(),
            "m1".to_owned(),
            "fd/alpha/other".to_owned(),
            "origin".to_owned(),
            DEST.to_owned(),
            OID.to_owned(),
        )
        .await;
    assert!(
        taken.is_err(),
        "UNIQUE (target_dest, ref) refuses the insert"
    );
    let facts = call(&store.inner.db, |conn| {
        session_facts(conn, &SessionId::from("s1"))
    })
    .await
    .unwrap()
    .unwrap();
    assert!(!facts.created_branch, "the update rolled back");
    assert_eq!(facts.publish_ref, None);
    assert_eq!(store.git_push("s1".into()).await.unwrap(), None);
}

#[tokio::test]
async fn before_the_publication_nothing_selects_the_session() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    // The push loop selects no row, and `fleet_git.push_now` has no row to
    // answer (`not_synced`).
    assert!(store.git_push_due(i64::MAX).await.unwrap().is_empty());
    assert_eq!(store.git_push_now("s1".into()).await.unwrap(), None);
    // The land decision reads these facts: no branch of its own yet.
    let facts = call(&store.inner.db, |conn| {
        session_facts(conn, &SessionId::from("s1"))
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!((facts.created_branch, facts.publish_ref), (false, None));
    assert_eq!(facts.push_dest, None);
    // After the transaction the loop sees the row (the hold skips it, T4.2d).
    store
        .move_publish(
            "s1".into(),
            "m1".to_owned(),
            "fd/alpha/calm".to_owned(),
            "origin".to_owned(),
            DEST.to_owned(),
            OID.to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(store.git_push_due(i64::MAX).await.unwrap().len(), 1);
}

#[tokio::test]
async fn each_transition_with_the_wrong_expected_state_changes_zero_rows() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let outcome = call(&store.inner.db, |conn| {
        let wrong = advance_in(
            conn,
            "m1",
            MoveInState::Imported,
            MoveInState::Launching,
            None,
            Tombstone::Refuse,
            50,
        )?;
        let unknown = advance_in(
            conn,
            "nope",
            MoveInState::Staged,
            MoveInState::Imported,
            None,
            Tombstone::Refuse,
            50,
        )?;
        let wrong_note = note_in(
            conn,
            "m1",
            MoveInState::Imported,
            Note {
                code: "move_branch_exists",
                text: "x",
            },
            50,
        )?;
        let first = advance_in(
            conn,
            "m1",
            MoveInState::Staged,
            MoveInState::Imported,
            None,
            Tombstone::Refuse,
            60,
        )?;
        let second = advance_in(
            conn,
            "m1",
            MoveInState::Staged,
            MoveInState::Imported,
            None,
            Tombstone::Refuse,
            70,
        )?;
        let row = get_in(conn, "m1")?.unwrap();
        Ok((
            wrong.is_some(),
            unknown.is_some(),
            wrong_note.is_some(),
            first.is_some(),
            second.is_some(),
            row,
        ))
    })
    .await
    .unwrap();
    let (wrong, unknown, wrong_note, first, second, row) = outcome;
    assert_eq!(
        (wrong, unknown, wrong_note, first, second),
        (false, false, false, true, false)
    );
    assert_eq!((row.state, row.updated_at), (MoveInState::Imported, 60));
    let states: Vec<_> = move_in_events(&store, "s1")
        .await
        .into_iter()
        .map(|i| i.state)
        .collect();
    assert_eq!(
        states,
        vec![MoveInState::Imported],
        "one event for each change"
    );
}

#[tokio::test]
async fn a_transition_stores_its_code_and_text_and_a_note_keeps_the_state() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    call(&store.inner.db, |conn| {
        note_in(
            conn,
            "m1",
            MoveInState::Staged,
            Note {
                code: "move_branch_exists",
                text: "The branch exists.",
            },
            40,
        )?;
        advance_in(
            conn,
            "m1",
            MoveInState::Staged,
            MoveInState::Abandoned,
            Some(Note {
                code: "move_cancelled",
                text: "Cancelled.",
            }),
            Tombstone::Ignore,
            50,
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let infos = move_in_events(&store, "s1").await;
    assert_eq!(infos.len(), 2);
    assert_eq!(infos[0].state, MoveInState::Staged);
    assert_eq!(infos[0].code.as_deref(), Some("move_branch_exists"));
    assert_eq!(infos[1].state, MoveInState::Abandoned);
    assert_eq!(infos[1].text.as_deref(), Some("Cancelled."));
    let row = store.move_in_run("m1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.code.as_deref(), Some("move_cancelled"));
}

#[tokio::test]
async fn a_tombstone_is_durable_idempotent_and_refuses_a_guarded_transition() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    // Also for an id that the station does not know.
    assert!(!store.move_tombstoned("ghost".to_owned()).await.unwrap());
    let (ghost, first, second, guarded, ignored) = call(&store.inner.db, |conn| {
        insert_tombstone(conn, "ghost", 100)?;
        insert_tombstone(conn, "ghost", 200)?;
        let first: i64 = conn.query_row(
            "SELECT at FROM move_tombstones WHERE move_id = 'ghost'",
            [],
            |r| r.get(0),
        )?;
        insert_tombstone(conn, "m1", 300)?;
        let guarded = advance_in(
            conn,
            "m1",
            MoveInState::Staged,
            MoveInState::Launching,
            None,
            Tombstone::Refuse,
            310,
        )?;
        let ignored = advance_in(
            conn,
            "m1",
            MoveInState::Staged,
            MoveInState::Discarded,
            None,
            Tombstone::Ignore,
            320,
        )?;
        Ok((
            is_tombstoned(conn, "ghost")?,
            first,
            is_tombstoned(conn, "m1")?,
            guarded.is_some(),
            ignored.is_some(),
        ))
    })
    .await
    .unwrap();
    assert!(ghost && second);
    assert_eq!(first, 100, "the first time stays");
    assert!(!guarded, "a tombstone refuses the launch admission");
    assert!(ignored, "the discard itself is not refused");
    assert!(store.move_tombstoned("m1".to_owned()).await.unwrap());
    assert!(
        !store
            .move_in_advance(
                "m1".to_owned(),
                MoveInState::Staged,
                MoveInState::Launching,
                None,
                Tombstone::Refuse,
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn the_wrapper_publishes_the_event_and_reports_a_lost_race() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let advance =
        |from, to| store.move_in_advance("m1".to_owned(), from, to, None, Tombstone::Refuse);
    assert!(
        advance(MoveInState::Staged, MoveInState::Imported)
            .await
            .unwrap()
    );
    assert!(
        !advance(MoveInState::Staged, MoveInState::Imported)
            .await
            .unwrap()
    );
    assert!(
        advance(MoveInState::Imported, MoveInState::Launching)
            .await
            .unwrap()
    );
    let states: Vec<_> = move_in_events(&store, "s1")
        .await
        .into_iter()
        .map(|i| i.state)
        .collect();
    assert_eq!(states, vec![MoveInState::Imported, MoveInState::Launching]);
}

#[tokio::test]
async fn imported_files_are_appended_in_order_whatever_the_state() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let staging = Staging {
        path: "/data/moves/m1".to_owned(),
        dev: 7,
        ino: 99,
    };
    let file = |n: u64| ImportedFile {
        path: format!("/home/.claude/projects/p/{n}.jsonl"),
        dev: 7,
        ino: n,
    };
    let (set, unknown, row) = call(&store.inner.db, {
        let staging = staging.clone();
        move |conn| {
            let set = set_staging(conn, "m1", &staging, 20)?;
            let unknown = append_imported(conn, "ghost", &file(1), 21)?;
            append_imported(conn, "m1", &file(1), 22)?;
            advance_in(
                conn,
                "m1",
                MoveInState::Staged,
                MoveInState::Discarded,
                None,
                Tombstone::Ignore,
                23,
            )?;
            // The record is kept after the state changed.
            append_imported(conn, "m1", &file(2), 24)?;
            // The staging is only set while `staged`.
            let late = set_staging(conn, "m1", &staging, 25)?;
            Ok((set && !late, unknown, get_in(conn, "m1")?.unwrap()))
        }
    })
    .await
    .unwrap();
    assert!(set);
    assert!(!unknown);
    assert_eq!(row.staging, Some(staging));
    assert_eq!(
        row.imported.iter().map(|f| f.ino).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(row.state, MoveInState::Discarded);
}

#[tokio::test]
async fn the_first_input_is_stored_once_and_only_after_the_release() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let outcome = call(&store.inner.db, |conn| {
        let staged = note_first_input(conn, "m1", 10)?;
        for (from, to) in [
            (MoveInState::Staged, MoveInState::Imported),
            (MoveInState::Imported, MoveInState::Launching),
            (MoveInState::Launching, MoveInState::Resumed),
        ] {
            advance_in(conn, "m1", from, to, None, Tombstone::Refuse, 11)?;
        }
        let resumed = note_first_input(conn, "m1", 12)?;
        advance_in(
            conn,
            "m1",
            MoveInState::Resumed,
            MoveInState::Released,
            None,
            Tombstone::Ignore,
            13,
        )?;
        let first = note_first_input(conn, "m1", 14)?;
        let second = note_first_input(conn, "m1", 15)?;
        Ok((staged, resumed, first, second, get_in(conn, "m1")?.unwrap()))
    })
    .await
    .unwrap();
    let (staged, resumed, first, second, row) = outcome;
    assert_eq!(
        (staged, resumed, first, second),
        (false, false, true, false)
    );
    assert_eq!(row.first_input_at, Some(14));
}

#[tokio::test]
async fn the_target_row_is_found_by_command_and_by_session() {
    let det = Det::new();
    let store = store(&det).await;
    admitted(&store, "s1", "m1").await;
    let (by_command, by_session, other, duplicate) = call(&store.inner.db, |conn| {
        Ok((
            in_by_command(conn, "cmd-m1")?,
            in_of_session(conn, &SessionId::from("s1"))?,
            in_by_command(conn, "cmd-none")?,
            insert_in(conn, &new_in("m2", "cmd-m1", "s1"), 5).is_err(),
        ))
    })
    .await
    .unwrap();
    assert_eq!(by_command.as_ref().map(|r| r.move_id.as_str()), Some("m1"));
    assert_eq!(by_command, by_session);
    assert_eq!(other, None);
    assert!(duplicate, "a command id makes one run");
}
