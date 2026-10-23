//! The V45 migration (B12, design `docs/v2/design/workflow-agents.md` §4.4):
//! the workflow projection, the binding of workflow agents to their
//! transcripts and the launch facts of a process. Additive; forward only.

pub(super) const V45: &str = "
-- B12: the workflow projection (§4.3a). Written in the event transaction.
CREATE TABLE workflow_runs (
    session_id     TEXT    NOT NULL,
    task_id        TEXT    NOT NULL,
    run_order      INTEGER NOT NULL,
    node_id        TEXT,
    name           TEXT,
    run_id         TEXT,
    status         TEXT    NOT NULL,
    untracked      INTEGER NOT NULL DEFAULT 0,
    running_agents INTEGER NOT NULL DEFAULT 0,
    agents_total   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, task_id)
) STRICT;
CREATE INDEX workflow_runs_order ON workflow_runs(session_id, run_order);
CREATE TABLE workflow_phases (
    session_id TEXT    NOT NULL,
    task_id    TEXT    NOT NULL,
    idx        INTEGER NOT NULL,
    title      TEXT    NOT NULL,
    PRIMARY KEY (session_id, task_id, idx)
) STRICT;
CREATE TABLE workflow_agents (
    session_id TEXT    NOT NULL,
    task_id    TEXT    NOT NULL,
    idx        INTEGER NOT NULL,
    agent_id   TEXT,
    state      TEXT    NOT NULL,
    entry      TEXT    NOT NULL,  -- the JSON of the WorkflowAgent
    PRIMARY KEY (session_id, task_id, idx)
) STRICT;
-- B12: one row for each bound workflow agent, with the run folder and the
-- launch facts of the process that reported it. Written in the event
-- transaction (§4.4 Binding); never removed by R6.
CREATE TABLE workflow_agent_logs (
    session_id    TEXT NOT NULL,
    agent_id      TEXT NOT NULL,
    run_id        TEXT NOT NULL,
    native_id     TEXT NOT NULL,
    cwd           TEXT NOT NULL,
    provider_home TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, agent_id)
) STRICT;
-- B12 (also DL-191): the launch facts of a process (direct or shim), written
-- in the same transaction as its `processes` row. The binding reads them in
-- the event transaction; an adoption reads them for the DL-191 observer.
CREATE TABLE process_launch_facts (
    process_incarnation TEXT PRIMARY KEY,
    session_id          TEXT    NOT NULL,
    provider_home       TEXT    NOT NULL,
    cwd                 TEXT    NOT NULL,
    created_at          INTEGER NOT NULL
) STRICT;
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rusqlite::Connection;

    use super::super::migrations::migrations;

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_v43_database_migrates_to_v45_with_the_workflow_tables() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 43).unwrap();
        conn.execute(
            "INSERT INTO sessions (session_id, provider, provider_instance, cwd, mode, state,
                 offline_approval_policy, summary, created_at, updated_at)
             VALUES ('s1', 'claude', 'claude', '/w', 'default', 'running', '{}', '{}', 1, 1)",
            [],
        )
        .unwrap();
        migrations.to_latest(&mut conn).unwrap();
        // The old row stays.
        assert_eq!(count(&conn, "sessions"), 1);
        // Each new table accepts one row.
        conn.execute(
            "INSERT INTO workflow_runs (session_id, task_id, run_order, status)
             VALUES ('s1', 't1', 1, 'running')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_phases (session_id, task_id, idx, title)
             VALUES ('s1', 't1', 1, 'Alpha')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_agents (session_id, task_id, idx, agent_id, state, entry)
             VALUES ('s1', 't1', 1, 'a1', 'running', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_agent_logs
                 (session_id, agent_id, run_id, native_id, cwd, provider_home, created_at)
             VALUES ('s1', 'a1', 'wf_x', 'n1', '/w', '/h/.claude', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO process_launch_facts
                 (process_incarnation, session_id, provider_home, cwd, created_at)
             VALUES ('p1', 's1', '/h/.claude', '/w', 5)",
            [],
        )
        .unwrap();
        for table in [
            "workflow_runs",
            "workflow_phases",
            "workflow_agents",
            "workflow_agent_logs",
            "process_launch_facts",
        ] {
            assert_eq!(count(&conn, table), 1, "{table}");
        }
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 44, "V45 sits at position 44 on this branch");
    }
}
