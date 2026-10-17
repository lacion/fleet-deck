//! The V28 migration (Fleet Git phase 1, design §3.5): the snapshot of the
//! project settings, the Fleet Git data of a session and the push state.
//! Forward only; no existing row is rewritten.

pub(super) const V28: &str = "
CREATE TABLE git_snapshot (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    revision    INTEGER NOT NULL,
    digest      TEXT    NOT NULL,   -- SHA-256 of the canonical snapshot JSON
    received_at INTEGER NOT NULL
) STRICT;
CREATE TABLE git_projects (
    project_id        TEXT PRIMARY KEY,
    settings_revision INTEGER NOT NULL,
    home_generation   INTEGER NOT NULL,
    home_path         TEXT    NOT NULL,   -- as the deck sent it
    canonical_path    TEXT,               -- the station's realpath after the home check
    check_state       TEXT    NOT NULL,   -- verified, mismatch, missing, not_a_repo, remote_ambiguous, no_remote, refused
    check_code        TEXT,
    view              TEXT    NOT NULL,   -- GitProjectView JSON
    updated_at        INTEGER NOT NULL
) STRICT;
CREATE TABLE station_homes (
    project_id TEXT    NOT NULL,
    generation INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    sync       INTEGER NOT NULL CHECK (sync IN (0, 1)),
    PRIMARY KEY (project_id)
) STRICT;                                 -- every home of the newest snapshot (O37 coverage, §15.4)
CREATE TABLE session_git (
    session_id        TEXT PRIMARY KEY REFERENCES sessions(session_id),
    project_id        TEXT    NOT NULL,
    kind              TEXT    NOT NULL CHECK (kind IN ('worktree', 'in_place')),
    settings_revision INTEGER NOT NULL,
    home_generation   INTEGER NOT NULL,
    slug              TEXT,               -- NULL for in_place; the slug of THIS station at the start or the move-in
    branch            TEXT,               -- NULL for in_place; the LOCAL branch (a moved-in session keeps the source name, §12.5)
    publish_ref       TEXT,               -- revision 3 (M9): the remote branch that FG-2 and FG-8 write; fd/<slug>/<callsign>[-n]
    created_branch    INTEGER NOT NULL CHECK (created_branch IN (0, 1)), -- 1: the station made `branch` for this session (start or move-in)
    repo_root         TEXT    NOT NULL,   -- the canonical home path
    remote_dest       TEXT    NOT NULL,   -- RemoteIdentity.dest of the project at the start
    base_stale        INTEGER NOT NULL DEFAULT 0,
    ended_at          INTEGER,            -- the LATEST transition to exited/lost; NULL while live (decision 24)
    cleanup_state     TEXT,               -- NULL, waiting, kept:<reason>, trashed
    cleanup_at        INTEGER,
    artifacts_path    TEXT,
    artifacts_dev     INTEGER,
    artifacts_ino     INTEGER,
    artifacts_url     TEXT,
    created_at        INTEGER NOT NULL,
    CHECK (kind = 'in_place' OR (slug IS NOT NULL AND branch IS NOT NULL)),
    CHECK (created_branch = 0 OR kind = 'worktree'),
    CHECK (created_branch = 0 OR (publish_ref IS NOT NULL AND substr(publish_ref, 1, length(slug) + 4) = 'fd/' || slug || '/'))
) STRICT;
CREATE TABLE git_pushes (
    session_id      TEXT PRIMARY KEY REFERENCES sessions(session_id),
    target          TEXT    NOT NULL,  -- the remote name or the wip_remote URL, as configured
    target_dest     TEXT    NOT NULL,  -- RemoteIdentity.dest of the effective push URL (§3.8)
    ref             TEXT    NOT NULL,  -- session_git.publish_ref (revision 3, M9); the lease belongs to (target_dest, ref)
    last_pushed_oid TEXT,              -- the lease (O10)
    last_push_kind  TEXT,              -- push, land
    desired_oid     TEXT,              -- the newest tip that a trigger saw (decision 18)
    attempt_id      TEXT,              -- the open attempt; NULL when none is open
    attempt_oid     TEXT,              -- the commit of the open attempt; never changed during the attempt
    attempt_lease   TEXT,              -- the lease of the open attempt; '' = the ref must not exist
    attempt_url     TEXT,              -- revision 3 (B1): the verified push URL of the open attempt
    state           TEXT    NOT NULL,  -- idle, due, pushing, pushed, failing, stopped
    stop_code       TEXT,              -- project_gone, moved, land_published, unsupported_mode, move_failed
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    code            TEXT,
    message         TEXT,
    updated_at      INTEGER NOT NULL,
    UNIQUE (target_dest, ref)
) STRICT;
-- revision 3 (M4): the identity that the cleanup proved; the trash effect checks it again (§6.3)
ALTER TABLE worktree_ops ADD COLUMN expect_branch TEXT;
ALTER TABLE worktree_ops ADD COLUMN expect_oid    TEXT;
";
