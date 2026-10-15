//! `deck.db` migrations (PLAN §10). Each entry is one transactional
//! migration; the list only grows (a shipped migration never changes).
//!
//! V1 (M2 P3-6a): `meta` (incarnation, revocation generation), the station
//! registry with its pins, grants and resync cursor, the session read model
//! and the condensed timeline. V2: link epochs (G1). V3 (P3-7): the
//! command router's `commands` table (G3). V4 (P3-7b): the outbox of
//! deck-owned intent (A5) and the receipts of `coordinator.call` (G7). Auth
//! and audit tables follow in P3-8 (V5). V6 (M3 6-6): the plan
//! library. V7 (M3 6-2, 6-4): mail and tasks. V8 (M3 6-7): file touches.
//! V13 (R2.1): the `blocked` and `question` task states. V14 (R12): artifacts.
//! V16 (R5): automations. V17 (R6): read cursors. V20 (ADR-008): fleet
//! control. V22 (DL-178): pairing completion records. V23 (DL-189): the
//! history option of a share.

use rusqlite_migration::{M, Migrations};

const V1: &str = "
CREATE TABLE meta (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    incarnation_id         TEXT    NOT NULL,
    revocation_generation  INTEGER NOT NULL DEFAULT 0 CHECK (revocation_generation >= 0),
    identity_key_ref       TEXT,
    created_at             INTEGER NOT NULL
) STRICT;

-- One paired station: its pin (key, role), the grants the deck gives it,
-- the link direction and the resync cursor of its event stream (G4).
-- `station_id` is the fingerprint at pairing and stays when the key
-- rotates (DL-049).
CREATE TABLE stations (
    station_id      TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL UNIQUE,
    key             TEXT    NOT NULL UNIQUE,
    fingerprint     TEXT    NOT NULL UNIQUE,
    role            TEXT    NOT NULL CHECK (role = 'station'),
    scopes          TEXT    NOT NULL,
    dial_url        TEXT,
    direction       TEXT    NOT NULL CHECK (direction IN ('deck_dials', 'station_dials')),
    paired_at       INTEGER NOT NULL,
    last_seen       INTEGER,
    incarnation_id  TEXT,
    cursor          INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0)
) STRICT;

-- The read model: the station's SessionSummary projection (JSON).
CREATE TABLE sessions (
    station_id  TEXT    NOT NULL REFERENCES stations(station_id) ON DELETE CASCADE,
    session_id  TEXT    NOT NULL,
    summary     TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
) STRICT;

-- The condensed per-session timeline for the board: every event except
-- the streaming deltas, capped per session (DL-049).
CREATE TABLE timeline (
    station_id   TEXT    NOT NULL REFERENCES stations(station_id) ON DELETE CASCADE,
    station_seq  INTEGER NOT NULL,
    session_id   TEXT    NOT NULL,
    session_seq  INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    envelope     TEXT    NOT NULL,
    PRIMARY KEY (station_id, station_seq)
) STRICT;

CREATE INDEX timeline_by_session ON timeline (station_id, session_id, station_seq);
";

/// The command router (G3, PLAN §8.4). `envelope` is the stored envelope:
/// it never changes, and every send sends these bytes. `attempts` counts
/// the sends; the first send sets `sent_at`.
const V3: &str = "
CREATE TABLE commands (
    command_id   TEXT PRIMARY KEY,
    station_id   TEXT NOT NULL,
    method       TEXT NOT NULL,
    origin       TEXT NOT NULL,
    envelope     TEXT NOT NULL,
    retry_until  INTEGER NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('pending', 'done', 'failed', 'ambiguous')),
    attempts     INTEGER NOT NULL DEFAULT 0,
    sent_at      INTEGER,
    receipt      TEXT,
    reason       TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX commands_pending ON commands (station_id, created_at) WHERE state = 'pending';
CREATE INDEX commands_by_station ON commands (station_id, created_at);
";

/// The outbox (A5) and the coordinator receipts (G7). An outbox item is
/// deck-owned intent for one station; each delivery attempt is one routed
/// command (`command_id`, in the same transaction). The receipts table
/// follows the station's `commands` table: same id + method + caller +
/// payload hash returns the stored receipt.
const V4: &str = "
CREATE TABLE outbox (
    outbox_id    TEXT PRIMARY KEY,
    station_id   TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('command')),
    origin       TEXT NOT NULL,
    payload      TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('queued', 'sending', 'delivered', 'dead')),
    command_id   TEXT UNIQUE REFERENCES commands (command_id),
    attempts     INTEGER NOT NULL DEFAULT 0,
    next_at      INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    reason       TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX outbox_due ON outbox (station_id, next_at) WHERE state = 'queued';
