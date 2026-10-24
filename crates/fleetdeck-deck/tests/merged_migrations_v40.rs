//! Knobs K4: the recording length can be up to 3600 seconds. The deck
//! migration V41 rebuilds `desktop_captures` and `coordinator_desktop_ops`;
//! the station migration V40 rebuilds `capture_ops`. (The deck list numbers it
//! V41 because V40, MCP P0, was already taken.) A database that is migrated
//! from the version before keeps every row unchanged, takes 121 and 3600
//! seconds and refuses 3601; a fresh database does the same.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;
use rusqlite::types::Value;

const DECK_BEFORE: usize = 40;
const DECK_AFTER: i64 = 47;
const STATION_BEFORE: usize = 39;
const STATION_AFTER: i64 = 44;

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

/// Every row of `table`, ordered by `order`, as values.
fn rows(conn: &Connection, table: &str, order: &str) -> Vec<Vec<Value>> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
        .unwrap();
    let width = stmt.column_count();
    stmt.query_map([], |r| (0..width).map(|i| r.get::<_, Value>(i)).collect())
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The columns of `table` as `(name, type, notnull, default, pk)`, in order.
fn columns(conn: &Connection, table: &str) -> Vec<(String, String, i64, Option<String>, i64)> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info('{table}')"
        ))
        .unwrap();
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// The names of the indexes of `table` that the schema list made.
fn indexes(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL ORDER BY name",
        )
        .unwrap();
    stmt.query_map([table], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

// ------------------------------------------------------------------ deck

fn deck_capture(conn: &Connection, id: &str, seconds: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO desktop_captures (station_id, capture_id, action_hash, state, monitor,
             created_at, updated_at, operation_kind, requested_seconds, owner_login, link_epoch,
             coordinator)
         VALUES ('st-1', ?1, 'h', 'prepared', 'DP-1', 1, 2, 'recording', ?2, 'login', 'ep', 'ctx')",
        rusqlite::params![id, seconds],
    )
}

fn deck_op(conn: &Connection, id: &str, seconds: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
             payload_hash, tool, class, binding_id, generation, caller_station,
             caller_session, target_station, resource, state, charged, created_at,
             updated_at, seconds)
         VALUES (?1, 'g1', ?1, 'h', 'record_monitor', 'capture', 'b', 1, 'st-1', 'se-1',
             'st-1', '{}', 'prepared', '{}', 1, 2, ?2)",
        rusqlite::params![id, seconds],
    )
}

fn assert_deck_limits(conn: &Connection) {
    assert!(deck_capture(conn, "n121", 121).is_ok());
    assert!(deck_capture(conn, "n3600", 3600).is_ok());
    assert!(deck_capture(conn, "n3601", 3601).is_err());
    assert!(deck_capture(conn, "n0", 0).is_err());
    assert!(deck_op(conn, "o121", 121).is_ok());
    assert!(deck_op(conn, "o3600", 3600).is_ok());
    assert!(deck_op(conn, "o3601", 3601).is_err());
    assert!(deck_op(conn, "o0", 0).is_err());
    // The unique key of a grant and a request key still holds.
    assert!(deck_op(conn, "o121", 5).is_err());
}

