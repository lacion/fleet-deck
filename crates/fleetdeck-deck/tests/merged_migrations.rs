//! The migration order after v2 merged into Fleet Git (DL-232). A deck
//! database made by v2 is at version 25 (v2's display migration); the merged
//! list keeps v2's V1 to V25 and appends the five Fleet Git migrations as V26
//! to V30, and R13 P5a adds V31 (the artifact tombstones) and V32 (the owner
//! screenshots); R13 P4 adds V33 (the station palettes); R13 P6 adds V34 (the
//! grant caps and the coordinator desktop ledger); R13 P5b adds V35 (the
//! recording columns of the owner captures); R13 P5b-2 adds V36 (the recording
//! caps and the rebuilt ledger); R13 P7 adds V37 (the input allowance, the
//! ledger rebuilds, the session addresses). The station list is Fleet Git's
//! list (32 entries), R13 P5a V33 (the artifact deletions) and V34 (the capture
//! operations), R13 P6 V35 (the coordinator desktop operations) and R13 P5b V36
//! (the recording columns and the capture garbage); a station database made by
//! v2 is at version 27.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

const DECK_V2_LAST: usize = 25;
const DECK_LATEST: i64 = 37;
const STATION_V2_LAST: usize = 27;
const STATION_LATEST: i64 = 36;

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
    assert!(op("notify", "keyboard", "prepared").is_err());
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

/// R13 P5b task F: V34 to V35 keeps a screenshot row and reads it as a
/// `screenshot` with the new columns at their defaults; the new checks hold.
#[test]
fn migration_v35_keeps_a_deck_capture_row_as_a_screenshot() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 34).unwrap();
    assert_eq!(user_version(&conn), 34);
    conn.execute(
        "INSERT INTO desktop_captures (station_id, capture_id, action_hash, state, monitor,
             created_at, updated_at)
         VALUES ('st-1', 'c1', 'h', 'committed', 'DP-1', 1, 2)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_LATEST);
    type Row = (String, String, Option<i64>, Option<String>, String, i64);
    let row: Row = conn
        .query_row(
            "SELECT operation_kind, state, requested_seconds, owner_login, stop_intent,
                 audited_terminal
             FROM desktop_captures WHERE capture_id = 'c1'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "screenshot".to_owned(),
            "committed".to_owned(),
            None,
            None,
            "none".to_owned(),
            0
        )
    );
    let insert = |id: &str, kind: &str, seconds: i64| {
        conn.execute(
            "INSERT INTO desktop_captures (station_id, capture_id, action_hash, state, monitor,
                 created_at, updated_at, operation_kind, requested_seconds)
             VALUES ('st-1', ?1, 'h', 'prepared', 'DP-1', 1, 1, ?2, ?3)",
            rusqlite::params![id, kind, seconds],
        )
    };
    assert!(insert("r1", "recording", 120).is_ok());
    assert!(insert("r2", "recording", 0).is_err());
    assert!(insert("r3", "recording", 121).is_err());
    assert!(insert("r4", "video", 5).is_err());
    assert!(
        conn.execute("UPDATE desktop_captures SET stop_intent = 'later'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE desktop_captures SET audited_terminal = 2", [])
            .is_err()
    );
}

