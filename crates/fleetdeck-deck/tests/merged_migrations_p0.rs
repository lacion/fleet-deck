//! MCP P0 §3.4: the deck migration V40 adds the grant of a coordinator
//! receipt. A receipt row of a V39 database keeps its values and reads
//! `NULL` for `grant_id`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::Connection;

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn has_grant_column(conn: &Connection) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('coordinator_receipts')
             WHERE name = 'grant_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    n == 1
}

#[test]
fn migration_v40_adds_the_receipt_grant_and_old_rows_read_null() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 39).unwrap();
    assert_eq!(user_version(&conn), 39);
    assert!(!has_grant_column(&conn));
    conn.execute(
        "INSERT INTO coordinator_receipts
             (command_id, station_id, method, caller, payload_sha256, receipt, at)
         VALUES ('k1', 's1', 'coordinator.call', 's1/coord-1', 'sha', '{}', 5)",
        [],
    )
    .unwrap();

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(user_version(&conn), 42);
    assert!(has_grant_column(&conn));
    let (caller, grant): (String, Option<String>) = conn
        .query_row(
            "SELECT caller, grant_id FROM coordinator_receipts WHERE command_id = 'k1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(caller, "s1/coord-1");
    assert_eq!(grant, None);
}
