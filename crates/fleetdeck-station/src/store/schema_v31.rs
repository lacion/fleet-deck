//! The V31 migration (Fleet Git phase 4, design §3.5, §12.3): the move runs of
//! the source (`move_out_runs`) and of the target (`move_in_runs`), and the
//! durable tombstones of discarded moves. Forward only; no existing row is
//! rewritten.

pub(super) const V31: &str = "
CREATE TABLE move_out_runs (                 -- on the source
    move_id     TEXT PRIMARY KEY,
    command_id  TEXT NOT NULL UNIQUE,
    session_id  TEXT NOT NULL REFERENCES sessions(session_id),
    to_station  TEXT NOT NULL,
    state       TEXT NOT NULL,               -- holding, finished, aborted, undone (revision 4)
    publication TEXT NOT NULL,               -- MovePublication JSON, captured under the hold
    authority   TEXT,
    created_at  INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE move_in_runs (                  -- on the target
    move_id           TEXT PRIMARY KEY,
    command_id        TEXT NOT NULL UNIQUE,
    session_id        TEXT NOT NULL REFERENCES sessions(session_id),  -- the new session
    provider          TEXT NOT NULL,
    native_session_id TEXT,                  -- NULL for branch_only
    state             TEXT NOT NULL,         -- staged, imported, launching, launch_refused, resumed, released,
                                             -- confirmed, resume_failed, failed, abandoned, discarded
    staging_path      TEXT, staging_dev INTEGER, staging_ino INTEGER,
    imported          TEXT NOT NULL DEFAULT '[]',  -- [{ path, dev, ino }] of the files placed in the provider home
    authority         TEXT,
    first_input_at    INTEGER,               -- revision 4 (NEW-2): the first input after the release (Claude); NULL before
    code              TEXT, message TEXT,
    created_at        INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE move_tombstones (               -- revision 4 (B4), on the target: a discarded move id never runs
    move_id TEXT PRIMARY KEY,
    at      INTEGER NOT NULL
) STRICT;
";