CREATE TABLE coordinator_receipts (
    command_id     TEXT PRIMARY KEY,
    station_id     TEXT NOT NULL,
    method         TEXT NOT NULL,
    caller         TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    receipt        TEXT NOT NULL,
    at             INTEGER NOT NULL
) STRICT;
";

/// Owner login and audit (P3-8; PLAN §9, A13, L15). Secrets are stored as
/// SHA-256 hashes (session cookies, login links, API tokens) or as an
/// argon2id PHC string (the password).
const V5: &str = "
CREATE TABLE owner (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    owner_id        TEXT NOT NULL,
    password_hash   TEXT,
    password_set_at INTEGER,
    created_at      INTEGER NOT NULL
) STRICT;
CREATE TABLE login_sessions (
    session_hash TEXT PRIMARY KEY,
    csrf_token   TEXT NOT NULL,
    method       TEXT NOT NULL CHECK (method IN ('password', 'link')),
    created_at   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
) STRICT;
CREATE TABLE login_links (
    token_hash TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE api_tokens (
    name       TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    scopes     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_used  INTEGER
) STRICT;
CREATE TABLE audit (
    audit_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    actor      TEXT NOT NULL,
    action     TEXT NOT NULL,
    target     TEXT,
    command_id TEXT,
    result     TEXT NOT NULL CHECK (result IN ('ok', 'refused', 'failed')),
    detail     TEXT
) STRICT;
";

/// The plan library (M3 6-6, DL-301): one row for each folded
/// `plan.proposed` event. No foreign key to `stations`: a plan is fleet
/// intent and stays when its station is removed or gets a new incarnation.
const V6: &str = "
CREATE TABLE plans (
    plan_key       TEXT    PRIMARY KEY,
    station_id     TEXT    NOT NULL,
    session_id     TEXT    NOT NULL,
    plan_id        TEXT    NOT NULL,
    turn_id        TEXT,
    session_title  TEXT,
    markdown       TEXT    NOT NULL,
    truncated      INTEGER NOT NULL CHECK (truncated IN (0, 1)),
    status         TEXT    NOT NULL CHECK (status IN ('proposed', 'executed', 'archived')),
    proposed_at    INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
) STRICT;
CREATE INDEX plans_by_time ON plans (proposed_at DESC);
";

/// Fleet mail (M3 6-2) and tasks with waitable completions (6-4). Each row
/// names the outbox item that delivers its text; the item is inserted in
/// the same transaction (A5).
const V7: &str = "
CREATE TABLE mail (
    mail_id    TEXT PRIMARY KEY,
    sender     TEXT NOT NULL,
    station_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    delivery   TEXT NOT NULL CHECK (delivery IN ('follow_up', 'steer')),
    text       TEXT NOT NULL,
    outbox_id  TEXT NOT NULL REFERENCES outbox (outbox_id),
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX mail_by_session ON mail (station_id, session_id, created_at);
CREATE TABLE tasks (
    task_id      TEXT PRIMARY KEY,
    title        TEXT NOT NULL,
    creator      TEXT NOT NULL,
    station_id   TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('open', 'done', 'failed')),
    result       TEXT,
    outbox_id    TEXT NOT NULL REFERENCES outbox (outbox_id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    completed_at INTEGER
) STRICT;
CREATE INDEX tasks_by_session ON tasks (station_id, session_id, created_at);
";

/// The conflict radar input (M3 6-7, DL-306; PLAN §10 `file_touches`): the
/// latest touch of a file by a session, keyed by the repository group.
/// Renumbered from V7 to V8 at the lane A merge (mail and tasks came first).
const V8: &str = "
CREATE TABLE file_touches (
    group_key      TEXT    NOT NULL,
    file           TEXT    NOT NULL,
    repo           TEXT,
    station_id     TEXT    NOT NULL,
    session_id     TEXT    NOT NULL,
    session_title  TEXT,
    path           TEXT    NOT NULL,
    touched_at     INTEGER NOT NULL,
    PRIMARY KEY (group_key, file, station_id, session_id)
) STRICT;
CREATE INDEX file_touches_by_time ON file_touches (touched_at);
";

/// Coordinator grants (M3 6-5, ADR-006 §4, PLAN §10 table `tokens`) and the
/// lineage of the sessions that a grant started (§6.3 `interrupt`/`stop`).
/// `spawn_used` is a durable lifetime counter (§6.4): receipt pruning never
/// changes it. Renumbered from V8 to V9 at the merge of lane C's V8.
const V9: &str = "
CREATE TABLE tokens (
    grant_id                TEXT    PRIMARY KEY,
    station_id              TEXT    NOT NULL,
    session_id              TEXT    NOT NULL,
    classes                 TEXT    NOT NULL,
    stations                TEXT    NOT NULL,
    spawn_cap               INTEGER NOT NULL CHECK (spawn_cap BETWEEN 0 AND 50),
    spawn_used              INTEGER NOT NULL DEFAULT 0 CHECK (spawn_used >= 0),
    max_modes               TEXT    NOT NULL,
    idle_ttl_ms             INTEGER NOT NULL CHECK (idle_ttl_ms BETWEEN 1 AND 86400000),
    created_at              INTEGER NOT NULL,
    expires_at              INTEGER NOT NULL,
    state                   TEXT    NOT NULL CHECK (state IN ('active', 'revoked', 'expired')),
    revoked_reason          TEXT,
    bound_generation        INTEGER,
    bound_native_id         TEXT,
    bound_provider_instance TEXT,
    binding_id              TEXT,
    last_used_at            INTEGER
) STRICT;
CREATE INDEX tokens_by_caller ON tokens (station_id, session_id, created_at);
CREATE TABLE coordinator_lineage (
    command_id      TEXT    PRIMARY KEY,
    grant_id        TEXT    NOT NULL REFERENCES tokens (grant_id),
    station_id      TEXT    NOT NULL,
    session_id      TEXT,
    start_outbox_id TEXT,
    created_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX lineage_by_session ON coordinator_lineage (station_id, session_id);
";

/// The owner's pending renewal of an idle-expired grant (ADR-006 §5.5,
/// test 25b): the generation that the station must acknowledge with
/// `coordinator.bound`. `NULL` when no renewal waits.
const V10: &str = "ALTER TABLE tokens ADD COLUMN renew_generation INTEGER;";

/// The owner's start command of a grant (ADR-006 §4, DL-217): a grant
/// created with a session start has no session id until the station
/// names the session; a resend of the same start finds the same grant.
const V11: &str = "
ALTER TABLE tokens ADD COLUMN start_command_id TEXT;
CREATE UNIQUE INDEX tokens_by_start_command ON tokens (start_command_id)
    WHERE start_command_id IS NOT NULL;
";

/// The archived (cleared) sessions of the board (DL-149) and the ids of the
/// archive mutations that were applied (never pruned: a stale resend of an
/// old mutation must stay a no-op).
const V12: &str = "
CREATE TABLE archived_sessions (
    station_id TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    archived   INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
);
CREATE TABLE archive_mutations (
    mutation_id TEXT    PRIMARY KEY,
    applied_at  INTEGER NOT NULL
);
";

/// Task completion kinds `blocked` and `question` (R2.1, DL-160): the state
/// check of `tasks` grows, and a task keeps the reason of its completion
/// the question of a worker, and the coordinator that assigned it. SQLite cannot change a CHECK in place, so
/// the table is rebuilt with its rows.
const V13: &str = "
CREATE TABLE tasks_v13 (
    task_id           TEXT PRIMARY KEY,
    title             TEXT NOT NULL,
    creator           TEXT NOT NULL,
    station_id        TEXT NOT NULL,
    session_id        TEXT NOT NULL,
    state             TEXT NOT NULL
        CHECK (state IN ('open', 'done', 'failed', 'blocked', 'question')),
    result            TEXT,
    completion_reason TEXT,
    question          TEXT,
    -- The coordinator session that created the task (an `ask_coordinator`
    -- mail goes there); both NULL for a task of the owner.
    coordinator_station TEXT,
    coordinator_session TEXT,
    outbox_id         TEXT NOT NULL REFERENCES outbox (outbox_id),
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    completed_at      INTEGER
) STRICT;
INSERT INTO tasks_v13 (task_id, title, creator, station_id, session_id, state, result,
        outbox_id, created_at, updated_at, completed_at)
    SELECT task_id, title, creator, station_id, session_id, state, result,
        outbox_id, created_at, updated_at, completed_at FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_v13 RENAME TO tasks;
CREATE INDEX tasks_by_session ON tasks (station_id, session_id, created_at);
";

/// Artifacts (R12, DL-165): the index that the folded `artifact.published`
/// events feed, the version metadata, the byte cache that a share or a view
/// fills (so a share works while the station is off), and the share links.
/// A share row keeps the SHA-256 of its token, never the token. `kind` is
/// `share` (the owner's link) or `view` (a short link for the board's own
/// viewer). No foreign key to `stations`: the cache and the shares stay
/// when the station is removed.
const V14: &str = "
CREATE TABLE artifacts (
    station_id     TEXT    NOT NULL,
    artifact_id    TEXT    NOT NULL,
    session_id     TEXT,
    title          TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    latest_version INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (station_id, artifact_id)
);
CREATE INDEX artifacts_by_time ON artifacts (updated_at DESC);
CREATE INDEX artifacts_by_session ON artifacts (station_id, session_id);
CREATE TABLE artifact_versions (
    station_id  TEXT    NOT NULL,
    artifact_id TEXT    NOT NULL,
    version     INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    sha256      TEXT    NOT NULL,
    files       TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (station_id, artifact_id, version)
);
CREATE TABLE artifact_cache (
    station_id  TEXT    NOT NULL,
    artifact_id TEXT    NOT NULL,
    version     INTEGER NOT NULL,
    path        TEXT    NOT NULL,
    bytes       BLOB    NOT NULL,
    PRIMARY KEY (station_id, artifact_id, version, path)
);
CREATE TABLE artifact_shares (
    token_hash  TEXT    PRIMARY KEY,
    share_id    TEXT    NOT NULL UNIQUE,
    station_id  TEXT    NOT NULL,
    artifact_id TEXT    NOT NULL,
    version     INTEGER,
    kind        TEXT    NOT NULL CHECK (kind IN ('share', 'view')),
    created_by  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    revoked_at  INTEGER,
    views       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX artifact_shares_by_artifact ON artifact_shares (station_id, artifact_id);
";

/// The automations (R5, DL-168): the schedules that the deck owns and the
/// run of each due time. `UNIQUE (schedule_id, due_at)` makes a run
/// idempotent: a restart or a second tick finds the row and does nothing.
/// A `manual` run ("run now") is not a due time: it never moves the cursor.
/// `armed_at` is the time since which due times count: a new schedule, or
/// one that the owner turns on again or gives another cron or time zone,
/// does not run for the time before. A run row is never changed back: a
/// crash between the claim and the end leaves `started` (at most once).
const V16: &str = "
CREATE TABLE schedules (
    schedule_id       TEXT    PRIMARY KEY,
    name              TEXT    NOT NULL,
    enabled           INTEGER NOT NULL,
    cron              TEXT    NOT NULL,
    timezone          TEXT    NOT NULL,
    station_id        TEXT    NOT NULL,
    project_id        TEXT,
    path              TEXT    NOT NULL,
    provider_instance TEXT    NOT NULL,
    mode              TEXT,
    model             TEXT,
    prompt            TEXT    NOT NULL,
    precheck_cmd      TEXT,
    reuse_session     INTEGER NOT NULL,
    last_station      TEXT,
    last_session      TEXT,
    grace_ms          INTEGER NOT NULL DEFAULT 3600000,
    created_at        INTEGER NOT NULL,
    armed_at          INTEGER NOT NULL
) STRICT;

CREATE TABLE schedule_runs (
    run_id      TEXT    PRIMARY KEY,
    schedule_id TEXT    NOT NULL REFERENCES schedules(schedule_id) ON DELETE CASCADE,
    due_at      INTEGER NOT NULL,
    started_at  INTEGER NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN ('started', 'skipped', 'failed', 'missed')),
    reason      TEXT    NOT NULL DEFAULT '',
    station_id  TEXT    NOT NULL,
    session_id  TEXT,
    manual      INTEGER NOT NULL DEFAULT 0,
    UNIQUE (schedule_id, due_at)
) STRICT;
";

/// The read cursors of the board (R6.1, DL-169): the highest `session_seq`
/// that a caller has seen of a session. `principal` is `owner` or the actor
/// name of another caller. No foreign key: a cursor outlives a snapshot that
/// drops and re-adds the session. The owner starts with every stored session
/// read, so an upgrade shows no unread dots. The timeline gets its event
/// time as an indexed column for the activity stream (R6.5).
const V17: &str = "
CREATE TABLE read_cursors (
    principal   TEXT    NOT NULL,
    station_id  TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    seq         INTEGER NOT NULL CHECK (seq >= 0),
    at          INTEGER NOT NULL,
    PRIMARY KEY (principal, station_id, session_id)
) STRICT;
INSERT INTO read_cursors (principal, station_id, session_id, seq, at)
    SELECT 'owner', station_id, session_id, MAX(session_seq),
           CAST(strftime('%s', 'now') AS INTEGER) * 1000
    FROM timeline GROUP BY station_id, session_id;
ALTER TABLE timeline ADD COLUMN at INTEGER GENERATED ALWAYS AS (json_extract(envelope, '$.at')) VIRTUAL;
CREATE INDEX timeline_by_time ON timeline (at DESC, station_seq DESC);
";

/// The projects of the board (R3, DL-164): a repository on a station with
/// its forge and tracker bindings (JSON) and the prompts for sessions that
/// start from a ticket or a review. No token is ever stored here.
const V18: &str = "
CREATE TABLE projects (
    project_id      TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    station_id      TEXT    NOT NULL,
    path            TEXT    NOT NULL,
    forge           TEXT,
    trackers        TEXT    NOT NULL DEFAULT '[]',
    prompt_template TEXT,
    review_prompt   TEXT,
    created_at      INTEGER NOT NULL
);
CREATE INDEX projects_by_station ON projects (station_id);
";

/// The shared fleet (R11, DL-166): spaces, shares, invites, local members and
/// their logins, the side chat, and the mob-mode driver with its requests.
/// Only hashes of secrets are stored (the invite token, the member cookie).
/// `commands.author` is who sent a command (`owner`, `member:<name>`,
/// `token:<name>`); `NULL` for a command that the deck made itself.
const V15: &str = "
CREATE TABLE spaces (
    space_id   TEXT    PRIMARY KEY,
    name       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE space_sessions (
    space_id   TEXT NOT NULL REFERENCES spaces (space_id) ON DELETE CASCADE,
    station_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    PRIMARY KEY (space_id, station_id, session_id)
) STRICT;
CREATE TABLE shares (
    share_id   TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK (kind IN ('session', 'space', 'board')),
    target     TEXT    NOT NULL,
    role       TEXT    NOT NULL CHECK (role IN ('viewer', 'collaborator', 'approver')),
    steering   TEXT    NOT NULL CHECK (steering IN ('open', 'mob')),
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE invites (
    token_hash TEXT    PRIMARY KEY,
    share_id   TEXT    NOT NULL REFERENCES shares (share_id) ON DELETE CASCADE,
    uses_left  INTEGER NOT NULL CHECK (uses_left >= 0),
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX invites_by_share ON invites (share_id);
CREATE TABLE members (
    member_id    TEXT    PRIMARY KEY,
    share_id     TEXT    NOT NULL REFERENCES shares (share_id) ON DELETE CASCADE,
    display_name TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE (share_id, display_name)
) STRICT;
CREATE TABLE member_sessions (
    session_hash TEXT    PRIMARY KEY,
    member_id    TEXT    NOT NULL REFERENCES members (member_id) ON DELETE CASCADE,
    share_id     TEXT    NOT NULL REFERENCES shares (share_id) ON DELETE CASCADE,
    csrf_token   TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX member_sessions_by_share ON member_sessions (share_id);
CREATE TABLE session_chat (
    station_id TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    author     TEXT    NOT NULL,
    text       TEXT    NOT NULL,
    at         INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id, seq)
) STRICT;
CREATE TABLE drivers (
    station_id      TEXT    NOT NULL,
    session_id      TEXT    NOT NULL,
    member_or_owner TEXT    NOT NULL,
    since           INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
) STRICT;
CREATE TABLE drive_requests (
    station_id TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    requester  TEXT    NOT NULL,
    at         INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id, requester)
) STRICT;
ALTER TABLE commands ADD COLUMN author TEXT;
";

/// The project of a session (DL-176): a manual choice of the owner. A row
/// with a NULL `project_id` means "no project" and overrides the automatic
/// match by folder; no row means the automatic match.
const V19: &str = "
CREATE TABLE project_sessions (
    station_id  TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    project_id  TEXT    REFERENCES projects (project_id) ON DELETE CASCADE,
    assigned_at INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
) STRICT;
";

/// Fleet control (ADR-008 §3, §6.1). A grant has a `scope` and an answer
/// budget with a durable lifetime counter (like `spawn_used`). An outbox item
/// made by a fleet-control op keeps its `authority` (JSON of
/// `CommandAuthority`); every outbox item of a coordinator call keeps the id
/// of that call, so `command_status` can show its deliveries.
const V20: &str = "
ALTER TABLE tokens ADD COLUMN scope TEXT NOT NULL DEFAULT 'lineage'
    CHECK (scope IN ('lineage', 'fleet'));
ALTER TABLE tokens ADD COLUMN answer_cap INTEGER
    CHECK (answer_cap IS NULL OR answer_cap BETWEEN 0 AND 5000);
ALTER TABLE tokens ADD COLUMN answer_used INTEGER NOT NULL DEFAULT 0
    CHECK (answer_used >= 0);
ALTER TABLE outbox ADD COLUMN authority TEXT;
ALTER TABLE outbox ADD COLUMN call_id TEXT;
CREATE INDEX outbox_call ON outbox (call_id) WHERE call_id IS NOT NULL;
";

/// Review comments on the Changes diff (DL-179, design `review-comments.md`
/// section 1). Named V21 next to the V20 of another goal branch; the list
/// position decides at the merge. No foreign key joins these tables to
/// `sessions`: the snapshot path deletes and inserts the session rows, and
/// the cleanup code deletes the review rows of a session that is gone.
///
/// - `review_attempts`: one row per "Send again" (`attempt_key`), so a repeat
///   of the same attempt returns its stored result and makes no second
///   command (design 3, step 6).
/// - `review_imports`: one row per imported local note (`import_key`), with
///   the comment that it made, so a repeated import makes no duplicate (7).
const V21: &str = "
CREATE TABLE reviews (
    review_id     TEXT    PRIMARY KEY,
    short_id      TEXT    NOT NULL,
    station_id    TEXT    NOT NULL,
    session_id    TEXT    NOT NULL,
    send_key      TEXT    NOT NULL,
    fingerprint   TEXT    NOT NULL,
    principal     TEXT    NOT NULL,
    command_id    TEXT    NOT NULL,
    delivery      TEXT    NOT NULL CHECK (delivery IN ('pending', 'delivered', 'failed', 'unknown')),
    attempts      INTEGER NOT NULL DEFAULT 1 CHECK (attempts BETWEEN 1 AND 5),
    input_id      TEXT,
    reason        TEXT,
    comment_count INTEGER NOT NULL,
    sent_at       INTEGER NOT NULL,
    UNIQUE (station_id, session_id, principal, send_key),
    UNIQUE (station_id, session_id, short_id)
) STRICT;
CREATE INDEX reviews_command ON reviews (command_id);
CREATE TABLE review_comments (
    comment_id   TEXT    PRIMARY KEY,
    short_id     TEXT    NOT NULL,
    station_id   TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    thread_id    TEXT    NOT NULL,
    snapshot     TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    other_path   TEXT,
    side         TEXT    NOT NULL CHECK (side IN ('new', 'old')),
    line_start   INTEGER NOT NULL CHECK (line_start >= 1),
    line_end     INTEGER NOT NULL CHECK (line_end >= line_start AND line_end - line_start < 200),
    anchor_text  TEXT    NOT NULL,
    anchor_kind  TEXT    NOT NULL CHECK (anchor_kind IN ('diff', 'imported')),
    body         TEXT    NOT NULL,
    author_kind  TEXT    NOT NULL CHECK (author_kind IN ('owner', 'member')),
    author_name  TEXT,
    state        TEXT    NOT NULL CHECK (state IN ('draft', 'sent')),
    resolved     INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
    review_id    TEXT    REFERENCES reviews (review_id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (station_id, session_id, short_id)
) STRICT;
CREATE INDEX review_comments_session ON review_comments (station_id, session_id, thread_id);
CREATE INDEX review_comments_review ON review_comments (review_id);
CREATE TABLE review_revs (
    station_id TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    rev        INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
) STRICT;
CREATE TABLE review_imports (
    station_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    principal  TEXT NOT NULL,
    import_key TEXT NOT NULL,
    comment_id TEXT NOT NULL,
    PRIMARY KEY (station_id, session_id, principal, import_key)
) STRICT;
CREATE TRIGGER review_comments_short_id_is_immutable
BEFORE UPDATE OF short_id ON review_comments
WHEN OLD.short_id <> NEW.short_id
BEGIN
    SELECT RAISE(ABORT, 'a short id never changes');
END;
CREATE TRIGGER reviews_short_id_is_immutable
BEFORE UPDATE OF short_id ON reviews
WHEN OLD.short_id <> NEW.short_id
BEGIN
    SELECT RAISE(ABORT, 'a short id never changes');
END;
CREATE TABLE review_attempts (
    review_id   TEXT    NOT NULL REFERENCES reviews (review_id),
    attempt_key TEXT    NOT NULL,
    command_id  TEXT    NOT NULL,
    attempt     INTEGER NOT NULL,
    PRIMARY KEY (review_id, attempt_key)
) STRICT;
";

/// Pairing completion records (DL-178, tailnet design 5.4). One row for each
/// approved pairing request, written in the transaction that stores the
/// station, and deleted when the station is removed, paired again or its key
/// rotates. It holds no secret: a request id, two public keys, a nonce and
/// digest of the request, the requested name and the two scope lists. To
/// reverse the migration, drop the table: the only loss is the retry of a
/// lost `PairApproved` (the station can then be removed and asked again).
const V22: &str = "
CREATE TABLE pairing_completions (
    request_id        TEXT    PRIMARY KEY,
    station_key       TEXT    NOT NULL,
    deck_key          TEXT    NOT NULL,
    request_digest    TEXT    NOT NULL,
    nonce             TEXT    NOT NULL,
    requested_name    TEXT    NOT NULL,
    offered_scopes    TEXT    NOT NULL,
    granted_scopes    TEXT    NOT NULL,
    window_generation INTEGER NOT NULL,
    approved_at       INTEGER NOT NULL
) STRICT;
CREATE INDEX pairing_completions_station ON pairing_completions (station_key);
";

/// The history option of a share (DL-189): members of the share can read the
/// earlier history of imported sessions. Every existing share reads 0 (off).
/// To reverse the migration, drop the column.
const V23: &str = "
ALTER TABLE shares ADD COLUMN history INTEGER NOT NULL DEFAULT 0 CHECK (history IN (0, 1));
";

/// The `deck.db` migrations.
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(V1),
        M::up(fleetdeck_link::LINK_EPOCHS_SQL),
        M::up(V3),
        M::up(V4),
        M::up(V5),
        M::up(V6),
        M::up(V7),
        M::up(V8),
        M::up(V9),
        M::up(V10),
        M::up(V11),
        M::up(V12),
        M::up(V13),
        M::up(V14),
        // R5 is named V16 so that it does not clash with V13 to V15 of the
        // other v2 goal branches; the list position decides at the merge.
        M::up(V16),
        M::up(V17),
        M::up(V18),
        M::up(V15),
        M::up(V19),
        M::up(V20),
        M::up(V21),
        M::up(V22),
        M::up(V23),
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_migrations_are_valid() {
        super::migrations().validate().unwrap();
    }

    #[test]
    fn an_existing_share_reads_history_off_after_v23() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let migrations = super::migrations();
        migrations.to_version(&mut conn, 22).unwrap();
        conn.execute(
            "INSERT INTO shares (share_id, kind, target, role, steering, expires_at, created_at)
             VALUES ('sh1', 'board', '', 'viewer', 'open', 2, 1)",
            [],
        )
        .unwrap();
        migrations.to_latest(&mut conn).unwrap();
        let history: i64 = conn
            .query_row(
                "SELECT history FROM shares WHERE share_id = 'sh1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(history, 0);
        // The column holds 0 or 1 only.
        assert!(conn.execute("UPDATE shares SET history = 2", []).is_err());
    }
}
