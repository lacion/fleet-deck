//! The migration order after v2 merged into Fleet Git (DL-232). A deck
//! database made by v2 is at version 25 (v2's display migration); the merged
//! list keeps v2's V1 to V25 and appends the five Fleet Git migrations as V26
//! to V30, and R13 P5a adds V31 (the artifact tombstones) and V32 (the owner
//! screenshots). The station list
//! is Fleet Git's list (32 entries) and R13 P5a V33 (the artifact
//! deletions); a station database made by v2 is at
//! version 27.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

const DECK_V2_LAST: usize = 25;
const DECK_LATEST: i64 = 32;
const STATION_V2_LAST: usize = 27;
const STATION_LATEST: i64 = 33;

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

/// Every table, index and view as `(type, name, sql)`, sorted.
fn schema_of(conn: &Connection) -> Vec<(String, String, Option<String>)> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A deck database at v2's last version, with rows in the tables that the
/// later migrations change, migrates to the merged list's last version and
/// keeps every row.
#[test]
fn a_deck_database_at_v2_last_version_migrates_to_the_merged_list() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, DECK_V2_LAST).unwrap();
    assert_eq!(user_version(&conn), 25);
    // v2's V25 ran: the display tables have its columns.
    conn.execute(
        "INSERT INTO displays (display_id, name, ceiling, created_at, auth_generation)
         VALUES ('d1', 'Wall', 'view', 1, 7)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO display_links (token_hash, display_id, created_at, expires_at, auth_generation)
         VALUES ('h1', 'd1', 1, 2, 7)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO projects (project_id, name, station_id, path, forge, trackers, created_at)
         VALUES ('p1', 'App', 'st-1', '/work/app', NULL, '[]', 1)",
        [],
    )
    .unwrap();
    // The Fleet Git tables do not exist yet.
    for table in [
        "project_homes",
        "land_queue",
        "session_moves",
        "station_machines",
    ] {
        let n = count(
            &conn,
            &format!("SELECT count(*) FROM sqlite_master WHERE name = '{table}'"),
        );
        assert_eq!(n, 0, "{table} at V25");
    }

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_LATEST);
    // The rows and the v2 columns survive.
    let (generation, name): (i64, String) = conn
        .query_row(
            "SELECT auth_generation, name FROM displays WHERE display_id = 'd1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((generation, name.as_str()), (7, "Wall"));
    assert_eq!(count(&conn, "SELECT count(*) FROM display_links"), 1);
    // v2's tables of V25 still exist.
    for table in ["display_directives", "wake_seen", "wake_state"] {
        count(&conn, &format!("SELECT count(*) FROM {table}"));
    }
    // The Fleet Git tables exist, and the project got its home.
    for table in ["land_queue", "session_moves", "station_machines"] {
        assert_eq!(count(&conn, &format!("SELECT count(*) FROM {table}")), 0);
    }
    assert_eq!(count(&conn, "SELECT count(*) FROM project_homes"), 1);
    let git: String = conn
        .query_row(
            "SELECT git FROM projects WHERE project_id = 'p1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(git, "{}");
}

/// A fresh deck database and a migrated one have the same schema.
#[test]
fn a_fresh_deck_database_and_a_migrated_one_have_the_same_schema() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut fresh = Connection::open_in_memory().unwrap();
    migrations.to_latest(&mut fresh).unwrap();
    assert_eq!(user_version(&fresh), DECK_LATEST);

    let mut migrated = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut migrated, DECK_V2_LAST).unwrap();
    migrations.to_latest(&mut migrated).unwrap();
    assert_eq!(user_version(&migrated), DECK_LATEST);

    assert_eq!(schema_of(&fresh), schema_of(&migrated));
}

/// A station database at v2's last version (27) migrates to 33, and a fresh
/// one has the same schema.
#[test]
fn a_station_database_at_v2_last_version_migrates_to_the_merged_list() {
    let migrations = fleetdeck_station::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, STATION_V2_LAST).unwrap();
    assert_eq!(user_version(&conn), 27);
    conn.execute(
        "INSERT INTO meta (id, incarnation_id, station_seq_next, created_at)
         VALUES (1, 'inc-1', 5, 1)",
        [],
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), STATION_LATEST);
    let (incarnation, next): (String, i64) = conn
        .query_row(
            "SELECT incarnation_id, station_seq_next FROM meta WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((incarnation.as_str(), next), ("inc-1", 5));

    let mut fresh = Connection::open_in_memory().unwrap();
    migrations.to_latest(&mut fresh).unwrap();
    assert_eq!(user_version(&fresh), STATION_LATEST);
    assert_eq!(schema_of(&fresh), schema_of(&conn));
}