/// R13 P5b-2 task Q-F: V35 to V36 gives an existing grant four zero recording
/// counters, keeps an existing operation row through the table rebuild (with
/// `seconds` `NULL`), and accepts `record_monitor` and `seconds` 1 to 120 only.
#[test]
fn migration_v36_keeps_a_grant_at_zero_recording_caps_and_an_op_row_through_the_rebuild() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 35).unwrap();
    assert_eq!(user_version(&conn), 35);
    conn.execute(
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state)
         VALUES ('g1', 'st-1', 'se-1', '[\"capture\"]', '[\"st-1\"]', 3, '{}', 60000, 1, 2,
             'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
             payload_hash, tool, class, binding_id, generation, caller_station,
             caller_session, target_station, action_hash, resource, state, result, code,
             charged, created_at, updated_at)
         VALUES ('op1', 'g1', 'k1', 'h', 'screenshot', 'capture', 'b', 4, 'st-1', 'se-1',
             'st-2', 'ah', '{\"monitor\":\"DP-1\"}', 'done', '{\"x\":1}', NULL,
             '{\"desktop\":1,\"capture\":1}', 10, 11)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_LATEST);
    // The existing grant reads zero recording caps and counters.
    let zeros: (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT recording_cap, recording_used, recording_second_cap, recording_second_used
             FROM tokens WHERE grant_id = 'g1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(zeros, (0, 0, 0, 0));
    for bad in [
        "recording_cap = 5001",
        "recording_cap = -1",
        "recording_used = -1",
        "recording_second_cap = 600001",
        "recording_second_used = -1",
    ] {
        let sql = format!("UPDATE tokens SET {bad} WHERE grant_id = 'g1'");
        assert!(conn.execute(&sql, []).is_err(), "{bad}");
    }
    for ok in ["recording_cap = 5000", "recording_second_cap = 600000"] {
        let sql = format!("UPDATE tokens SET {ok} WHERE grant_id = 'g1'");
        assert_eq!(conn.execute(&sql, []).unwrap(), 1, "{ok}");
    }
    // The existing operation row survived the rebuild, with `seconds` NULL.
    type Op = (
        String,
        String,
        String,
        i64,
        String,
        String,
        i64,
        i64,
        Option<i64>,
    );
    let op: Op = conn
        .query_row(
            "SELECT tool, state, result, generation, target_station, charged, created_at,
                 updated_at, seconds
             FROM coordinator_desktop_ops WHERE operation_id = 'op1'",
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
                    r.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        op,
        (
            "screenshot".to_owned(),
            "done".to_owned(),
            "{\"x\":1}".to_owned(),
            4,
            "st-2".to_owned(),
            "{\"desktop\":1,\"capture\":1}".to_owned(),
            10,
            11,
            None
        )
    );
    // The state index came back, and the old name is gone.
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_master
             WHERE name = 'coordinator_desktop_ops_state' AND tbl_name = 'coordinator_desktop_ops'"
        ),
        1
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name = 'coordinator_desktop_ops_new'"
        ),
        0
    );
    // The rebuilt table takes `record_monitor` with 1 to 120 seconds, and the
    // old checks still hold. One request key for each grant still holds.
    let insert = |id: &str, tool: &str, seconds: Option<i64>| {
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at,
                 updated_at, seconds)
             VALUES (?1, 'g1', ?1, 'h', ?2, 'capture', 'b', 1, 'st-1', 'se-1', 'st-1', '{}',
                 'prepared', '{}', 1, 1, ?3)",
            rusqlite::params![id, tool, seconds],
        )
    };
    assert!(insert("r1", "record_monitor", Some(1)).is_ok());
    assert!(insert("r2", "record_monitor", Some(120)).is_ok());
    assert!(insert("r3", "record_monitor", Some(0)).is_err());
    assert!(insert("r4", "record_monitor", Some(121)).is_err());
    assert!(insert("r5", "notify", None).is_ok());
    assert!(insert("r6", "record_demo", Some(5)).is_err());
    assert!(insert("op1", "notify", None).is_err());
    assert!(
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at,
                 updated_at)
             VALUES ('dup', 'g1', 'k1', 'h', 'notify', 'desktop', 'b', 1, 's', 's', 's', '{}',
                 'prepared', '{}', 1, 1)",
            [],
        )
        .is_err()
    );
}

