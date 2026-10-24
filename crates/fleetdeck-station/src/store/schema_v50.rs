//! The V50 migration (analytics, B29, `docs/v2/design/analytics.md` section
//! 3.3): the transcript files, the thread ancestry, the usage hour buckets,
//! the dedupe keys, the Fleet Deck counters, the execution intervals and the
//! event cursor. Forward only; no existing row changes. Reverse with
//! `DROP TABLE analytics_meta; DROP TABLE analytics_workflows;
//! DROP TABLE analytics_exec; DROP TABLE analytics_open;
//! DROP TABLE analytics_activity; DROP TABLE analytics_seen;
//! DROP TABLE analytics_usage; DROP TABLE analytics_threads;
//! DROP TABLE analytics_files;`.

pub(super) const V50: &str = "
-- B29: one row per transcript file that the analytics scan read.
--   home      the source key (AnalyticsSource.key)
--   rel_path  the path below the home; never sent on the wire
--   mtime_ns  st_mtime of the file in ns at the last read
--   offset    bytes read: the end of the last whole line
--   guard     hex SHA-256 of the 64 bytes before offset ('' at offset 0)
--   state     the JSON parser state at offset (Codex; '{}' for Claude)
--   seen_size the file size at the last read
CREATE TABLE analytics_files (
    home        TEXT    NOT NULL,
    rel_path    TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    dev         INTEGER NOT NULL,
    ino         INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    offset      INTEGER NOT NULL,
    guard       TEXT    NOT NULL,
    state       TEXT    NOT NULL,
    seen_size   INTEGER NOT NULL,
    malformed   INTEGER NOT NULL DEFAULT 0,
    scanned_at  INTEGER NOT NULL,
    PRIMARY KEY (home, rel_path)
) STRICT;

-- B29: the Codex thread ancestry, from the first session_meta of each
-- rollout. parent NULL: a root thread. Kept forever.
CREATE TABLE analytics_threads (
    home   TEXT NOT NULL,
    thread TEXT NOT NULL,
    parent TEXT,
    PRIMARY KEY (home, thread)
) STRICT, WITHOUT ROWID;

-- B29: the transcript usage per UTC hour, home, provider, model,
-- conversation (Claude sessionId, Codex root thread id) and attribution
-- (session_id: the Fleet Deck session of the proven run, '' for none;
-- decided per record at ingestion). Kept forever.
CREATE TABLE analytics_usage (
    hour         INTEGER NOT NULL,
    home         TEXT    NOT NULL,
    provider     TEXT    NOT NULL,
    model        TEXT    NOT NULL,
    conversation TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    requests     INTEGER NOT NULL,
    input        INTEGER NOT NULL,
    cache_read   INTEGER NOT NULL,
    cache_write  INTEGER NOT NULL,
    output       INTEGER NOT NULL,
    reasoning    INTEGER NOT NULL,
    PRIMARY KEY (hour, home, provider, model, conversation, session_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX analytics_usage_session ON analytics_usage(session_id, hour);

-- B29: the dedupe keys of the counted records: the first 8 bytes of the
-- SHA-256 of the key text, as a signed integer. Kept forever.
CREATE TABLE analytics_seen (
    key INTEGER PRIMARY KEY
) STRICT;

-- B29: the Fleet Deck counters per UTC hour and session. Kept forever.
CREATE TABLE analytics_activity (
    hour       INTEGER NOT NULL,
    session_id TEXT    NOT NULL,
    metric     TEXT    NOT NULL,
    value      INTEGER NOT NULL,
    PRIMARY KEY (hour, session_id, metric)
) STRICT, WITHOUT ROWID;

-- B29: the open turns, tool calls and approvals, for their kind and duration.
CREATE TABLE analytics_open (
    session_id TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('turn', 'tool', 'approval')),
    id         TEXT    NOT NULL,
    started_at INTEGER NOT NULL,
    detail     TEXT,
    PRIMARY KEY (session_id, kind, id)
) STRICT, WITHOUT ROWID;

-- B29: the proven execution intervals of the Fleet Deck sessions: from a
-- session.started to the end of that process. ended_at NULL: open. Kept
-- forever.
CREATE TABLE analytics_exec (
    session_id TEXT    NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER,
    PRIMARY KEY (session_id, started_at)
) STRICT, WITHOUT ROWID;

-- B29: the workflows that the counters saw, owned by analytics (not the
-- workflow_runs projection). Kept forever.
CREATE TABLE analytics_workflows (
    session_id TEXT NOT NULL,
    task_id    TEXT NOT NULL,
    PRIMARY KEY (session_id, task_id)
) STRICT, WITHOUT ROWID;

-- B29: the one ordered event cursor. Every event with station_seq < next_seq
-- is counted, exactly once, in station_seq order. A new database has no meta
-- row yet when this runs: its first event is 1.
CREATE TABLE analytics_meta (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    events_since INTEGER,
    next_seq     INTEGER NOT NULL
) STRICT;
INSERT INTO analytics_meta (id, events_since, next_seq)
SELECT 1, (SELECT min(at) FROM events),
       COALESCE((SELECT min(station_seq) FROM events),
                (SELECT station_seq_next FROM meta WHERE id = 1),
                1);
";
