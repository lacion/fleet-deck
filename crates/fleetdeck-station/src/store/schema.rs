//! `station.db` migrations (PLAN §10). The migration list and its SQL moved to
//! `migrations.rs` for the module size rule (T1.0); [`migrations`] is
//! re-exported here, so every caller keeps its path.

pub use super::migrations::migrations;

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::migrations;

    fn insert(conn: &Connection, id: &str, op: &str, state: &str, detail: Option<&str>) {
        conn.execute(
            "INSERT INTO shell_effects
               (effect_id, command_id, shell_id, op, change, state, detail, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, '{}', ?5, ?6, 5, 6)",
            params![
                id,
                format!("cmd-{id}"),
                format!("sh-{id}"),
                op,
                state,
                detail
            ],
        )
        .unwrap();
    }

    fn rows(conn: &Connection) -> Vec<(String, String, String, String, Option<String>)> {
        conn.prepare(
            "SELECT effect_id, command_id, op, state, detail FROM shell_effects
             ORDER BY effect_id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    #[test]
    fn v18_keeps_the_shell_effects_and_admits_the_handover_op() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = migrations();
        migrations.to_version(&mut conn, 17).unwrap();
        insert(&conn, "a", "open", "queued", None);
        insert(&conn, "b", "resize", "claimed", None);
        insert(&conn, "c", "close", "confirmed", None);
        insert(&conn, "d", "open", "failed", Some("no pty"));
        insert(
            &conn,
            "e",
            "open",
            "retired_not_dispatched",
            Some("station_restart"),
        );
        let before = rows(&conn);
        assert_eq!(before.len(), 5);

        migrations.to_latest(&mut conn).unwrap();
        assert_eq!(rows(&conn), before);
        let (command, shell, created, updated): (String, String, i64, i64) = conn
            .query_row(
                "SELECT command_id, shell_id, created_at, updated_at FROM shell_effects
                 WHERE effect_id = 'a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (command.as_str(), shell.as_str(), created, updated),
            ("cmd-a", "sh-a", 5, 6)
        );

        insert(&conn, "f", "handover", "queued", None);
        assert!(
            conn.execute(
                "INSERT INTO shell_effects
                   (effect_id, command_id, shell_id, op, change, state, created_at, updated_at)
                 VALUES ('g', 'cmd-g', 'sh-g', 'release', '{}', 'queued', 1, 1)",
                [],
            )
            .is_err(),
            "release makes no effect row"
        );
        assert!(
            conn.execute(
                "INSERT INTO shell_effects
                   (effect_id, command_id, shell_id, op, change, state, created_at, updated_at)
                 VALUES ('h', 'cmd-h', 'sh-h', 'bogus', '{}', 'queued', 1, 1)",
                [],
            )
            .is_err(),
            "an unknown op is refused"
        );
        // The unique command id and the partial index survive the rebuild.
        assert!(
            conn.execute(
                "INSERT INTO shell_effects
                   (effect_id, command_id, shell_id, op, change, state, created_at, updated_at)
                 VALUES ('i', 'cmd-a', 'sh-i', 'open', '{}', 'queued', 1, 1)",
                [],
            )
            .is_err()
        );
        let index: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'shell_effects_open'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(index.contains("'queued', 'claimed'"));
    }

    #[test]
    fn v22_adds_the_assist_cache_and_the_daily_usage() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO assist_cache VALUES ('h', 'title', 'A title', 10, 3, 5)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO assist_cache VALUES ('h', 'title', 'Other', 1, 1, 6)",
                [],
            )
            .is_err(),
            "one row per input hash"
        );
        conn.execute("INSERT INTO assist_usage VALUES (20000, 13)", [])
            .unwrap();
        assert!(
            conn.execute("INSERT INTO assist_usage VALUES (20000, 1)", [])
                .is_err(),
            "one row per day"
        );
    }

    #[test]
    fn v20_admits_one_open_trash_row_per_session() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut conn).unwrap();
        let insert = |op: &str, session: &str, state: &str| {
            conn.execute(
                "INSERT INTO worktree_ops (op_id, session_id, main, admin_dir, original_path,
                     trash_path, dev, ino, state, trashed_at, updated_at)
                 VALUES (?1, ?2, '', '', '/w/a', '/w/.fleetdeck-trash/a--1', 1, 2, ?3, 5, 5)",
                params![op, session, state],
            )
        };
        insert("op1", "s1", "trashed").unwrap();
        assert!(
            insert("op2", "s1", "failed").is_err(),
            "one open row per session"
        );
        insert("op3", "s2", "purged").unwrap();
        insert("op4", "s2", "trashing").unwrap();
        insert("op5", "s2", "released").unwrap();
        conn.execute(
            "UPDATE worktree_ops SET state = 'restored' WHERE op_id = 'op1'",
            [],
        )
        .unwrap();
        insert("op6", "s1", "trashing").unwrap();
    }
}
