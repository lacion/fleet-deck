//! Knobs pre-PR round 1: the deck migration V42 adds `idle_until` to
//! `login_sessions` and `display_sessions`. An existing row gets `last_seen`
//! plus the idle limit that was in force (7 days for a login, 30 days for a
//! display session); a database that is new has the columns too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

const DECK_BEFORE: usize = 41;
const DECK_AFTER: i64 = 53;
const LOGIN_IDLE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const DISPLAY_IDLE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn idle_until(conn: &Connection, table: &str, hash: &str) -> i64 {
    conn.query_row(
        &format!("SELECT idle_until FROM {table} WHERE session_hash = ?1"),
        [hash],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn migration_v42_gives_an_existing_session_its_idle_deadline() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, DECK_BEFORE).unwrap();
    assert_eq!(user_version(&conn), 41);
    for sql in [
        "INSERT INTO displays (display_id, name, ceiling, created_at) VALUES ('d', 'Wall', '[]', 1)",
        "INSERT INTO login_sessions (session_hash, csrf_token, method, created_at, last_seen, expires_at)
         VALUES ('l1', 'c', 'password', 1000, 5000, 99999999999)",
        "INSERT INTO login_sessions (session_hash, csrf_token, method, created_at, last_seen, expires_at)
         VALUES ('l2', 'c', 'link', 1000, 8000, 99999999999)",
        "INSERT INTO display_sessions (session_hash, display_id, created_at, last_seen, expires_at)
         VALUES ('d1', 'd', 1000, 7000, 99999999999)",
    ] {
        conn.execute(sql, []).unwrap();
    }
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_AFTER);
    assert_eq!(
        idle_until(&conn, "login_sessions", "l1"),
        5000 + LOGIN_IDLE_MS
    );
    assert_eq!(
        idle_until(&conn, "login_sessions", "l2"),
        8000 + LOGIN_IDLE_MS
    );
    assert_eq!(
        idle_until(&conn, "display_sessions", "d1"),
        7000 + DISPLAY_IDLE_MS
    );
}

#[test]
fn a_new_database_has_the_idle_deadline_columns() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), DECK_AFTER);
    for table in ["login_sessions", "display_sessions"] {
        let columns: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM pragma_table_info('{table}')
                     WHERE name = 'idle_until' AND \"notnull\" = 1 AND dflt_value = '0'"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1, "{table}");
    }
}
