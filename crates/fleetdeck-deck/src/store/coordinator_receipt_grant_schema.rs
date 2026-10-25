//! The V40 migration (MCP P0 §3.4): the grant of a coordinator receipt.
//! Forward only; existing rows get NULL and never match a status query. To
//! reverse it, drop the column.

pub(crate) const SQL: &str = "
ALTER TABLE coordinator_receipts ADD COLUMN grant_id TEXT;
";
