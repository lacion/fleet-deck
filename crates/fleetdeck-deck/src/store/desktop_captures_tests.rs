//! The owner screenshot record (R13 P5a task E): the migration, the send
//! states, the atomic completion, the tombstone, and the capture-cache bound.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactPublished, ArtifactVersion};
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_testkit::Det;

use super::{CaptureState, Completed, DeckStore, Prepared, stamp_published};

const ID1: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ID2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const ID3: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

struct W {
    s: DeckStore,
    det: Det,
    st: StationId,
}

async fn setup() -> W {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    W {
        s,
        det,
        st: StationId::from("st-1".to_owned()),
    }
}

fn published(artifact_id: &str) -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: artifact_id.to_owned(),
            station_id: "station-side-name".to_owned(),
            session_id: None,
            title: "Desktop capture".to_owned(),
            kind: ArtifactKind::Image,
            latest_version: 1,
            created_at: 5_000,
            updated_at: 5_000,
            author: "owner".to_owned(),
        },
        version: ArtifactVersion {
            artifact_id: artifact_id.to_owned(),
            version: 1,
            size: 3,
            sha256: "ab".repeat(32),
            files: vec!["capture.png".to_owned()],
            created_at: 5_000,
            capture: None,
        },
        created: true,
    }
}

fn provenance(capture_id: &str) -> CaptureProvenance {
    CaptureProvenance {
        capture_id: capture_id.to_owned(),
        station_id: "station-side-name".to_owned(),
        monitor: "DP-1".to_owned(),
        kind: CaptureKind::Screenshot,
        caller: CaptureCaller::Owner,
        caller_station: None,
        caller_session: None,
        caller_grant_id: None,
        at: 5_000,
    }
}

async fn prepare(w: &W, id: &str, hash: &str) -> Prepared {
    w.s.capture_prepare(
        w.st.clone(),
        id.to_owned(),
        hash.to_owned(),
        "DP-1".to_owned(),
    )
    .await
    .unwrap()
}

async fn sql<R: Send + 'static>(
    w: &W,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<R> + Send + 'static,
) -> R {
    super::super::call(&w.s.inner.db, move |conn| Ok(f(conn)?))
        .await
        .unwrap()
}

async fn count(w: &W, query: &'static str) -> i64 {
    sql(w, move |c| c.query_row(query, [], |r| r.get(0))).await
}

/// A capture that the station committed and the deck indexed.
async fn indexed(w: &W, capture_id: &str, artifact_id: &str) {
    assert_eq!(prepare(w, capture_id, "h").await, Prepared::New);
    w.s.capture_attempted(w.st.clone(), capture_id.to_owned())
        .await
        .unwrap();
    let done =
        w.s.complete_capture(
            w.st.clone(),
            capture_id.to_owned(),
            published(artifact_id),
            provenance(capture_id),
        )
        .await
        .unwrap();
    assert_eq!(done, Completed::Indexed);
}

#[test]
fn a_v31_database_migrates_to_v32_and_keeps_its_cache_rows() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = crate::store::schema::migrations();
    migrations.to_version(&mut conn, 31).unwrap();
    conn.execute(
        "INSERT INTO artifact_cache (station_id, artifact_id, version, path, bytes)
         VALUES ('st', 'a1', 1, 'f', x'0102')",
        [],
    )
    .unwrap();
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'desktop_captures'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, 0);
    // V32 only: a later migration (V33, the station palettes of P4) must not
    // turn this into a test of the whole list.
    migrations.to_version(&mut conn, 32).unwrap();
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(user_version, 32);
    let (capture, used): (i64, i64) = conn
        .query_row(
            "SELECT capture, last_used_at FROM artifact_cache WHERE artifact_id = 'a1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((capture, used), (0, 0));
    conn.execute(
        "INSERT INTO desktop_captures
             (station_id, capture_id, action_hash, state, monitor, created_at, updated_at)
         VALUES ('st', 'c1', 'h', 'prepared', 'DP-1', 1, 1)",
        [],
    )
    .unwrap();
    // The key is the station and the capture id.
    assert!(
        conn.execute(
            "INSERT INTO desktop_captures
                 (station_id, capture_id, action_hash, state, monitor, created_at, updated_at)
             VALUES ('st', 'c1', 'h', 'prepared', 'DP-1', 1, 1)",
            [],
        )
        .is_err()
    );
    let version_has_capture = conn
        .prepare("SELECT capture FROM artifact_versions")
        .is_ok();
    assert!(version_has_capture);
}

