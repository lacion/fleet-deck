//! The V39 migration (BG-4, design `docs/v2/design/board-gaps.md` §5): the
//! member witnesses of a shim run and the durable stops of a lost run.
//! Forward only: two new tables, no row of an older table changes. To
//! reverse it, drop both tables; no other table refers to them.

pub(super) const V39: &str = "
-- BG-4: the processes that the station saw as members of the domain of a
-- run while its leader was alive (PID, start time, boot id). A witness
-- authorizes only itself. At most 512 rows per shim; the oldest go first.
CREATE TABLE execution_witnesses (
    shim_id     TEXT    NOT NULL,
    pid         INTEGER NOT NULL,
    start_time  INTEGER NOT NULL,
    boot_id     TEXT    NOT NULL,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (shim_id, pid, start_time)
) STRICT;

-- BG-4: one row per `session.stop_execution` command. The row is written
-- as `authorized` in the command transaction, `attempted` before the first
-- signal, and settled to `done`, `failed` or `unknown`.
CREATE TABLE execution_stops (
    command_id  TEXT    PRIMARY KEY,
    session_id  TEXT    NOT NULL,
    shim_id     TEXT    NOT NULL,
    plan_id     TEXT    NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN ('authorized','attempted','done','failed','unknown')),
    code        TEXT,
    term_sent   INTEGER NOT NULL DEFAULT 0,
    kill_sent   INTEGER NOT NULL DEFAULT 0,
    gone        INTEGER NOT NULL DEFAULT 0,
    skipped     INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rusqlite::Connection;

    use super::super::migrations::migrations;

    #[test]
    fn v39_keeps_the_rows_of_the_older_tables_and_adds_the_stop_tables() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 38).unwrap();
        conn.execute(
            "INSERT INTO desktop_ops (deck_fp, operation_id, action_hash, kind, grant_id,
                 caller_station, caller_session, state, result, code, claimed_at, updated_at)
             VALUES ('fp', 'o1', 'h', 'notify', 'g', 's', 'x', 'done', '{}', 'c', 5, 6)",
            [],
        )
        .unwrap();
        assert!(
            conn.prepare("SELECT 1 FROM execution_stops").is_err(),
            "V38 has no stop table"
        );
        migrations.to_version(&mut conn, 39).unwrap();
        let kept: i64 = conn
            .query_row("SELECT count(*) FROM desktop_ops", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1);
        for table in ["execution_witnesses", "execution_stops"] {
            let rows: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
        conn.execute(
            "INSERT INTO execution_stops (command_id, session_id, shim_id, plan_id, state,
                 created_at, updated_at) VALUES ('c1', 's', 'x', 'p', 'authorized', 1, 1)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO execution_stops (command_id, session_id, shim_id, plan_id, state,
                     created_at, updated_at) VALUES ('c2', 's', 'x', 'p', 'running', 1, 1)",
                [],
            )
            .is_err(),
            "the state check holds"
        );
        conn.execute(
            "INSERT INTO execution_witnesses VALUES ('x', 10, 5, 'b', 1)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO execution_witnesses VALUES ('x', 10, 5, 'b', 2)",
                [],
            )
            .is_err(),
            "one row per shim, pid and start time"
        );
    }
}
