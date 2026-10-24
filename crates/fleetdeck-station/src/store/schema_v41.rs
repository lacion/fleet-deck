//! The V41 migration (MCP P4a, `docs/v2/design/mcp-p4a.md` section 6.1): the
//! rows of `shim_bindings` get the kind of their grant, so that a shim
//! reattach keeps a worker binding a worker binding. Existing rows read
//! `coordinator`. Forward only. To reverse it, rebuild the table without the
//! column; a `worker` row then reads as an owner grant, so end the open
//! worker rows first.

pub(super) const V41: &str = "
ALTER TABLE shim_bindings
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'coordinator'
        CHECK (kind IN ('coordinator','worker'));
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rusqlite::Connection;

    use super::super::migrations::migrations;

    #[test]
    fn v41_adds_the_kind_and_existing_rows_read_coordinator() {
        let migrations = migrations();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations.to_version(&mut conn, 40).unwrap();
        conn.execute(
            "INSERT INTO shim_bindings (digest, shim_id, session_id, binding_id, grant_id,
                 provider_instance, generation, classes, scope, state, control_skill, created_at)
             VALUES ('d', 'x', 's', 'b', 'g', 'claude', 0, '[]', '\"lineage\"', 'pending', '\"\"', 1)",
            [],
        )
        .unwrap();
        migrations.to_version(&mut conn, 41).unwrap();
        let kind: String = conn
            .query_row(
                "SELECT kind FROM shim_bindings WHERE digest = 'd'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "coordinator");
        assert!(
            conn.execute("UPDATE shim_bindings SET kind = 'other'", [])
                .is_err(),
            "the CHECK refuses an unknown kind"
        );
        conn.execute("UPDATE shim_bindings SET kind = 'worker'", [])
            .unwrap();
    }
}
