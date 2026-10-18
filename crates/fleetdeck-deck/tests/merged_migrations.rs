//! The migration order after v2 merged into Fleet Git (DL-232). A deck
//! database made by v2 is at version 25 (v2's display migration); the merged
//! list keeps v2's V1 to V25 and appends the five Fleet Git migrations as V26
//! to V30, and R13 P5a adds V31 (the artifact tombstones) and V32 (the owner
//! screenshots); R13 P4 adds V33 (the station palettes); R13 P6 adds V34 (the
//! grant caps and the coordinator desktop ledger). The station list is Fleet
//! Git's list (32 entries), R13 P5a V33 (the artifact deletions) and V34 (the
//! capture operations), and R13 P6 V35 (the coordinator desktop operations);
//! a station database made by v2 is at version 27.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

const DECK_V2_LAST: usize = 25;
const DECK_LATEST: i64 = 34;
const STATION_V2_LAST: usize = 27;
const STATION_LATEST: i64 = 35;

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

/// R13 P4 task F1: V32 to V33 adds the station palette table and keeps every
/// row of the older tables.
#[test]
fn migration_v33_adds_the_station_palettes_and_keeps_the_rows() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 32).unwrap();
    assert_eq!(user_version(&conn), 32);
    let n = count(
        &conn,
        "SELECT count(*) FROM sqlite_master WHERE name = 'station_palettes'",
    );
    assert_eq!(n, 0, "no palette table at V32");
    conn.execute(
        "INSERT INTO displays (display_id, name, ceiling, created_at, auth_generation)
         VALUES ('d1', 'Wall', 'view', 1, 7)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_LATEST);
    assert_eq!(count(&conn, "SELECT count(*) FROM displays"), 1);
    assert_eq!(count(&conn, "SELECT count(*) FROM station_palettes"), 0);
    conn.execute(
        "INSERT INTO station_palettes (station_id, mode, colors, updated_at)
         VALUES ('st-1', 'dark', '{}', 5)",
        [],
    )
    .unwrap();
    // The table is STRICT, with one row for each station.
    assert!(
        conn.execute(
            "INSERT INTO station_palettes (station_id, mode, colors, updated_at)
             VALUES ('st-1', 'light', '{}', 6)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO station_palettes (station_id, mode, colors, updated_at)
             VALUES ('st-2', 'dark', '{}', 'text')",
            [],
        )
        .is_err()
    );
}

