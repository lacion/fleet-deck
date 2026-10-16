//! The V27 migration (B8, DL-196): the tables of the `!` shell commands.
//! Forward only; no existing row is rewritten.

pub(super) const V27: &str = "
-- B8: one row for each `!` shell command, written in the decision of the
-- command with its effect.
--   state            queued, running, awaiting_proof, done, failed, cancelled,
--                    timeout or unknown
--   advance_pending  1 from the settlement until the folder generation advanced
--   generation       the folder generation at the start of the command
--   included_in      the input id that carried the result to the model
CREATE TABLE shell_runs (
    effect_id        TEXT PRIMARY KEY,
    session_id       TEXT NOT NULL,
    provider         TEXT NOT NULL,
    worktree_root    TEXT NOT NULL,
    command          TEXT NOT NULL,
    state            TEXT NOT NULL,
    exit_code        INTEGER,
    stdout_tail      TEXT,
    stderr_tail      TEXT,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    advance_pending  INTEGER NOT NULL DEFAULT 0,
    generation       INTEGER,
    included_in      TEXT,
    created_at       INTEGER NOT NULL,
    ended_at         INTEGER
) STRICT;
CREATE INDEX shell_runs_root ON shell_runs(worktree_root);
CREATE INDEX shell_runs_session ON shell_runs(session_id);
-- B8: the folder generation of each worktree root; a shell command advances it
-- at its start and at its settlement.
CREATE TABLE worktree_generations (
    worktree_root TEXT PRIMARY KEY,
    generation    INTEGER NOT NULL
) STRICT;
-- B8: durable station flags; `shell_admission` is `open` or `closed`.
CREATE TABLE station_flags (
    key   TEXT PRIMARY KEY,
    value TEXT
) STRICT;
-- B8: the folder generation at which a turn start checkpoint was taken
-- (NULL = 0).
ALTER TABLE turn_checkpoints ADD COLUMN generation INTEGER;
";
