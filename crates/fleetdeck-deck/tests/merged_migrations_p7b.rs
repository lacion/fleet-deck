//! R13 P7b task F: the deck migration V38 (the click allowance and the
//! `any_window` scope of a grant, the `pointer_click` tool and the
//! `window_list` bucket) keeps every row, passes the foreign-key check and
//! rebuilds `tokens` with the extended checks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

const GRANT_COLUMNS: &str = "json_array(grant_id, station_id, session_id, classes, stations,
    spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason,
    bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at,
    renew_generation, start_command_id, scope, answer_cap, answer_used, desktop_cap, desktop_used,
    capture_cap, capture_used, image_byte_cap, image_byte_used, desktop_attempts, recording_cap,
    recording_used, recording_second_cap, recording_second_used, allow_input, input_scope,
    input_cap, input_used, input_character_cap, input_character_used)";

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn text(conn: &Connection, sql: &str) -> String {
    conn.query_row(sql, [], |r| match r.get_ref(0)? {
        rusqlite::types::ValueRef::Integer(n) => Ok(n.to_string()),
        other => Ok(other.as_str()?.to_owned()),
    })
    .unwrap()
}

fn foreign_key_violations(conn: &Connection) -> i64 {
    conn.prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |_| Ok(()))
        .unwrap()
        .count()
        .try_into()
        .unwrap()
}

/// A deck database at V37 with an input grant, its lineage row, an input
/// operation and a rate row, with foreign keys on as in a real migration.
fn at_v37() -> Connection {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
    migrations.to_version(&mut conn, 37).unwrap();
    for sql in [
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state, bound_generation,
             bound_native_id, bound_provider_instance, binding_id, last_used_at, renew_generation,
             start_command_id, scope, answer_cap, answer_used, desktop_cap, desktop_used,
             capture_cap, capture_used, image_byte_cap, image_byte_used, desktop_attempts,
             recording_cap, recording_used, recording_second_cap, recording_second_used,
             allow_input, input_scope, input_cap, input_used, input_character_cap,
             input_character_used)
         VALUES ('g1', 'st-1', 'se-1', '[\"desktop\",\"capture\",\"input\"]', '[\"st-1\"]', 3,
             '{}', 60000, 1, 2, 'active', 4, 'native', 'claude', 'b1', 7, 5, 'cmd-1', 'fleet', 10,
             1, 300, 9, 20, 8, 67108864, 2097152, 12, 5, 2, 600, 41, 1, 'own_windows', 100, 6,
             20000, 321)",
        "INSERT INTO coordinator_lineage (command_id, grant_id, station_id, session_id,
             start_outbox_id, created_at)
         VALUES ('cmd-1', 'g1', 'st-1', 'se-1', 'ob-1', 3)",
        "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key, payload_hash,
             tool, class, binding_id, generation, caller_station, caller_session, target_station,
             action_hash, resource, state, result, code, charged, created_at, updated_at, seconds,
             input_length, attempted_at, finished_at, exposure_addrs)
         VALUES ('op1', 'g1', 'k1', 'h', 'type_text', 'input', 'b1', 4, 's', 's', 't', 'ah',
             '{}', 'done', '{}', NULL, '{\"input\":1}', 10, 11, NULL, 12, 13, 14,
             '[\"192.0.2.7\"]')",
        "INSERT INTO coordinator_desktop_rate VALUES ('grant:g1', 'input', 5)",
    ] {
        conn.execute(sql, []).unwrap();
    }
    assert_eq!(foreign_key_violations(&conn), 0);
    conn
}

