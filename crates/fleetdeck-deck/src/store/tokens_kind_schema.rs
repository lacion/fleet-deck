//! The V43 migration (MCP P4a §4): the kind of a grant. Every existing grant
//! reads `coordinator`; only the worker attachment of `worker_grant.rs`
//! writes `worker`. Forward only. To reverse it, drop the column (a worker
//! grant would then read as an owner grant, so revoke the worker grants
//! first).

pub(crate) const SQL: &str = "
ALTER TABLE tokens ADD COLUMN kind TEXT NOT NULL DEFAULT 'coordinator'
    CHECK (kind IN ('coordinator', 'worker'));
";
