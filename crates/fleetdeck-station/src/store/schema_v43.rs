//! The V43 migration (DL-402 follow-up): `runtime_requests` is keyed by
//! (session_id, request_id), because the providers number request ids per
//! session. Forward only; every row is kept.

pub(super) const V43: &str = "
-- DL-402: two sessions of one station can have the same request id (`req-3`),
-- so the key is (session_id, request_id). The table keeps its rowid order.
CREATE TABLE runtime_requests_v43 (
    request_id           TEXT    NOT NULL,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    process_incarnation  TEXT REFERENCES processes(process_incarnation),
    node_id              TEXT,
    kind                 TEXT    NOT NULL,
    options              TEXT    NOT NULL,
    response_channel     TEXT    NOT NULL,
    state                TEXT    NOT NULL CHECK (state IN ('open', 'resolved', 'expired')),
    receipt              TEXT,
    deny_at              INTEGER,
    requested_at         INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL,
    time_proven          INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (session_id, request_id)
) STRICT;
INSERT INTO runtime_requests_v43
    (request_id, session_id, process_incarnation, node_id, kind, options,
     response_channel, state, receipt, deny_at, requested_at, updated_at, time_proven)
SELECT request_id, session_id, process_incarnation, node_id, kind, options,
     response_channel, state, receipt, deny_at, requested_at, updated_at, time_proven
FROM runtime_requests ORDER BY rowid;
DROP TABLE runtime_requests;
ALTER TABLE runtime_requests_v43 RENAME TO runtime_requests;
CREATE INDEX runtime_requests_open ON runtime_requests(state, deny_at);
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rusqlite::{Connection, params};

    use super::super::migrations::migrations;

    fn session(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, provider, provider_instance, cwd, state,
                 offline_approval_policy, summary, created_at, updated_at)
             VALUES (?1, 'claude', 'claude', '/w', 'running', 'wait', '{}', 1, 1)",
            [id],
        )
        .unwrap();
    }

    fn request(conn: &Connection, session: &str) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO runtime_requests (request_id, session_id, kind, options,
                 response_channel, state, requested_at, updated_at)
             VALUES ('req-3', ?1, 'tool', '[]', 'c', 'open', 5, 6)",
            params![session],
        )
    }

    #[test]
    fn v43_keeps_every_row_and_allows_one_id_in_two_sessions() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 42).unwrap();
        session(&conn, "a");
        session(&conn, "b");
        conn.execute(
            "INSERT INTO runtime_requests (request_id, session_id, node_id, kind, options,
                 response_channel, state, receipt, deny_at, requested_at, updated_at,
                 time_proven)
             VALUES ('req-3', 'a', 'n1', 'tool', '[1]', 'c', 'open', '{\"r\":1}', 99, 5, 6, 0)",
            [],
        )
        .unwrap();
        migrations.to_version(&mut conn, 43).unwrap();
        let kept: Vec<String> = conn
            .query_row(
                "SELECT request_id || '|' || session_id || '|' || node_id || '|' || kind
                        || '|' || options || '|' || response_channel || '|' || state
                        || '|' || receipt || '|' || COALESCE(process_incarnation, 'none')
                        || '|' || deny_at || '|' || requested_at || '|' || updated_at
                        || '|' || time_proven
                 FROM runtime_requests",
                [],
                |r| r.get::<_, String>(0),
            )
            .map(|row| row.split('|').map(str::to_owned).collect())
            .unwrap();
        assert_eq!(
            kept,
            [
                "req-3",
                "a",
                "n1",
                "tool",
                "[1]",
                "c",
                "open",
                "{\"r\":1}",
                "none",
                "99",
                "5",
                "6",
                "0"
            ]
        );
        request(&conn, "b").unwrap();
        assert!(
            request(&conn, "a").is_err(),
            "the key is (session, request)"
        );
    }
}