#[test]
fn migration_v38_keeps_every_row_and_adds_a_zero_click_allowance() {
    let mut conn = at_v37();
    let before = text(&conn, &format!("SELECT {GRANT_COLUMNS} FROM tokens"));
    let migrations = fleetdeck_deck::store::schema::migrations();
    migrations.to_version(&mut conn, 38).unwrap();
    assert_eq!(user_version(&conn), 38);
    // Every value of the grant reads back, and the click allowance is 0.
    assert_eq!(
        text(&conn, &format!("SELECT {GRANT_COLUMNS} FROM tokens")),
        before
    );
    assert_eq!(text(&conn, "SELECT allow_click FROM tokens"), "0");
    assert_eq!(
        text(
            &conn,
            "SELECT json_array(allow_input, input_scope, input_cap, input_used,
                 input_character_cap, input_character_used, allow_click) FROM tokens"
        ),
        r#"[1,"own_windows",100,6,20000,321,0]"#
    );
    // The foreign-key check is empty, and the lineage row still joins its grant.
    assert_eq!(foreign_key_violations(&conn), 0);
    assert_eq!(
        text(
            &conn,
            "SELECT t.grant_id || '/' || l.command_id FROM coordinator_lineage l
             JOIN tokens t ON t.grant_id = l.grant_id"
        ),
        "g1/cmd-1"
    );
    // The foreign key is still enforced against the rebuilt table.
    assert!(
        conn.execute(
            "INSERT INTO coordinator_lineage (command_id, grant_id, station_id, created_at)
             VALUES ('cmd-2', 'missing', 'st-1', 3)",
            [],
        )
        .is_err()
    );
    // The operation row, its input columns and the rate row are kept.
    assert_eq!(
        text(
            &conn,
            "SELECT json_array(tool, class, state, generation, created_at, updated_at, seconds,
                 input_length, attempted_at, finished_at, exposure_addrs, charged)
             FROM coordinator_desktop_ops WHERE operation_id = 'op1'"
        ),
        r#"["type_text","input","done",4,10,11,null,12,13,14,"[\"192.0.2.7\"]","{\"input\":1}"]"#
    );
    assert_eq!(
        text(
            &conn,
            "SELECT json_array(scope_key, bucket, at) FROM coordinator_desktop_rate"
        ),
        r#"["grant:g1","input",5]"#
    );
    // The indexes and the unique constraints are back, and no table of the
    // rebuild is left behind.
    assert_eq!(
        text(
            &conn,
            "SELECT group_concat(name) FROM (SELECT name FROM sqlite_master
             WHERE name IN ('tokens_by_caller', 'tokens_by_start_command',
                 'coordinator_desktop_ops_state', 'coordinator_desktop_rate_key')
             ORDER BY name)"
        ),
        "coordinator_desktop_ops_state,coordinator_desktop_rate_key,tokens_by_caller,tokens_by_start_command"
    );
    assert_eq!(
        text(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name LIKE '%\\_new' ESCAPE '\\'"
        ),
        "0"
    );
    assert_eq!(
        text(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name = 'tokens_v37_copy'"
        ),
        "0",
        "the copy table is gone"
    );
    assert!(
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at, updated_at)
             VALUES ('op2', 'g1', 'k1', 'h', 'notify', 'desktop', 'b', 1, 's', 's', 't', '{}',
                 'prepared', '{}', 1, 1)",
            [],
        )
        .is_err(),
        "the request key stays unique per grant"
    );
    assert!(
        conn.execute(
            "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
                 max_modes, idle_ttl_ms, created_at, expires_at, state, start_command_id)
             VALUES ('g2', 'st-1', 'se-2', '[]', '[]', 3, '{}', 60000, 1, 2, 'active', 'cmd-1')",
            [],
        )
        .is_err(),
        "the start command stays unique"
    );
}

#[test]
fn migration_v38_accepts_any_window_the_click_and_the_list_bucket_and_refuses_the_rest() {
    let mut conn = at_v37();
    fleetdeck_deck::store::schema::migrations()
        .to_version(&mut conn, 38)
        .unwrap();
    // `any_window` is a scope now; `any` and every other name is refused.
    assert_eq!(
        conn.execute(
            "UPDATE tokens SET input_scope = 'any_window', allow_click = 1",
            []
        )
        .unwrap(),
        1
    );
    assert_eq!(
        text(
            &conn,
            "SELECT json_array(input_scope, allow_click) FROM tokens"
        ),
        r#"["any_window",1]"#
    );
    for bad in [
        "input_scope = 'any'",
        "input_scope = 'AnyWindow'",
        "input_scope = ''",
        "allow_click = 2",
        "allow_click = -1",
    ] {
        assert!(
            conn.execute(&format!("UPDATE tokens SET {bad}"), [])
                .is_err(),
            "{bad}"
        );
    }
    // A new grant row with the scope and the click allowance can be inserted.
    conn.execute(
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state, allow_input, input_scope,
             input_cap, input_character_cap, allow_click)
         VALUES ('g2', 'st-1', 'se-2', '[]', '[]', 3, '{}', 60000, 1, 2, 'active', 1,
             'any_window', 5, 6, 1)",
        [],
    )
    .unwrap();
    // A `pointer_click` operation and a `window_list` rate row can be
    // inserted; another tool or bucket is still refused.
    conn.execute(
        "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key, payload_hash,
             tool, class, binding_id, generation, caller_station, caller_session, target_station,
             resource, state, charged, created_at, updated_at, input_length)
         VALUES ('op-click', 'g1', 'k-click', 'h', 'pointer_click', 'input', 'b1', 4, 's', 's',
             't', '{}', 'prepared', '{}', 1, 1, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO coordinator_desktop_rate VALUES ('grant:g1', 'window_list', 9)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO coordinator_desktop_ops (operation_id, grant_id, request_key,
                 payload_hash, tool, class, binding_id, generation, caller_station,
                 caller_session, target_station, resource, state, charged, created_at, updated_at)
             VALUES ('op-bad', 'g1', 'k-bad', 'h', 'scroll', 'input', 'b1', 4, 's', 's', 't',
                 '{}', 'prepared', '{}', 1, 1)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO coordinator_desktop_rate VALUES ('grant:g1', 'window_lists', 9)",
            [],
        )
        .is_err()
    );
    assert_eq!(foreign_key_violations(&conn), 0);
}
