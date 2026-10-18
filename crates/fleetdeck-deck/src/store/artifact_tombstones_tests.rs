//! The artifact tombstones (R13 P5a task C): the migration, the one
//! transaction of the deletion, the guard of every write and every read, and
//! the state changes of the station job.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::{EventEnvelope, StationId};

use super::super::artifacts::NewShare;
use super::super::review_tests::{W, setup};
use super::{DeleteStart, TombstoneDue, TryOutcome, retry_delay_ms};

fn artifact(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: Some("s1".into()),
        title: "Report".to_owned(),
        kind: ArtifactKind::Html,
        latest_version: 1,
        created_at: 10,
        updated_at: 10,
        author: "agent".to_owned(),
    }
}

fn version(id: &str) -> ArtifactVersion {
    ArtifactVersion {
        artifact_id: id.to_owned(),
        version: 1,
        size: 3,
        sha256: "00".repeat(32),
        files: vec!["index.html".to_owned()],
        created_at: 10,
        capture: None,
    }
}

fn share(station: &StationId, id: &str, hash: &str, view: bool) -> NewShare {
    NewShare {
        token_hash: hash.repeat(64).chars().take(64).collect(),
        station_id: station.clone(),
        artifact_id: id.to_owned(),
        version: Some(1),
        view,
        created_by: "owner".to_owned(),
        expires_at: i64::MAX,
    }
}

/// The index row, one version, one cached file, one share and one view link.
async fn seed(w: &W, id: &str, hash: (&str, &str)) {
    let st = w.station.clone();
    w.s.note_artifact(st.clone(), &artifact(id)).await.unwrap();
    w.s.put_artifact_versions(st.clone(), vec![version(id)])
        .await
        .unwrap();
    w.s.put_artifact_file(
        st.clone(),
        id.to_owned(),
        1,
        "index.html".to_owned(),
        b"abc".to_vec(),
    )
    .await
    .unwrap();
    assert!(
        w.s.create_artifact_share(share(&st, id, hash.0, false))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        w.s.create_artifact_share(share(&st, id, hash.1, true))
            .await
            .unwrap()
            .is_some()
    );
}

async fn copies(w: &W, id: &'static str) -> i64 {
    let mut n = 0;
    for table in [
        "artifacts",
        "artifact_versions",
        "artifact_cache",
        "artifact_shares",
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE artifact_id = '{id}'");
        n += super::super::call(&w.s.inner.db, move |conn| {
            Ok(conn.query_row(&sql, [], |r| r.get::<_, i64>(0))?)
        })
        .await
        .unwrap();
    }
    n
}

fn published(seq: u64, id: &str) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: "s1".into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::ArtifactPublished {
            artifact_id: id.to_owned(),
            version: 2,
            title: "Again".to_owned(),
            artifact_kind: ArtifactKind::Html,
        }),
    }
}

#[test]
fn a_v30_database_migrates_to_v31_with_the_artifact_tombstones() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = crate::store::schema::migrations();
    migrations.to_version(&mut conn, 30).unwrap();
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'artifact_tombstones'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, 0);
    migrations.to_latest(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO artifact_tombstones (station_id, artifact_id, state, created_at, updated_at)
         VALUES ('st', 'a1', 'pending', 1, 1)",
        [],
    )
    .unwrap();
    let attempts: i64 = conn
        .query_row("SELECT attempts FROM artifact_tombstones", [], |r| r.get(0))
        .unwrap();
    assert_eq!(attempts, 0);
    // The key is the identity: a second row for it is refused.
    assert!(
        conn.execute(
            "INSERT INTO artifact_tombstones (station_id, artifact_id, state, created_at, updated_at)
             VALUES ('st', 'a1', 'pending', 1, 1)",
            [],
        )
        .is_err()
    );
}

