//! BG-3: the deck migration V39 adds the setup report tables and keeps every
//! row of the older tables. The tables are empty after the migration, hold
//! only the validated fields, and refuse a value of the wrong type.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn migration_v39_adds_the_provision_report_tables_and_keeps_the_rows() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 38).unwrap();
    assert_eq!(user_version(&conn), 38);
    for table in ["provision_reports", "provision_retries"] {
        let n = count(
            &conn,
            &format!("SELECT count(*) FROM sqlite_master WHERE name = '{table}'"),
        );
        assert_eq!(n, 0, "{table} at V38");
    }
    conn.execute(
        "INSERT INTO audit (at, actor, action, target, detail, result)
         VALUES (1, 'owner', 'pairing.window_open', '', 'kept', 'ok')",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), 52);
    assert_eq!(count(&conn, "SELECT count(*) FROM audit"), 1);
    for table in ["provision_reports", "provision_retries"] {
        assert_eq!(count(&conn, &format!("SELECT count(*) FROM {table}")), 0);
    }

    // A report and its permission store; a station row is not made.
    conn.execute(
        "INSERT INTO provision_reports (fingerprint, terminal_id, state_digest, code, phase,
             name, received_at, last_contact, updated_at)
         VALUES ('f', 't', 'd', 'denied', 'link', 'desk', 1, 2, 3)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO provision_retries (fingerprint, retry_id, terminal_id, state_digest,
             request_id, created_at, expires_at, delivered_at)
         VALUES ('f', 'r', 't', 'd', 'req', 1, 2, NULL)",
        [],
    )
    .unwrap();
    assert_eq!(count(&conn, "SELECT count(*) FROM stations"), 0);
    // One latest report for each key.
    assert!(
        conn.execute(
            "INSERT INTO provision_reports (fingerprint, terminal_id, state_digest, code,
                 phase, name, received_at, last_contact, updated_at)
             VALUES ('f', 't', 'd', 'denied', 'link', 'x', 1, 2, 3)",
            [],
        )
        .is_err()
    );
    // The tables are STRICT: a text value in a time column is refused.
    assert!(
        conn.execute(
            "INSERT INTO provision_reports (fingerprint, terminal_id, state_digest, code,
                 phase, name, received_at, last_contact, updated_at)
             VALUES ('g', 't', 'd', 'denied', 'link', 'x', 'soon', 2, 3)",
            [],
        )
        .is_err()
    );
}