#[tokio::test]
async fn prepare_inserts_once_and_tells_a_repeat_from_a_conflict() {
    let w = setup().await;
    assert_eq!(prepare(&w, ID1, "h1").await, Prepared::New);
    assert_eq!(prepare(&w, ID1, "h2").await, Prepared::Conflict);
    let Prepared::Existing(row) = prepare(&w, ID1, "h1").await else {
        panic!("a repeat is Existing");
    };
    assert_eq!(row.state, CaptureState::Prepared);
    assert_eq!(row.monitor, "DP-1");
    assert!(!row.indexed && !row.deleted && row.published.is_none());
    // Another station has its own row for the same id.
    let other = StationId::from("st-2".to_owned());
    let got =
        w.s.capture_prepare(other, ID1.to_owned(), "h2".to_owned(), "DP-2".to_owned())
            .await
            .unwrap();
    assert_eq!(got, Prepared::New);
}

#[tokio::test]
async fn the_send_states_move_forward_and_a_committed_row_stays_committed() {
    let w = setup().await;
    prepare(&w, ID1, "h").await;
    // `attempted` needs `prepared`; `unknown` needs `attempted`.
    w.s.capture_unknown(w.st.clone(), ID1.to_owned())
        .await
        .unwrap();
    let state = |w: &W, id: &'static str| {
        let s = w.s.clone();
        let st = w.st.clone();
        async move {
            s.capture_get(st, id.to_owned())
                .await
                .unwrap()
                .unwrap()
                .state
        }
    };
    assert_eq!(state(&w, ID1).await, CaptureState::Prepared);
    w.s.capture_attempted(w.st.clone(), ID1.to_owned())
        .await
        .unwrap();
    assert_eq!(state(&w, ID1).await, CaptureState::Attempted);
    w.s.capture_failed(
        w.st.clone(),
        ID1.to_owned(),
        "busy".to_owned(),
        "A capture is already running.".to_owned(),
    )
    .await
    .unwrap();
    let row =
        w.s.capture_get(w.st.clone(), ID1.to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(row.code.as_deref(), Some("busy"));
    assert_eq!(
        row.message.as_deref(),
        Some("A capture is already running.")
    );
    // A terminal row does not change.
    w.s.capture_unknown(w.st.clone(), ID1.to_owned())
        .await
        .unwrap();
    assert_eq!(state(&w, ID1).await, CaptureState::Failed);
    // A committed row is not failed afterwards.
    indexed(&w, ID2, "A2").await;
    w.s.capture_failed(w.st.clone(), ID2.to_owned(), "busy".into(), "x".into())
        .await
        .unwrap();
    assert_eq!(state(&w, ID2).await, CaptureState::Committed);
}

#[tokio::test]
async fn completion_indexes_the_artifact_the_version_and_the_provenance_together() {
    let w = setup().await;
    indexed(&w, ID1, "A1").await;
    let row =
        w.s.capture_get(w.st.clone(), ID1.to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.state, CaptureState::Committed);
    assert!(row.indexed && !row.deleted);
    // The deck's station id replaces the station side name.
    let stamped = row.published.unwrap();
    assert_eq!(stamped.artifact.station_id, "st-1");
    let capture = stamped.version.capture.clone().unwrap();
    assert_eq!(capture.station_id, "st-1");
    assert_eq!(capture.capture_id, ID1);
    assert_eq!(
        stamped,
        stamp_published(&w.st, published("A1"), &provenance(ID1))
    );
    // The index, the version read and the list show it.
    let list = w.s.artifacts(None, None, None).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].artifact_id, "A1");
    assert_eq!(list[0].station_id, "st-1");
    let versions =
        w.s.artifact_versions(w.st.clone(), "A1".to_owned())
            .await
            .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].capture.as_ref(), Some(&capture));
    // A second completion changes nothing.
    let again =
        w.s.complete_capture(
            w.st.clone(),
            ID1.to_owned(),
            published("A1"),
            provenance(ID1),
        )
        .await
        .unwrap();
    assert_eq!(again, Completed::Indexed);
    assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 1);
    // An unresolved list holds no indexed row.
    assert!(w.s.captures_unresolved().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_publish_never_sets_the_capture_column() {
    let w = setup().await;
    let mut v = published("A9").version;
    v.capture = Some(provenance(ID1));
    w.s.put_artifact_versions(w.st.clone(), vec![v])
        .await
        .unwrap();
    let got =
        w.s.artifact_versions(w.st.clone(), "A9".to_owned())
            .await
            .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].capture, None);
}