#[tokio::test]
async fn an_artifact_delete_removes_every_deck_copy_in_one_step() {
    let w = setup().await;
    seed(&w, "a1", ("aa", "bb")).await;
    seed(&w, "a2", ("cc", "dd")).await;
    assert_eq!(copies(&w, "a1").await, 5);
    let start =
        w.s.artifact_delete(w.station.clone(), "a1".to_owned())
            .await
            .unwrap();
    assert_eq!(start, DeleteStart::Created);
    assert_eq!(copies(&w, "a1").await, 0);
    // Another artifact is not touched.
    assert_eq!(copies(&w, "a2").await, 5);
    let view =
        w.s.tombstone(w.station.clone(), "a1".to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        (view.state.as_str(), view.reason.as_deref()),
        ("pending", Some("retry"))
    );
    assert!(
        w.s.tombstone(w.station.clone(), "a2".to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_repeated_artifact_delete_gives_the_state_and_an_unknown_one_gives_unknown() {
    let w = setup().await;
    seed(&w, "a1", ("aa", "bb")).await;
    let id = "a1".to_owned();
    w.s.artifact_delete(w.station.clone(), id.clone())
        .await
        .unwrap();
    w.s.tombstone_set(w.station.clone(), id.clone(), TryOutcome::Unsupported, 5)
        .await
        .unwrap();
    let again =
        w.s.artifact_delete(w.station.clone(), id.clone())
            .await
            .unwrap();
    let DeleteStart::Existing(view) = again else {
        panic!("{again:?}");
    };
    assert_eq!(
        (view.state.as_str(), view.reason.as_deref()),
        ("pending", Some("unsupported"))
    );
    let rows = w.rows("SELECT COUNT(*) FROM artifact_tombstones").await;
    assert_eq!(rows, 1);
    let unknown =
        w.s.artifact_delete(w.station.clone(), "nothing".to_owned())
            .await
            .unwrap();
    assert_eq!(unknown, DeleteStart::Unknown);
    // An unknown identity makes no tombstone.
    assert_eq!(w.rows("SELECT COUNT(*) FROM artifact_tombstones").await, 1);
}

#[tokio::test]
async fn a_tombstoned_artifact_is_restored_by_no_write() {
    let w = setup().await;
    seed(&w, "a1", ("aa", "bb")).await;
    let st = w.station.clone();
    w.s.artifact_delete(st.clone(), "a1".to_owned())
        .await
        .unwrap();
    // The index row through the fold of an event.
    w.s.apply_events(st.clone(), "i1".into(), vec![published(2, "a1")])
        .await
        .unwrap();
    // The index row of a publish that the deck relayed.
    w.s.note_artifact(st.clone(), &artifact("a1"))
        .await
        .unwrap();
    w.s.put_artifact_versions(st.clone(), vec![version("a1")])
        .await
        .unwrap();
    w.s.put_artifact_file(
        st.clone(),
        "a1".to_owned(),
        1,
        "index.html".to_owned(),
        vec![1],
    )
    .await
    .unwrap();
    for (hash, view) in [("ee", false), ("ff", true)] {
        let made =
            w.s.create_artifact_share(share(&st, "a1", hash, view))
                .await
                .unwrap();
        assert!(made.is_none(), "view={view}");
    }
    assert_eq!(copies(&w, "a1").await, 0);
    // The fold still moved the cursor: the event is consumed, not repeated.
    assert_eq!(
        w.s.station(st).await.unwrap().unwrap().cursor,
        2,
        "the event is folded"
    );
}

#[tokio::test]
async fn every_artifact_read_leaves_out_a_tombstoned_identity() {
    let w = setup().await;
    seed(&w, "a1", ("aa", "bb")).await;
    seed(&w, "a2", ("cc", "dd")).await;
    let st = w.station.clone();
    // A tombstone and the rows that a bug could leave behind: the reads
    // must not show them.
    super::super::call(&w.s.inner.db, {
        let st = st.clone();
        move |conn| {
            conn.execute(
                "INSERT INTO artifact_tombstones
                     (station_id, artifact_id, state, created_at, updated_at)
                 VALUES (?1, 'a1', 'deleted', 1, 1)",
                [st.as_str()],
            )?;
            Ok(())
        }
    })
    .await
    .unwrap();
    let listed = w.s.artifacts(Some(st.clone()), None, None).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|a| a.artifact_id.as_str())
            .collect::<Vec<_>>(),
        ["a2"]
    );
    assert!(
        w.s.artifact(st.clone(), "a1".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        w.s.artifact(st.clone(), "a2".to_owned())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        w.s.artifact_versions(st.clone(), "a1".to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        w.s.cached_artifact_file(st.clone(), "a1".to_owned(), 1, "index.html".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        w.s.cached_versions(st.clone(), "a1".to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        w.s.shares_of(st.clone(), "a1".to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    let hidden = share(&st, "a1", "aa", false).token_hash;
    assert!(w.s.share_by_hash(hidden.clone()).await.unwrap().is_none());
    w.s.count_share_view(hidden).await.unwrap();
    assert_eq!(
        w.rows("SELECT SUM(views) FROM artifact_shares WHERE artifact_id = 'a1'")
            .await,
        0
    );
    // The other artifact reads as before.
    assert_eq!(
        w.s.shares_of(st.clone(), "a2".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(w.s.cached_versions(st, "a2".to_owned()).await.unwrap(), [1]);
}

#[tokio::test]
async fn the_artifact_job_state_follows_the_outcome_and_a_deleted_row_stays_deleted() {
    let w = setup().await;
    seed(&w, "a1", ("aa", "bb")).await;
    let (st, id) = (w.station.clone(), "a1".to_owned());
    w.s.artifact_delete(st.clone(), id.clone()).await.unwrap();
    let set = |outcome, now| {
        let (s, st, id) = (w.s.clone(), st.clone(), id.clone());
        async move { s.tombstone_set(st, id, outcome, now).await.unwrap() }
    };
    let row = || async {
        let sql = "SELECT state, COALESCE(reason, ''), attempts, COALESCE(next_at, -1)
                   FROM artifact_tombstones WHERE artifact_id = 'a1'";
        super::super::call(&w.s.inner.db, move |conn| {
            Ok(conn.query_row(sql, [], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?)
        })
        .await
        .unwrap()
    };
    set(TryOutcome::Offline, 100).await;
    assert_eq!(row().await, ("pending".into(), "offline".into(), 0, -1));
    set(TryOutcome::Unsupported, 100).await;
    assert_eq!(row().await, ("pending".into(), "unsupported".into(), 0, -1));
    // The delays are 1, 2, 4, 8, 16, 32, then 60 seconds.
    let mut now = 1_000;
    for (n, delay) in [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000]
        .into_iter()
        .enumerate()
    {
        set(TryOutcome::Retry, now).await;
        let attempts = i64::try_from(n).unwrap() + 1;
        assert_eq!(
            row().await,
            ("pending".into(), "retry".into(), attempts, now + delay)
        );
        now += 1_000_000;
    }
    set(TryOutcome::Deleted, now).await;
    assert_eq!(row().await.0, "deleted");
    assert_eq!(row().await.1, "");
    // A deleted row never goes back to pending.
    for outcome in [
        TryOutcome::Retry,
        TryOutcome::Offline,
        TryOutcome::Unsupported,
    ] {
        set(outcome, now + 1).await;
        assert_eq!(row().await.0, "deleted", "{outcome:?}");
    }
    assert!(w.s.tombstones_pending(st).await.unwrap().is_empty());
}

#[test]
fn the_artifact_retry_delays_are_one_two_four_eight_sixteen_thirty_two_then_sixty_seconds() {
    let delays: Vec<i64> = (0..8).map(retry_delay_ms).collect();
    assert_eq!(
        delays,
        [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000]
    );
}

#[tokio::test]
async fn the_artifact_pending_list_tells_a_link_row_from_a_due_and_a_waiting_retry() {
    let w = setup().await;
    let st = w.station.clone();
    for id in ["a1", "a2", "a3", "a4"] {
        w.s.note_artifact(st.clone(), &artifact(id)).await.unwrap();
        w.s.artifact_delete(st.clone(), id.to_owned())
            .await
            .unwrap();
    }
    let now = w.s.now_ms();
    // a1: offline; a2: retry that is due; a3: retry that waits; a4: deleted.
    w.s.tombstone_set(st.clone(), "a1".to_owned(), TryOutcome::Offline, now)
        .await
        .unwrap();
    w.s.tombstone_set(st.clone(), "a2".to_owned(), TryOutcome::Retry, now - 10_000)
        .await
        .unwrap();
    w.s.tombstone_set(
        st.clone(),
        "a3".to_owned(),
        TryOutcome::Retry,
        now + 100_000,
    )
    .await
    .unwrap();
    w.s.tombstone_set(st.clone(), "a4".to_owned(), TryOutcome::Deleted, now)
        .await
        .unwrap();
    let mut pending = w.s.tombstones_pending(st).await.unwrap();
    pending.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        pending
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["a1", "a2", "a3"]
    );
    let by: std::collections::BTreeMap<_, _> = pending.into_iter().collect();
    assert_eq!(by["a1"], TombstoneDue::OnLink);
    assert_eq!(by["a2"], TombstoneDue::Due);
    assert_eq!(by["a3"], TombstoneDue::Waiting);
}
