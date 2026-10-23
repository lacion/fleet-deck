//! The V49 migration (G5, design `fleet-gaps.md` §3): the wake key has
//! the session, because request ids are per session (DL-402). An old row
//! has no session: it is kept as a legacy tombstone with the session
//! [`LEGACY_SESSION`] (`''`). A tombstone blocks its request id in every
//! session of its station until it ages out (24 hours, or the eviction of
//! a saturated table), so a request that an earlier deck reserved never
//! wakes again (the replay protection of V25 survives the upgrade).

/// The session of a legacy tombstone (a V48 row). A real session id is
/// never empty: `wake_reserve` refuses an empty session.
pub(in crate::store) const LEGACY_SESSION: &str = "";

/// The list position of V49 in `migrations()`: 48 until wall-control V48 is
/// merged into `v2`, then 49 (the architect changes it at the merge).
#[cfg(test)]
pub(in crate::store) const V49_POSITION: usize = 48;

/// The V49 migration.
pub(in crate::store) const SQL: &str = "
CREATE TABLE wake_seen_v49 (
    station_id     TEXT    NOT NULL,
    session_id     TEXT    NOT NULL,
    request_id     TEXT    NOT NULL,
    first_seen_ms  INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id, request_id)
) STRICT;
INSERT INTO wake_seen_v49 (station_id, session_id, request_id, first_seen_ms)
    SELECT station_id, '', request_id, first_seen_ms FROM wake_seen;
DROP TABLE wake_seen;
ALTER TABLE wake_seen_v49 RENAME TO wake_seen;
CREATE INDEX wake_seen_first_seen ON wake_seen(first_seen_ms);
";

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::V49_POSITION;
    use crate::store::schema::migrations;

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn v49_keeps_the_old_rows_as_legacy_tombstones_and_keys_by_session() {
        let mut conn = Connection::open_in_memory().unwrap();
        let m = migrations();
        m.to_version(&mut conn, V49_POSITION - 1).unwrap();
        conn.execute(
            "INSERT INTO wake_seen (station_id, request_id, first_seen_ms) VALUES ('s', 'r1', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE wake_state SET suspended_until_ms = 7 WHERE id = 1",
            [],
        )
        .unwrap();
        m.to_version(&mut conn, V49_POSITION).unwrap();

        assert_eq!(count(&conn, "SELECT count(*) FROM wake_seen"), 1);
        let row: (String, String, String, i64) = conn
            .query_row(
                "SELECT station_id, session_id, request_id, first_seen_ms FROM wake_seen",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, ("s".into(), String::new(), "r1".into(), 5));
        assert_eq!(
            count(
                &conn,
                "SELECT suspended_until_ms FROM wake_state WHERE id = 1"
            ),
            7
        );
        let insert = "INSERT INTO wake_seen (station_id, session_id, request_id, first_seen_ms)
                      VALUES ('s', ?1, 'r1', ?2)";
        conn.execute(insert, params!["a", 1]).unwrap();
        conn.execute(insert, params!["b", 1]).unwrap();
        assert!(conn.execute(insert, params!["a", 2]).is_err());
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM sqlite_master WHERE name = 'wake_seen_first_seen'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM sqlite_master WHERE name = 'wake_seen_v49'"
            ),
            0
        );
    }
}
