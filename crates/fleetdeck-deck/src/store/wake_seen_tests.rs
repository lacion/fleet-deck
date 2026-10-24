//! Tests of the wake key `(station_id, session_id, request_id)` (V49, G5).

use fleetdeck_testkit::Det;
use rusqlite::Connection;

use super::schema::V49_POSITION;
use super::{WAKE_SEEN_TTL_MS, WakeReserve, reserve};
use crate::store::DeckStore;
use crate::store::schema::migrations;

fn latest() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrations().to_latest(&mut conn).unwrap();
    conn
}

fn r(conn: &mut Connection, station: &str, session: &str, request: &str, now: i64) -> WakeReserve {
    reserve(conn, station, session, request, now).unwrap()
}

#[test]
fn one_request_id_in_two_sessions_is_two_wake_keys() {
    let mut conn = latest();
    assert_eq!(r(&mut conn, "s", "a", "r1", 1), WakeReserve::Reserved);
    assert_eq!(r(&mut conn, "s", "b", "r1", 1), WakeReserve::Reserved);
    assert_eq!(r(&mut conn, "s", "a", "r1", 2), WakeReserve::Seen);
    assert_eq!(r(&mut conn, "t", "a", "r1", 2), WakeReserve::Reserved);
}

#[tokio::test]
async fn the_session_key_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let det = Det::new();
    {
        let s = DeckStore::open(&path, det.clock(), det.ids())
            .await
            .unwrap();
        assert_eq!(
            s.wake_reserve("s", "a", "r1", 1_000).await,
            WakeReserve::Reserved
        );
    }
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    assert_eq!(
        s.wake_reserve("s", "a", "r1", 2_000).await,
        WakeReserve::Seen
    );
    assert_eq!(
        s.wake_reserve("s", "b", "r1", 2_000).await,
        WakeReserve::Reserved
    );
}

#[test]
fn a_v48_row_blocks_its_request_id_in_every_session_until_it_ages_out() {
    let mut conn = Connection::open_in_memory().unwrap();
    let m = migrations();
    m.to_version(&mut conn, V49_POSITION - 1).unwrap();
    let t0 = 1_000_000;
    conn.execute(
        "INSERT INTO wake_seen (station_id, request_id, first_seen_ms) VALUES ('s', 'r1', ?1)",
        [t0],
    )
    .unwrap();
    m.to_latest(&mut conn).unwrap();

    let t = t0 + 1_000;
    assert_eq!(r(&mut conn, "s", "a", "r1", t), WakeReserve::Seen);
    assert_eq!(r(&mut conn, "s", "b", "r1", t), WakeReserve::Seen);
    assert_eq!(r(&mut conn, "t", "a", "r1", t), WakeReserve::Reserved);
    assert_eq!(r(&mut conn, "s", "a", "r2", t), WakeReserve::Reserved);

    // The tombstone is deleted; the key recorded under it stays.
    let later = t0 + WAKE_SEEN_TTL_MS + 1;
    assert_eq!(r(&mut conn, "s", "c", "r1", later), WakeReserve::Reserved);
    assert_eq!(r(&mut conn, "s", "a", "r1", later), WakeReserve::Seen);
}

#[test]
fn an_empty_session_is_never_a_wake_key() {
    let mut conn = latest();
    assert_eq!(r(&mut conn, "s", "", "r9", 1), WakeReserve::Error);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM wake_seen", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