/// R13 P6 task F1: V33 to V34 keeps a grant row and gives it the new columns
/// at their defaults; the new tables are empty and STRICT.
#[test]
fn migration_v34_keeps_a_grant_and_adds_the_desktop_columns_at_their_defaults() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 33).unwrap();
    assert_eq!(user_version(&conn), 33);
    conn.execute(
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state)
         VALUES ('g1', 'st-1', 'se-1', '[\"read\"]', '[\"st-1\"]', 3, '{}', 60000, 1, 2,
             'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO desktop_captures (station_id, capture_id, action_hash, state, monitor,
             created_at, updated_at)
         VALUES ('st-1', 'c1', 'h', 'prepared', 'DP-1', 1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO display_directives (display_id, generation, mode, sender_kind, body,
             created_at)
         VALUES ('d1', 1, 'mission', 'owner', '{}', 1)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_LATEST);
    type Row = (
        String,
        Option<i64>,
        i64,
        Option<i64>,
        i64,
        Option<i64>,
        i64,
        i64,
    );
    let row: Row = conn
        .query_row(
            "SELECT state, desktop_cap, desktop_used, capture_cap, capture_used,
                    image_byte_cap, image_byte_used, desktop_attempts
             FROM tokens WHERE grant_id = 'g1'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row, ("active".to_owned(), None, 0, None, 0, None, 0, 0));
    // The other changed tables keep their row; the new columns are NULL.
    let coordinator: Option<String> = conn
        .query_row(
            "SELECT coordinator FROM desktop_captures WHERE capture_id = 'c1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(coordinator, None);
    let sender: (Option<i64>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT sender_generation, sender_station, sender_session
             FROM display_directives WHERE display_id = 'd1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(sender, (None, None, None));
    for table in [
        "coordinator_desktop_ops",
        "coordinator_desktop_rejects",
        "coordinator_desktop_rate",
    ] {
        assert_eq!(count(&conn, &format!("SELECT count(*) FROM {table}")), 0);
    }
    // The cap columns hold only their ranges, and the byte cap only whole MiB.
    for bad in [
        "desktop_cap = 5001",
        "capture_cap = -1",
        "image_byte_cap = 1048577",
        "image_byte_cap = 1073741825",
        "desktop_used = -1",
    ] {
        let sql = format!("UPDATE tokens SET {bad} WHERE grant_id = 'g1'");
        assert!(conn.execute(&sql, []).is_err(), "{bad}");
    }
    for ok in [
        "desktop_cap = 5000",
        "capture_cap = 0",
        "image_byte_cap = 1073741824",
    ] {
        let sql = format!("UPDATE tokens SET {ok} WHERE grant_id = 'g1'");
        assert_eq!(conn.execute(&sql, []).unwrap(), 1, "{ok}");
    }
    // An operation row takes only a known tool, class and state.
    let op = |tool: &str, class: &str, state: &str| {
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at,
                 updated_at)
             VALUES (?1, 'g1', ?1, 'h', ?2, ?3, 'b', 1, 'st-1', 'se-1', 'st-1', '{}', ?4, '{}',
                 1, 1)",
            rusqlite::params![format!("{tool}{class}{state}"), tool, class, state],
        )
    };
    assert!(op("notify", "desktop", "prepared").is_ok());
    assert!(op("screenshot", "capture", "unknown").is_ok());
    assert!(op("terminal_open", "desktop", "prepared").is_err());
    assert!(op("notify", "input", "prepared").is_err());
    assert!(op("notify", "desktop", "claimed").is_err());
    // One request key for each grant.
    assert!(
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at,
                 updated_at)
             VALUES ('other', 'g1', 'notifydesktopprepared', 'h', 'notify', 'desktop', 'b', 1,
                 's', 's', 's', '{}', 'prepared', '{}', 1, 1)",
            [],
        )
        .is_err()
    );
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

/// R13 P6 task F1: V34 to V35 keeps a capture operation row and gives it the
/// new `coordinator` column at its default; the new ledger is empty and
/// accepts only its own kinds and states.
#[test]
fn migration_v35_keeps_a_capture_op_and_adds_the_desktop_ledger() {
    let migrations = fleetdeck_station::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 34).unwrap();
    assert_eq!(user_version(&conn), 34);
    conn.execute(
        "INSERT INTO capture_ops (deck_fp, capture_id, action_hash, state, monitor,
             artifact_id, reserved_bytes, started_at, updated_at)
         VALUES ('fp', 'c1', 'h', 'committed', 'DP-1', 'a1', 100, 1, 2)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), STATION_LATEST);
    let (state, coordinator): (String, Option<String>) = conn
        .query_row(
            "SELECT state, coordinator FROM capture_ops WHERE capture_id = 'c1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), coordinator), ("committed", None));
    assert_eq!(count(&conn, "SELECT count(*) FROM desktop_ops"), 0);
    let op = |op_id: &str, kind: &str, state: &str| {
        conn.execute(
            "INSERT INTO desktop_ops (deck_fp, operation_id, action_hash, kind, grant_id,
                 caller_station, caller_session, state, claimed_at, updated_at)
             VALUES ('fp', ?1, 'h', ?2, 'g', 's', 'x', ?3, 1, 1)",
            rusqlite::params![op_id, kind, state],
        )
    };
    assert!(op("o1", "notify", "claimed").is_ok());
    assert!(op("o2", "preview_close", "failed").is_ok());
    assert!(op("o3", "screenshot", "claimed").is_err());
    assert!(op("o4", "notify", "running").is_err());
    // The key is the deck and the operation.
    assert!(op("o1", "notify", "done").is_err());
}

/// A station database at v2's last version (27) migrates to 35, and a fresh
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
