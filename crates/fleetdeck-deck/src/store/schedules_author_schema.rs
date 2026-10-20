//! The V44 migration (MCP P4a §3): who saved a schedule last. `owner` is the
//! owner's login, `token` a bearer token; every existing row reads
//! `unknown` and gets no worker grant until the owner saves it. Forward
//! only. To reverse it, drop the column.

pub(crate) const SQL: &str = "
ALTER TABLE schedules ADD COLUMN author TEXT NOT NULL DEFAULT 'unknown'
    CHECK (author IN ('owner', 'token', 'unknown'));
";