#[tokio::test]
async fn a_tombstone_keeps_the_capture_unindexed_and_the_lookup_deleted() {
    let w = setup().await;
    indexed(&w, ID1, "A1").await;
    w.s.artifact_delete(w.st.clone(), "A1".to_owned())
        .await
        .unwrap();
    let row =
        w.s.capture_get(w.st.clone(), ID1.to_owned())
            .await
            .unwrap()
            .unwrap();
    assert!(row.deleted);
    // A new capture of a tombstoned identity (an artifact id that came back).
    prepare(&w, ID2, "h").await;
    w.s.capture_attempted(w.st.clone(), ID2.to_owned())
        .await
        .unwrap();
    let done =
        w.s.complete_capture(
            w.st.clone(),
            ID2.to_owned(),
            published("A1"),
            provenance(ID2),
        )
        .await
        .unwrap();
    assert_eq!(done, Completed::Deleted);
    let row =
        w.s.capture_get(w.st.clone(), ID2.to_owned())
            .await
            .unwrap()
            .unwrap();
    assert!(row.deleted && !row.indexed && row.published.is_some());
    assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 0);
    assert_eq!(count(&w, "SELECT COUNT(*) FROM artifact_versions").await, 0);
    // A deleted row is not asked for again.
    assert!(w.s.captures_unresolved().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_indexing_failure_rolls_back_and_the_kept_result_is_indexed_later() {
    let w = setup().await;
    prepare(&w, ID1, "h").await;
    w.s.capture_attempted(w.st.clone(), ID1.to_owned())
        .await
        .unwrap();
    sql(&w, |c| {
        c.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON artifact_versions
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
    })
    .await;
    let failed =
        w.s.complete_capture(
            w.st.clone(),
            ID1.to_owned(),
            published("A1"),
            provenance(ID1),
        )
        .await;
    assert!(failed.is_err());
    // Nothing of the failed transaction stays.
    assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 0);
    let row =
        w.s.capture_get(w.st.clone(), ID1.to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.state, CaptureState::Attempted);
    assert!(row.published.is_none());
    // The result is kept: committed, not indexed, and still unresolved.
    w.s.capture_keep_published(w.st.clone(), ID1.to_owned(), published("A1"))
        .await
        .unwrap();
    let row =
        w.s.capture_get(w.st.clone(), ID1.to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!((row.state, row.indexed), (CaptureState::Committed, false));
    assert!(row.published.is_some());
    assert_eq!(w.s.captures_unresolved().await.unwrap().len(), 1);
    sql(&w, |c| c.execute_batch("DROP TRIGGER boom;")).await;
    let done =
        w.s.complete_capture(
            w.st.clone(),
            ID1.to_owned(),
            published("A1"),
            provenance(ID1),
        )
        .await
        .unwrap();
    assert_eq!(done, Completed::Indexed);
    assert!(w.s.captures_unresolved().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_unresolved_list_holds_attempted_and_committed_unindexed_rows_only() {
    let w = setup().await;
    prepare(&w, ID1, "h").await;
    prepare(&w, ID2, "h").await;
    w.s.capture_attempted(w.st.clone(), ID2.to_owned())
        .await
        .unwrap();
    prepare(&w, ID3, "h").await;
    w.s.capture_attempted(w.st.clone(), ID3.to_owned())
        .await
        .unwrap();
    w.s.capture_failed(w.st.clone(), ID3.to_owned(), "busy".into(), "x".into())
        .await
        .unwrap();
    let ids: Vec<String> =
        w.s.captures_unresolved()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.capture_id)
            .collect();
    assert_eq!(ids, vec![ID2.to_owned()]);
}

/// A capture version of `artifact_id` with a file of `len` bytes.
async fn cached(w: &W, artifact_id: &str, capture_id: &str, len: usize, limit: u64) {
    indexed(w, capture_id, artifact_id).await;
    w.s.put_artifact_file_with_limit(
        w.st.clone(),
        artifact_id.to_owned(),
        1,
        "capture.png".to_owned(),
        vec![7; len],
        limit,
    )
    .await
    .unwrap();
}

async fn cache_rows(w: &W) -> Vec<String> {
    sql(w, |c| {
        let mut stmt = c.prepare("SELECT artifact_id FROM artifact_cache ORDER BY artifact_id")?;
        stmt.query_map([], |r| r.get(0))?.collect()
    })
    .await
}

#[tokio::test]
async fn the_capture_cache_evicts_the_oldest_use_and_never_an_artifact_or_a_version() {
    let w = setup().await;
    // An ordinary artifact file is never a capture row.
    w.s.put_artifact_versions(
        w.st.clone(),
        vec![{
            let mut v = published("PLAIN").version;
            v.files = vec!["index.html".into()];
            v
        }],
    )
    .await
    .unwrap();
    w.s.put_artifact_file_with_limit(
        w.st.clone(),
        "PLAIN".to_owned(),
        1,
        "index.html".to_owned(),
        vec![1; 100],
        10,
    )
    .await
    .unwrap();
    cached(&w, "A1", ID1, 10, 25).await;
    w.det.advance(Duration::from_secs(1));
    cached(&w, "A2", ID2, 10, 25).await;
    w.det.advance(Duration::from_secs(1));
    // Reading A1 makes it the newest use.
    let got =
        w.s.cached_artifact_file(w.st.clone(), "A1".to_owned(), 1, "capture.png".to_owned())
            .await
            .unwrap();
    assert_eq!(got.map(|b| b.len()), Some(10));
    w.det.advance(Duration::from_secs(1));
    // 30 bytes of capture rows are over 25: A2 (oldest use) goes.
    cached(&w, "A3", ID3, 10, 25).await;
    assert_eq!(cache_rows(&w).await, ["A1", "A3", "PLAIN"]);
    // No artifact and no version row went.
    assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 3);
    assert_eq!(
        count(
            &w,
            "SELECT COUNT(*) FROM artifact_versions WHERE capture IS NOT NULL"
        )
        .await,
        3
    );
    let flags = count(&w, "SELECT COUNT(*) FROM artifact_cache WHERE capture = 1").await;
    assert_eq!(flags, 2);
    // The evicted artifact still reads from the index.
    assert!(
        w.s.artifact(w.st.clone(), "A2".to_owned())
            .await
            .unwrap()
            .is_some()
    );
    let total = count(
        &w,
        "SELECT SUM(length(bytes)) FROM artifact_cache WHERE capture = 1",
    )
    .await;
    assert!(total <= 25, "{total}");
}

#[tokio::test]
async fn a_capture_file_over_the_limit_is_not_cached() {
    let w = setup().await;
    cached(&w, "A1", ID1, 30, 25).await;
    assert!(cache_rows(&w).await.is_empty());
    assert!(
        w.s.artifact(w.st.clone(), "A1".to_owned())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_prepared_row_becomes_unknown_at_deck_start_and_is_never_sent() {
    let w = setup().await;
    assert_eq!(prepare(&w, ID1, "h1").await, Prepared::New);
    assert_eq!(w.s.captures_prepared_to_unknown().await.unwrap(), 1);
    let Prepared::Existing(row) = prepare(&w, ID1, "h1").await else {
        panic!("a repeat is Existing");
    };
    assert_eq!(row.state, CaptureState::Unknown);
    assert!(!row.unresolved());
    assert_eq!(w.s.captures_prepared_to_unknown().await.unwrap(), 0);
}
