//! The V42 migration (MCP P4b, `docs/v2/design/mcp-p4b.md` section 2): the
//! station keeps one desired grant for each session in `session_grants`.
//! MCP P4b §2: one desired grant per session; reverse with DROP TABLE.
//! Forward only; no existing row changes.

pub(super) const V42: &str = "
-- MCP P4b §2: one desired grant per session, ordered by one sequence.
--   grant_id      the grant of the record (also for an end that was never recorded)
--   seq           the per-session sequence of the latest accepted command
--   spec          the JSON of the CoordinatorSpec; NULL for an end without a record
--   generation    the binding generation of the latest process of this record
--   state         attached or ended
--   ended_reason  why the record ended; NULL while attached
CREATE TABLE session_grants (
    session_id    TEXT    PRIMARY KEY,
    grant_id      TEXT    NOT NULL,
    seq           INTEGER NOT NULL,
    spec          TEXT,
    generation    INTEGER NOT NULL DEFAULT 0,
    state         TEXT    NOT NULL CHECK (state IN ('attached','ended')),
    ended_reason  TEXT,
    updated_at    INTEGER NOT NULL
) STRICT;
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rusqlite::Connection;

    use super::super::migrations::migrations;

    #[test]
    fn v42_adds_session_grants_and_takes_one_row() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 41).unwrap();
        migrations.to_version(&mut conn, 42).unwrap();
        conn.execute(
            "INSERT INTO session_grants
                 (session_id, grant_id, seq, spec, state, updated_at)
             VALUES ('s', 'g', 1, '{}', 'attached', 7)",
            [],
        )
        .unwrap();
        let (generation, reason): (i64, Option<String>) = conn
            .query_row(
                "SELECT generation, ended_reason FROM session_grants WHERE session_id = 's'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(generation, 0, "the generation starts at 0");
        assert_eq!(reason, None);
        assert!(
            conn.execute(
                "INSERT INTO session_grants (session_id, grant_id, seq, state, updated_at)
                 VALUES ('t', 'g', 1, 'other', 7)",
                [],
            )
            .is_err(),
            "the CHECK refuses an unknown state"
        );
        conn.execute(
            "INSERT INTO session_grants (session_id, grant_id, seq, state, updated_at)
             VALUES ('t', 'g', 1, 'ended', 7)",
            [],
        )
        .unwrap();
    }
}
