//! The V30 migration (Fleet Git phase 3, design §3.5, §11.5, §11.6): the land
//! runs and their journal, the fences, the temporary worktrees and the session
//! holds. Forward only; no existing row is rewritten.

pub(super) const V30: &str = "
CREATE TABLE land_runs (
    land_id               TEXT PRIMARY KEY,
    command_id            TEXT    NOT NULL UNIQUE,
    session_id            TEXT    NOT NULL REFERENCES sessions(session_id),
    plan                  TEXT    NOT NULL,     -- LandPlan JSON
    attempt               INTEGER NOT NULL DEFAULT 1 CHECK (attempt IN (1, 2)),
    phase                 TEXT    NOT NULL,     -- LandPhase
    state                 TEXT    NOT NULL,     -- running or a terminal LandState
    default_ref           TEXT,                 -- resolved default branch name
    source_oid            TEXT,                 -- captured in attempt 1; never changed
    base_oid              TEXT,                 -- captured in each attempt
    rebased_oid           TEXT,                 -- R of the current attempt
    branch_lease          TEXT,                 -- the expected remote branch tip; '' = must not exist
    branch_published_oid  TEXT,                 -- set when a branch push is confirmed (any attempt)
    default_published_oid TEXT,
    pr_number             INTEGER, pr_url TEXT,
    files                 TEXT    NOT NULL DEFAULT '[]',
    code                  TEXT, message TEXT,
    authority             TEXT,                 -- CommandAuthority JSON; NULL for an owner land
    -- the outstanding remote write, kept apart from the phase (M1)
    pending_action        TEXT,                 -- NULL, branch_push, pr_create, default_push
    pending_attempt       INTEGER,              -- the land attempt (1 or 2) of the write
    pending_url           TEXT,                 -- the verified URL of the write, or the forge API base for pr_create
    pending_ref           TEXT,                 -- refs/heads/<publish_ref> or refs/heads/<default_ref>
    pending_oid           TEXT,                 -- R of the write
    pending_lease         TEXT,                 -- the branch lease ('' = must not exist); NULL for default_push and pr_create
    pending_expect        TEXT,                 -- default_push: base_oid; pr_create: '<head> TAB <base>'
    reconciling           INTEGER NOT NULL DEFAULT 0 CHECK (reconciling IN (0, 1)),
    reconcile_attempts    INTEGER NOT NULL DEFAULT 0,
    next_reconcile_at     INTEGER,
    last_write_outcome    TEXT,                 -- NULL while a write runs; applied, rejected, unknown, not_started
    abandon_write         TEXT,                 -- none, applied, rejected, unknown; set by the fence transaction
    created_at            INTEGER NOT NULL, updated_at INTEGER NOT NULL, ended_at INTEGER,
    CHECK (pending_action IS NULL OR (pending_attempt IS NOT NULL AND pending_url IS NOT NULL AND pending_ref IS NOT NULL AND pending_oid IS NOT NULL)),
    CHECK (reconciling = 0 OR pending_action IS NOT NULL)
) STRICT;
CREATE TABLE land_fences (
    land_id   TEXT PRIMARY KEY,
    fenced_at INTEGER NOT NULL
) STRICT;
CREATE TABLE land_temps (
    land_id    TEXT    NOT NULL,
    attempt    INTEGER NOT NULL,
    root       TEXT    NOT NULL,   -- the verified allowed root (canonical)
    path       TEXT    NOT NULL,   -- <root>/.fleetdeck-land/<first 12 chars of land_id>-<attempt>
    dev        INTEGER, ino INTEGER,
    admin_dir  TEXT,               -- <common git dir>/worktrees/<name>
    state      TEXT    NOT NULL,   -- reserved, present, removed, kept
    code       TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (land_id, attempt)
) STRICT;
CREATE TABLE session_holds (
    session_id TEXT    PRIMARY KEY REFERENCES sessions(session_id),  -- at most one hold per session
    kind       TEXT    NOT NULL CHECK (kind IN ('land', 'move', 'move_in')),
    holder_id  TEXT    NOT NULL,   -- land_id or move_id
    created_at INTEGER NOT NULL
) STRICT;
";
