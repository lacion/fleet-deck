//! The V46 migration (MCP Apps, B15, `docs/v2/design/mcp-apps.md` section 4.4):
//! the stored app documents and the app tool calls. Forward only; no existing
//! row changes. Reverse with `DROP TABLE mcp_app_calls; DROP TABLE mcp_apps;`.

pub(super) const V46: &str = "
-- B15: one row per MCP App that the station captured after a tool call.
--   tool          the raw tool name on the server (never the full mcp__ name)
--   html          the document as read, at most 2 MiB
--   doc_sha256    hex SHA-256 of html
--   csp_json      the sanitized AppCsp
--   prefers_border NULL, 0 or 1
--   tool_input    JSON, at most 64 KiB; 'null' when it was over
--   tool_result   JSON CallToolResult, at most 512 KiB; NULL when over
CREATE TABLE mcp_apps (
  app_id        TEXT PRIMARY KEY,
  session_id    TEXT NOT NULL,
  node_id       TEXT NOT NULL,
  server        TEXT NOT NULL,
  tool          TEXT NOT NULL,
  resource_uri  TEXT NOT NULL,
  html          BLOB NOT NULL,
  doc_sha256    TEXT NOT NULL,
  csp_json      TEXT NOT NULL,
  prefers_border INTEGER,
  tool_input    TEXT NOT NULL,
  tool_result   TEXT,
  created_at    INTEGER NOT NULL
);
CREATE INDEX mcp_apps_session ON mcp_apps(session_id, created_at);

-- B15: one row per app tools/call. Never deleted by the app caps; a row does
-- not need its mcp_apps row (list and release use the call row only).
--   read_only        1: the tool is read-only; no generation advance
--   process          the ProcessIncarnation text of the session's process
--   incarnation      store.incarnation() of the station process that started it
--   state            running | uncertain | done | failed | ended | released
--   cause            why uncertain: station_restart | provider_error; else NULL
--   generation       the folder generation after the start advance; NULL when read_only
CREATE TABLE mcp_app_calls (
  call_id         TEXT PRIMARY KEY,
  session_id      TEXT NOT NULL,
  app_id          TEXT NOT NULL,
  worktree_root   TEXT NOT NULL,
  server          TEXT NOT NULL,
  tool            TEXT NOT NULL,
  read_only       INTEGER NOT NULL,
  process         TEXT NOT NULL,
  incarnation     TEXT NOT NULL,
  state           TEXT NOT NULL,
  cause           TEXT,
  advance_pending INTEGER NOT NULL DEFAULT 0,
  generation      INTEGER,
  created_at      INTEGER NOT NULL,
  ended_at        INTEGER
);
CREATE INDEX mcp_app_calls_root ON mcp_app_calls(worktree_root, state);
CREATE INDEX mcp_app_calls_session ON mcp_app_calls(session_id, state);
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rusqlite::Connection;

    use super::super::migrations::migrations;

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                 AND name IN ('mcp_apps', 'mcp_app_calls') ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn migration_v46_applies() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        // V44 and V45 belong to other tracks: the test names no version number
        // above the one that precedes this migration (V43).
        migrations.to_version(&mut conn, 43).unwrap();
        assert!(tables(&conn).is_empty());
        migrations.to_latest(&mut conn).unwrap();
        assert_eq!(tables(&conn), ["mcp_app_calls", "mcp_apps"]);
        conn.execute(
            "INSERT INTO mcp_app_calls (call_id, session_id, app_id, worktree_root, server, tool,
                 read_only, process, incarnation, state, created_at)
             VALUES ('c', 's', 'a', '/w', 'srv', 't', 1, 'p', 'i', 'running', 1)",
            [],
        )
        .unwrap();
        let (pending, generation): (i64, Option<i64>) = conn
            .query_row(
                "SELECT advance_pending, generation FROM mcp_app_calls WHERE call_id = 'c'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((pending, generation), (0, None));
    }
}