/// R13 P7 task F: V36 to V37 keeps a grant, an op row (recording `seconds`) and
/// a rate row through the rebuilds, takes the input values, signs sessions out.
#[test]
fn migration_v37_keeps_the_rows_and_signs_every_session_out() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 36).unwrap();
    for sql in [
        "PRAGMA foreign_keys = OFF",
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state)
         VALUES ('g1', 'st-1', 'se-1', '[]', '[\"st-1\"]', 3, '{}', 60000, 1, 2, 'active')",
        "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key, payload_hash,
             tool, class, binding_id, generation, caller_station, caller_session, target_station,
             action_hash, resource, state, result, code, charged, created_at, updated_at, seconds)
         VALUES ('op1', 'g1', 'k1', 'h', 'record_monitor', 'capture', 'b', 4, 's', 's', 't',
             'ah', '{}', 'done', '{}', NULL, '{}', 10, 11, 30)",
        "INSERT INTO coordinator_desktop_rate VALUES ('grant:g1', 'mutation', 5)",
        "INSERT INTO login_sessions VALUES ('s1', 'c', 'password', 1, 1, 99)",
        "INSERT INTO member_sessions VALUES ('m1', 'mem', 'sh', 'c', 1, 99)",
    ] {
        conn.execute(sql, []).unwrap();
    }
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_LATEST);
    let text = |sql: &str| -> String { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    // The grant reads input disabled with zero caps and usage; the checks hold.
    assert_eq!(
        text(
            "SELECT json_array(allow_input, input_scope, input_cap, input_used,
                 input_character_cap, input_character_used) FROM tokens"
        ),
        "[0,null,0,0,0,0]"
    );
    for bad in ["allow_input = 2", "input_scope = 'any'", "input_cap = 5001"] {
        assert!(
            conn.execute(&format!("UPDATE tokens SET {bad}"), [])
                .is_err()
        );
    }
    for bad in ["input_used = -1", "input_character_cap = 1000001"] {
        assert!(
            conn.execute(&format!("UPDATE tokens SET {bad}"), [])
                .is_err()
        );
    }
    let ok = "UPDATE tokens SET allow_input = 1, input_scope = 'own_windows', input_cap = 5000,
        input_character_cap = 1000000, input_character_used = 3";
    assert_eq!(conn.execute(ok, []).unwrap(), 1);
    // The op row, its recording `seconds` included, the index and the rate row.
    assert_eq!(
        text(
            "SELECT json_array(tool, state, generation, created_at, updated_at, seconds,
                 input_length, attempted_at, finished_at, exposure_addrs)
             FROM coordinator_desktop_ops WHERE operation_id = 'op1'"
        ),
        r#"["record_monitor","done",4,10,11,30,null,null,null,null]"#
    );
    assert_eq!(
        text(
            "SELECT group_concat(name) FROM sqlite_master WHERE name IN
             ('coordinator_desktop_ops_state', 'coordinator_desktop_rate_key')
             OR name LIKE '%_new'"
        ),
        "coordinator_desktop_ops_state,coordinator_desktop_rate_key"
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM coordinator_desktop_rate"),
        1
    );
    // The input tools, class and length (0 to 2000); the old checks still hold.
    let insert = |id: &str, tool: &str, class: &str, length: Option<i64>| {
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at,
                 updated_at, input_length)
             VALUES (?1, 'g1', ?1, 'h', ?2, ?3, 'b', 1, 's', 's', 's', '{}', 'prepared', '{}',
                 1, 1, ?4)",
            rusqlite::params![id, tool, class, length],
        )
    };
    assert!(insert("i1", "key_press", "input", Some(1)).is_ok());
    assert!(insert("i2", "type_text", "input", Some(2000)).is_ok());
    assert!(insert("i3", "pointer_move", "input", Some(0)).is_ok());
    assert!(insert("i4", "type_text", "input", Some(2001)).is_err());
    assert!(insert("i5", "pointer_click", "input", None).is_err());
    assert!(insert("op1", "notify", "desktop", None).is_err()); // the key stays unique
    let rate = |bucket: &str| {
        let sql = "INSERT INTO coordinator_desktop_rate VALUES ('grant:g1', ?1, 6)";
        conn.execute(sql, [bucket])
    };
    assert!(rate("input").is_ok() && rate("click").is_err());
    // Every browser session is gone; the new columns exist for the next ones.
    let sessions =
        "SELECT (SELECT count(*) FROM login_sessions) + (SELECT count(*) FROM member_sessions)";
    assert_eq!(count(&conn, sessions), 0);
    for sql in [
        "INSERT INTO login_sessions VALUES ('s2', 'c', 'password', 1, 1, 99, '192.0.2.1', 5)",
        "INSERT INTO member_sessions VALUES ('m2', 'mem', 'sh', 'c', 1, 99, '2001:db8::1', 5)",
    ] {
        conn.execute(sql, []).unwrap();
    }
}

/// R13 P5b task F: V35 to V36 keeps a capture operation row as a
/// `screenshot` with the new columns at their defaults; `capture_garbage`
/// is empty and checks its own area.
#[test]
fn migration_v36_keeps_a_station_capture_op_as_a_screenshot() {
    let migrations = fleetdeck_station::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 35).unwrap();
    assert_eq!(user_version(&conn), 35);
    conn.execute(
        "INSERT INTO capture_ops (deck_fp, capture_id, action_hash, state, monitor,
             artifact_id, reserved_bytes, started_at, updated_at)
         VALUES ('fp', 'c1', 'h', 'committed', 'DP-1', 'a1', 100, 1, 2)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), STATION_LATEST);
    type Row = (String, String, i64, Option<i64>, Option<String>, String);
    let row: Row = conn
        .query_row(
            "SELECT operation_kind, state, reserved_bytes, requested_seconds, phase, stop_intent
             FROM capture_ops WHERE capture_id = 'c1'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "screenshot".to_owned(),
            "committed".to_owned(),
            100,
            None,
            None,
            "none".to_owned()
        )
    );
    let insert = |id: &str, kind: &str, phase: Option<&str>| {
        conn.execute(
            "INSERT INTO capture_ops (deck_fp, capture_id, action_hash, state, monitor,
                 artifact_id, reserved_bytes, started_at, updated_at, operation_kind, phase)
             VALUES ('fp', ?1, 'h', 'running', 'DP-1', ?1, 0, 1, 1, ?2, ?3)",
            rusqlite::params![id, kind, phase],
        )
    };
    assert!(insert("r1", "recording", Some("finalizing")).is_ok());
    assert!(insert("r2", "recording", Some("done")).is_err());
    assert!(insert("r3", "video", None).is_err());
    assert_eq!(count(&conn, "SELECT count(*) FROM capture_garbage"), 0);
    let garbage = |area: &str, bytes: i64| {
        conn.execute(
            "INSERT INTO capture_garbage (area, name, bytes, found_at)
             VALUES (?1, 'n', ?2, 1)",
            rusqlite::params![area, bytes],
        )
    };
    assert!(garbage("staging", 5).is_ok());
    assert!(garbage("artifacts", -1).is_err());
    assert!(garbage("elsewhere", 1).is_err());
    // The key is the area and the name.
    assert!(garbage("staging", 6).is_err());
}

/// A station database at v2's last version (27) migrates to 36, and a fresh
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
