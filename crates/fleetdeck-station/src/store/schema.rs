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
    fn v37_keeps_the_desktop_claims_and_admits_the_input_kinds() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 36).unwrap();
        let insert = |conn: &Connection, id: &str, kind: &str, state: &str| {
            conn.execute(
                "INSERT INTO desktop_ops (deck_fp, operation_id, action_hash, kind, grant_id,
                     caller_station, caller_session, state, result, code, claimed_at, updated_at)
                 VALUES ('fp', ?1, 'h', ?2, 'g', 's', 'x', ?3, '{\"ok\":true}', 'c', 5, 6)",
                params![id, kind, state],
            )
        };
        insert(&conn, "o1", "notify", "done").unwrap();
        insert(&conn, "o2", "preview_open", "claimed").unwrap();
        insert(&conn, "o3", "preview_close", "failed").unwrap();
        assert!(
            insert(&conn, "o4", "input_key_press", "claimed").is_err(),
            "V36 refuses the input kinds"
        );
        type Row = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
        );
        let rows = |conn: &Connection| -> Vec<Row> {
            let mut stmt = conn
                .prepare(
                    "SELECT operation_id, kind, state, action_hash, result, code, claimed_at,
                            updated_at
                     FROM desktop_ops ORDER BY operation_id",
                )
                .unwrap();
            stmt.query_map([], |r| {
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
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        let before = rows(&conn);
        assert_eq!(before.len(), 3);

        migrations.to_latest(&mut conn).unwrap();
        assert_eq!(rows(&conn), before);

        for (id, kind) in [
            ("i1", "input_key_press"),
            ("i2", "input_type_text"),
            ("i3", "input_pointer_move"),
        ] {
            insert(&conn, id, kind, "claimed").unwrap();
        }
        // The other checks and the key survive the rebuild.
        assert!(insert(&conn, "x1", "screenshot", "claimed").is_err());
        assert!(insert(&conn, "x2", "input_click", "claimed").is_err());
        assert!(insert(&conn, "x3", "notify", "running").is_err());
        assert!(
            insert(&conn, "o1", "notify", "done").is_err(),
            "one row per key"
        );
        let leftover: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'desktop_ops_v37'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
    }

    #[test]
    fn v38_keeps_the_desktop_claims_and_admits_the_p7b_input_kinds() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 37).unwrap();
        let insert = |conn: &Connection, id: &str, kind: &str, state: &str| {
            conn.execute(
                "INSERT INTO desktop_ops (deck_fp, operation_id, action_hash, kind, grant_id,
                     caller_station, caller_session, state, result, code, claimed_at, updated_at)
                 VALUES ('fp', ?1, 'h', ?2, 'g', 's', 'x', ?3, '{\"ok\":true}', 'c', 5, 6)",
                params![id, kind, state],
            )
        };
        for (id, kind, state) in [
            ("o1", "notify", "done"),
            ("o2", "preview_open", "claimed"),
            ("o3", "preview_close", "failed"),
            ("o4", "input_key_press", "done"),
            ("o5", "input_type_text", "claimed"),
            ("o6", "input_pointer_move", "failed"),
        ] {
            insert(&conn, id, kind, state).unwrap();
        }
        assert!(
            insert(&conn, "n1", "input_pointer_click", "claimed").is_err(),
            "V37 refuses the P7b kinds"
        );
        type Row = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
        );
        let rows = |conn: &Connection| -> Vec<Row> {
            let mut stmt = conn
                .prepare(
                    "SELECT operation_id, kind, state, action_hash, result, code, claimed_at,
                            updated_at
                     FROM desktop_ops ORDER BY operation_id",
                )
                .unwrap();
            stmt.query_map([], |r| {
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
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        let before = rows(&conn);
        assert_eq!(before.len(), 6);

        migrations.to_version(&mut conn, 38).unwrap();
        assert_eq!(rows(&conn), before);

        for (id, kind) in [
            ("p1", "input_pointer_click"),
            ("p2", "input_any_key_press"),
            ("p3", "input_any_type_text"),
            ("p4", "input_any_pointer_move"),
            ("p5", "input_any_pointer_click"),
        ] {
            insert(&conn, id, kind, "claimed").unwrap();
        }
        // The other checks and the key survive the rebuild.
        assert!(insert(&conn, "x1", "screenshot", "claimed").is_err());
        assert!(insert(&conn, "x2", "input_click", "claimed").is_err());
        assert!(insert(&conn, "x3", "input_any_scroll", "claimed").is_err());
        assert!(insert(&conn, "x4", "notify", "running").is_err());
        assert!(
            insert(&conn, "o1", "notify", "done").is_err(),
            "one row per key"
        );
        let leftover: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'desktop_ops_v38'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
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