#[test]
fn deck_v41_keeps_the_rows_and_takes_up_to_3600_seconds() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, DECK_BEFORE).unwrap();
    assert_eq!(user_version(&conn), 40);
    // At V40 a recording of 121 seconds is refused.
    assert!(deck_capture(&conn, "old121", 121).is_err());
    assert!(deck_op(&conn, "old121", 121).is_err());
    for (id, seconds) in [("c60", 60), ("c120", 120)] {
        deck_capture(&conn, id, seconds).unwrap();
    }
    for (id, seconds) in [("o60", 60), ("o120", 120)] {
        deck_op(&conn, id, seconds).unwrap();
    }
    conn.execute(
        "INSERT INTO desktop_captures (station_id, capture_id, action_hash, state, monitor,
             created_at, updated_at)
         VALUES ('st-1', 'shot', 'h', 'committed', 'DP-1', 1, 2)",
        [],
    )
    .unwrap();
    let captures = rows(&conn, "desktop_captures", "capture_id");
    let ops = rows(&conn, "coordinator_desktop_ops", "operation_id");
    let capture_columns = columns(&conn, "desktop_captures");
    let op_columns = columns(&conn, "coordinator_desktop_ops");
    let capture_indexes = indexes(&conn, "desktop_captures");
    let op_indexes = indexes(&conn, "coordinator_desktop_ops");

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_AFTER);
    assert_eq!(rows(&conn, "desktop_captures", "capture_id"), captures);
    assert_eq!(rows(&conn, "coordinator_desktop_ops", "operation_id"), ops);
    assert_eq!(columns(&conn, "desktop_captures"), capture_columns);
    assert_eq!(columns(&conn, "coordinator_desktop_ops"), op_columns);
    assert_eq!(indexes(&conn, "desktop_captures"), capture_indexes);
    assert_eq!(indexes(&conn, "coordinator_desktop_ops"), op_indexes);
    assert_eq!(capture_indexes, ["desktop_captures_recording"]);
    assert_eq!(op_indexes, ["coordinator_desktop_ops_state"]);
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name LIKE '%\\_new' ESCAPE '\\'"
        ),
        0
    );
    assert_deck_limits(&conn);
}

#[test]
fn a_fresh_deck_database_takes_up_to_3600_seconds() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_AFTER);
    assert_deck_limits(&conn);
}

// --------------------------------------------------------------- station

fn station_capture(conn: &Connection, id: &str, seconds: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO capture_ops (deck_fp, capture_id, action_hash, state, monitor, artifact_id,
             reserved_bytes, started_at, updated_at, operation_kind, requested_seconds,
             link_epoch, phase, stop_intent, coordinator)
         VALUES ('fp', ?1, 'h', 'running', 'DP-1', ?1, 100, 1, 2, 'recording', ?2, 'ep',
             'recording', 'none', 'ctx')",
        rusqlite::params![id, seconds],
    )
}

fn assert_station_limits(conn: &Connection) {
    assert!(station_capture(conn, "n121", 121).is_ok());
    assert!(station_capture(conn, "n3600", 3600).is_ok());
    assert!(station_capture(conn, "n3601", 3601).is_err());
    assert!(station_capture(conn, "n0", 0).is_err());
    // The other checks and the primary key still hold.
    assert!(station_capture(conn, "n121", 5).is_err());
    assert!(
        conn.execute("UPDATE capture_ops SET phase = 'later'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE capture_ops SET stop_intent = 'later'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE capture_ops SET operation_kind = 'video'", [])
            .is_err()
    );
}

#[test]
fn station_v40_keeps_the_rows_and_takes_up_to_3600_seconds() {
    let migrations = fleetdeck_station::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, STATION_BEFORE).unwrap();
    assert_eq!(user_version(&conn), 39);
    assert!(station_capture(&conn, "old121", 121).is_err());
    for (id, seconds) in [("c60", 60), ("c120", 120)] {
        station_capture(&conn, id, seconds).unwrap();
    }
    conn.execute(
        "INSERT INTO capture_ops (deck_fp, capture_id, action_hash, state, monitor, artifact_id,
             reserved_bytes, started_at, updated_at)
         VALUES ('fp', 'shot', 'h', 'committed', 'DP-1', 'a1', 100, 1, 2)",
        [],
    )
    .unwrap();
    let before = rows(&conn, "capture_ops", "capture_id");
    let before_columns = columns(&conn, "capture_ops");
    let before_indexes = indexes(&conn, "capture_ops");

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), STATION_AFTER);
    assert_eq!(rows(&conn, "capture_ops", "capture_id"), before);
    assert_eq!(columns(&conn, "capture_ops"), before_columns);
    assert_eq!(indexes(&conn, "capture_ops"), before_indexes);
    assert_eq!(before_indexes, ["capture_ops_artifact"]);
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name = 'capture_ops_v40'"
        ),
        0
    );
    assert_station_limits(&conn);
}

#[test]
fn a_fresh_station_database_takes_up_to_3600_seconds() {
    let migrations = fleetdeck_station::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), STATION_AFTER);
    assert_station_limits(&conn);
}
