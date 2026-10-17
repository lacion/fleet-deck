//! The V29 migration (Fleet Git phase 2, design §3.5): the home attempts of
//! `project.ensure_home` and the artifact authorization of a session.
//! Forward only; no existing row is rewritten.

pub(super) const V29: &str = "
CREATE TABLE home_attempts (
    attempt_id     TEXT PRIMARY KEY,
    project_id     TEXT    NOT NULL,
    generation     INTEGER NOT NULL,
    request_digest TEXT    NOT NULL,   -- SHA-256 of the canonical ensure_home params
    retry_of       TEXT,               -- the attempt that this one retries
    dest_path      TEXT    NOT NULL,   -- the reserved destination <canonical clone_root>/<folder>
    url            TEXT    NOT NULL,   -- the clone URL (with the SSH login name)
    remote_name    TEXT    NOT NULL,   -- the remote that the clone creates (push_remote at the add)
    state          TEXT    NOT NULL,   -- cloning, ready, adopted, failed, interrupted, cleared
    superseded     INTEGER NOT NULL DEFAULT 0 CHECK (superseded IN (0, 1)),
    path           TEXT,               -- the verified canonical path; only for ready and adopted
    dev            INTEGER, ino INTEGER,
    code           TEXT, message TEXT,
    started_at     INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    CHECK (state NOT IN ('ready', 'adopted') OR path IS NOT NULL)
) STRICT;
CREATE INDEX home_attempts_project ON home_attempts (project_id, generation);
CREATE UNIQUE INDEX home_attempts_reserved ON home_attempts (dest_path)
    WHERE state IN ('cloning', 'failed', 'interrupted')
       OR (state IN ('ready', 'adopted') AND superseded = 0);
-- The newest ArtifactAuthorization JSON of a session: display and audit only.
ALTER TABLE session_git ADD COLUMN artifact_auth TEXT;
";
