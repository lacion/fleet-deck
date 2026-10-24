//! The V49 migration (B28, `docs/v2/design/cli-update.md` section 3.11): the
//! attempts of provider CLI updates, the durable journal of an attempt that
//! may change an install, and the quarantine of a provider. Forward only; no
//! existing row changes. The number is reserved: the merge renumbers it.
//! Reverse with `DROP TABLE cli_update_quarantine; DROP TABLE cli_update_journal;
//! DROP TABLE cli_update_attempts;`.

pub(super) const V49: &str = "
CREATE TABLE cli_update_attempts (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id      TEXT    NOT NULL UNIQUE,
    provider        TEXT    NOT NULL CHECK (provider IN ('claude', 'codex')),
    trigger         TEXT    NOT NULL,
    method          TEXT    NOT NULL,
    channel         TEXT    NOT NULL,
    from_version    TEXT,
    target_version  TEXT,
    to_version      TEXT,
    outcome         TEXT    NOT NULL,
    reason          TEXT,
    started_at      INTEGER NOT NULL,
    ended_at        INTEGER
);
CREATE INDEX cli_update_attempts_provider ON cli_update_attempts (provider, seq);

-- The durable record of an attempt that may change an install. It lives from
-- the start of the attempt until the transaction that records the final
-- result; the start recovery reads it. Paths are local to the station and
-- never leave it.
CREATE TABLE cli_update_journal (
    provider        TEXT    PRIMARY KEY CHECK (provider IN ('claude', 'codex')),
    attempt_id      TEXT    NOT NULL REFERENCES cli_update_attempts (attempt_id),
    marker          TEXT    NOT NULL,
    method          TEXT    NOT NULL CHECK (method IN ('native', 'npm', 'bun')),
    phase           TEXT    NOT NULL CHECK (phase IN ('staging', 'activating')),
    from_version    TEXT    NOT NULL,
    target_version  TEXT    NOT NULL,
    launcher        TEXT    NOT NULL,
    dest            TEXT    NOT NULL,   -- native: the link L; npm, Bun: `modules`
    stage_root      TEXT    NOT NULL,   -- native: versions/.fd-stage-<id>; npm, Bun: R
    rollback_path   TEXT,               -- native: the rollback copy; npm, Bun: R/old
    rollback_sha256 TEXT,               -- native only
    moved           TEXT,               -- npm, Bun: JSON [{\"name\", \"digest\"}]
    installer_pgid  INTEGER,
    updated_at      INTEGER NOT NULL
);

-- The providers whose starts are refused until the station clears them.
CREATE TABLE cli_update_quarantine (
    provider    TEXT    PRIMARY KEY CHECK (provider IN ('claude', 'codex')),
    attempt_id  TEXT    NOT NULL,
    reason      TEXT    NOT NULL CHECK (reason IN ('rollback_failed', 'recovery_pending', 'process_left')),
    text        TEXT    NOT NULL,
    since       INTEGER NOT NULL
);
";
