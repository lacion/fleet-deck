//! The V44 migration (DL-432, B13): the station's own proof of an unverified
//! exit. Forward only, like V23; no row is rewritten.

pub(super) const V44: &str = "
-- DL-432 (B13): the station's own proof of an unverified exit. NULL or
-- 'checked_after_exit'; next to the row's state, reason and owner proof.
ALTER TABLE shims ADD COLUMN station_proof TEXT
    CHECK (station_proof IS NULL OR station_proof = 'checked_after_exit');
ALTER TABLE shims ADD COLUMN station_proof_at INTEGER;
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use fleetdeck_core::db::{Db, DbError};
    use rusqlite::Connection;

    use super::super::migrations::{migrations, migrations_upto};

    fn shim(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, provider, provider_instance, cwd, state,
                 offline_approval_policy, summary, created_at, updated_at)
             VALUES ('s', 'claude', 'claude', '/w', 'exited', 'wait', '{}', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO shims (shim_id, session_id, incarnation, state, socket_path,
                 created_at, updated_at)
             VALUES (?1, 's', 'i', 'exited', '/s', 1, 1)",
            [id],
        )
        .unwrap();
    }

    #[test]
    fn v44_adds_the_station_proof_columns() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 43).unwrap();
        shim(&conn, "old");
        migrations.to_version(&mut conn, 44).unwrap();
        let (proof, at): (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT station_proof, station_proof_at FROM shims WHERE shim_id = 'old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((proof, at), (None, None), "no row is rewritten");
        conn.execute(
            "UPDATE shims SET station_proof = 'checked_after_exit', station_proof_at = 5",
            [],
        )
        .unwrap();
        assert!(
            conn.execute("UPDATE shims SET station_proof = 'other'", [])
                .is_err(),
            "the CHECK refuses another value"
        );
    }

    #[test]
    fn v44_database_is_too_new_for_the_v43_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("station.db");
        drop(Db::open(&path, migrations()).unwrap());
        let Err(err) = Db::open(&path, migrations_upto(43)) else {
            panic!("the V43 list opened a V44 database");
        };
        assert!(matches!(err, DbError::TooNew { found: 44, .. }), "{err:?}");
    }
}
