//! The V33 migration (R13 P5a, A4): the permanent station tombstone of a
//! deleted artifact. A row says that the artifact is gone (`complete`) or
//! that its files are still to be removed (`pending`). A row is never
//! deleted, so a deleted artifact cannot come back. Forward only; no
//! existing row is rewritten.

pub(super) const V33: &str = "
CREATE TABLE artifact_deletions (            -- R13 P5a A4: permanent station tombstones
    artifact_id TEXT PRIMARY KEY,
    state       TEXT    NOT NULL,            -- pending, complete
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
";
