//! The V45 migration (MCP P4a §5): the stored worker-grant decision of a
//! start command, so a retry of the same command ID reuses the decision.
//! `original_sha256` is the digest of the board's command. Rows with the
//! decision `none` are pruned after 7 days; rows with a grant go with their
//! grant (`sweep_grants`). Forward only. To reverse it, drop the table: a
//! retry then finds the retained command row (the command store answers it).

pub(crate) const SQL: &str = "
CREATE TABLE worker_attach (
    command_id      TEXT    PRIMARY KEY,
    original_sha256 TEXT    NOT NULL,
    decision        TEXT    NOT NULL CHECK (decision IN ('grant', 'none')),
    reason          TEXT    NOT NULL,
    grant_id        TEXT,
    created_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX worker_attach_by_created ON worker_attach (created_at);
";
