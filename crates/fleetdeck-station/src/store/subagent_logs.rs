//! The binding of a subagent tool use to its transcript (DL-191, table
//! `subagent_logs`).
//!
//! When a running process reports the result of an `Agent` or `Task` call
//! with an agent id, the station stores one row with the launch facts of
//! that process. `session.subagent` reads only this row.

use fleetdeck_proto::SessionId;
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, call};

/// The V25 migration (DL-191). Forward only; no row is rewritten.
pub const V25: &str = "
-- DL-191: one row for each subagent tool use, with the launch facts of the
-- process that reported it (native id, cwd and absolute provider home).
CREATE TABLE subagent_logs (
    session_id    TEXT NOT NULL,
    tool_use_id   TEXT NOT NULL,
    agent_id      TEXT NOT NULL,
    native_id     TEXT NOT NULL,
    cwd           TEXT NOT NULL,
    provider_home TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, tool_use_id)
) STRICT;
";

/// One row of `subagent_logs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentLogRow {
    pub session_id: SessionId,
    pub tool_use_id: String,
    pub agent_id: String,
    pub native_id: String,
    pub cwd: String,
    pub provider_home: String,
    pub created_at: i64,
}

impl Store {
    /// Stores a binding unless the tool use has one. True when this call
    /// stored it: the first binding of a tool use stays.
    ///
    /// # Errors
    /// A database error.
    pub async fn put_subagent_log(&self, row: SubagentLogRow) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "INSERT OR IGNORE INTO subagent_logs
                   (session_id, tool_use_id, agent_id, native_id, cwd, provider_home, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.session_id.as_str(),
                    row.tool_use_id,
                    row.agent_id,
                    row.native_id,
                    row.cwd,
                    row.provider_home,
                    row.created_at,
                ],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// The binding of a tool use of a session.
    ///
    /// # Errors
    /// A database error.
    pub async fn subagent_log(
        &self,
        session_id: SessionId,
        tool_use_id: String,
    ) -> Result<Option<SubagentLogRow>> {
        call(&self.inner.db, move |conn| {
            let row = conn
                .query_row(
                    "SELECT agent_id, native_id, cwd, provider_home, created_at
                     FROM subagent_logs WHERE session_id = ?1 AND tool_use_id = ?2",
                    params![session_id.as_str(), tool_use_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            Ok(row.map(
                |(agent_id, native_id, cwd, provider_home, created_at)| SubagentLogRow {
                    session_id,
                    tool_use_id,
                    agent_id,
                    native_id,
                    cwd,
                    provider_home,
                    created_at,
                },
            ))
        })
        .await
    }

    /// The provider tool use id of the `tool.started` event of `node_id`
    /// in a session's log (`provider_refs.tool_use_id`).
    ///
    /// # Errors
    /// A database error.
    pub async fn tool_use_of_node(
        &self,
        session_id: SessionId,
        node_id: String,
    ) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT json_extract(payload, '$.provider_refs.tool_use_id') FROM events
                     WHERE session_id = ?1 AND kind = 'tool.started'
                       AND json_extract(payload, '$.node_id') = ?2
                     ORDER BY session_seq LIMIT 1",
                    params![session_id.as_str(), node_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }

    /// The native session id of a session, if it has one.
    ///
    /// # Errors
    /// A database error.
    pub async fn session_native_id(&self, session_id: SessionId) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT native_session_id FROM sessions WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::store::schema::migrations;

    #[test]
    fn v25_creates_one_row_for_each_tool_use() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_version(&mut conn, 25).unwrap();
        let insert = |session: &str, tool: &str| {
            conn.execute(
                "INSERT INTO subagent_logs VALUES (?1, ?2, 'a1', 'n1', '/w', '/h/.claude', 5)",
                params![session, tool],
            )
        };
        insert("s1", "t1").unwrap();
        insert("s1", "t2").unwrap();
        insert("s2", "t1").unwrap();
        assert!(insert("s1", "t1").is_err(), "one row for each tool use");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 25);
    }
}
