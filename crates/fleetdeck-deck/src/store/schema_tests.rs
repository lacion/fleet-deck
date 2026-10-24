#[test]
fn the_migrations_are_valid() {
    super::migrations().validate().unwrap();
}

#[test]
fn an_existing_share_reads_history_off_after_v23() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = super::migrations();
    migrations.to_version(&mut conn, 22).unwrap();
    conn.execute(
        "INSERT INTO shares (share_id, kind, target, role, steering, expires_at, created_at)
         VALUES ('sh1', 'board', '', 'viewer', 'open', 2, 1)",
        [],
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    let history: i64 = conn
        .query_row(
            "SELECT history FROM shares WHERE share_id = 'sh1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(history, 0);
    // The column holds 0 or 1 only.
    assert!(conn.execute("UPDATE shares SET history = 2", []).is_err());
}
