//! `station.db` migrations (PLAN §10). Each entry is one transactional
//! migration; the list only grows (a shipped migration never changes).
//!
//! V1 (M2 P2-4): `meta`, `pairing`, `deck_acks`, `sessions`, `events`,
//! `commands`, `shells`. V2 (M2 P2-5): `processes`, `effects`,
//! `runtime_requests` and the worker fence. V3 (M2 P3-5): pins, grants and
//! the revocation generation (G5). V4: link epochs (G1). V5 (M2 P3-5c):
//! the listen addresses that `station invite --listen` added (DL-048). The
//! workspace tables follow in Phase 6.

use rusqlite_migration::{M, Migrations};

const V1: &str = "
CREATE TABLE meta (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    incarnation_id    TEXT    NOT NULL,
    station_seq_next  INTEGER NOT NULL CHECK (station_seq_next >= 1),
    -- The highest station_seq that retention removed (0: none).
    pruned_through    INTEGER NOT NULL DEFAULT 0,
    identity_key_ref  TEXT,
    created_at        INTEGER NOT NULL
) STRICT;

CREATE TABLE pairing (
    deck_key   TEXT PRIMARY KEY,
    deck_url   TEXT,
    paired_at  INTEGER NOT NULL
) STRICT;

-- The highest station_seq that a paired deck acknowledged.
CREATE TABLE deck_acks (
    deck_key     TEXT PRIMARY KEY REFERENCES pairing(deck_key) ON DELETE CASCADE,
    station_seq  INTEGER NOT NULL,
    at           INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    session_id               TEXT PRIMARY KEY,
    provider                 TEXT NOT NULL,
    provider_instance        TEXT NOT NULL,
    native_session_id        TEXT,
    cwd                      TEXT NOT NULL,
    worktree                 TEXT,
    mode                     TEXT,
    state                    TEXT NOT NULL,
    offline_approval_policy  TEXT NOT NULL,
    -- The last allocated session_seq.
    session_seq              INTEGER NOT NULL DEFAULT 0,
    -- The SessionSummary projection (JSON), folded in each event's transaction.
    summary                  TEXT NOT NULL,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL
) STRICT;

CREATE TABLE events (
    station_seq  INTEGER PRIMARY KEY,
    session_id   TEXT    NOT NULL REFERENCES sessions(session_id),
    session_seq  INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    payload      TEXT    NOT NULL,
    raw_ref      TEXT,
    at           INTEGER NOT NULL,
    UNIQUE (session_id, session_seq)
) STRICT;

-- Command receipts (§6.4, G3).
CREATE TABLE commands (
    command_id     TEXT PRIMARY KEY,
    method         TEXT NOT NULL,
    caller         TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('accepted', 'rejected')),
    station_seq    INTEGER,
    receipt        TEXT NOT NULL,
    at             INTEGER NOT NULL
) STRICT;
CREATE INDEX commands_at ON commands(at);

CREATE TABLE shells (
    shell_id    TEXT PRIMARY KEY,
    cwd         TEXT NOT NULL,
    state       TEXT NOT NULL,
    created_at  INTEGER NOT NULL
) STRICT;
";

const V2: &str = "
-- The fence token of the current effect worker (PLAN §6.4). Each worker
-- start takes the next value; a claim or settle with an older token fails.
ALTER TABLE meta ADD COLUMN worker_fence INTEGER NOT NULL DEFAULT 0;

-- Provider processes (provider incarnations): one row per spawned process.
CREATE TABLE processes (
    process_incarnation  TEXT PRIMARY KEY,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    pid                  INTEGER,
    state                TEXT    NOT NULL CHECK (state IN ('live', 'dead')),
    started_at           INTEGER NOT NULL,
    ended_at             INTEGER,
    reason               TEXT
) STRICT;
CREATE INDEX processes_session ON processes(session_id, state);

-- The effect outbox (§6.4). seq orders the worker.
CREATE TABLE effects (
    seq                  INTEGER PRIMARY KEY AUTOINCREMENT,
    effect_id            TEXT    NOT NULL UNIQUE,
    command_id           TEXT,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    -- The target process; NULL for an effect that starts a process.
    process_incarnation  TEXT REFERENCES processes(process_incarnation),
    kind                 TEXT    NOT NULL,
    payload              TEXT    NOT NULL,
    -- The provider evidence that confirms the effect (input id, request id, mode).
    correlation          TEXT,
    state                TEXT    NOT NULL CHECK (state IN
        ('queued', 'claimed', 'confirmed', 'retired_not_dispatched', 'outcome_unknown')),
    fence_token          INTEGER,
    reason               TEXT,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX effects_state ON effects(state, seq);
CREATE INDEX effects_correlation ON effects(session_id, correlation);

-- Runtime requests (approvals, questions) with the provider's option ids (L3).
CREATE TABLE runtime_requests (
    request_id           TEXT PRIMARY KEY,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    process_incarnation  TEXT REFERENCES processes(process_incarnation),
    node_id              TEXT,
    kind                 TEXT    NOT NULL,
    options              TEXT    NOT NULL,
    response_channel     TEXT    NOT NULL,
    state                TEXT    NOT NULL CHECK (state IN ('open', 'resolved', 'expired')),
    -- The first answer (JSON AnswerReceipt): a repeated answer returns it.
    receipt              TEXT,
    -- deny_after (§6.5): the time of the automatic deny; NULL for wait.
    deny_at              INTEGER,
    requested_at         INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX runtime_requests_open ON runtime_requests(state, deny_at);
";

const V3: &str = "
-- G5: the pin (key and role) and the grants of each paired key, and a
-- revocation generation that every revocation and rotation increments.
ALTER TABLE meta ADD COLUMN revocation_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pairing ADD COLUMN fingerprint TEXT;
ALTER TABLE pairing ADD COLUMN role TEXT NOT NULL DEFAULT 'deck'
    CHECK (role IN ('deck', 'station'));
-- JSON array of scope names.
ALTER TABLE pairing ADD COLUMN scopes TEXT NOT NULL DEFAULT '[]';
CREATE UNIQUE INDEX pairing_fingerprint ON pairing(fingerprint);
";

const V5: &str = "
-- DL-048: a listen address that `station invite --listen` opened; the
-- station listens on it again after a restart (with `station.toml` listen).
CREATE TABLE listen_addrs (
    addr      TEXT PRIMARY KEY,
    added_at  INTEGER NOT NULL
) STRICT;
";

const V6: &str = "
-- DL-082: one row per app turn (station ULID, the hidden ref key) with the
-- two checkpoint sides; the row is written before a ref is published, and
-- `turn.finalized` is committed with `state = 'finalized'` (PLAN §6.6).
CREATE TABLE turn_checkpoints (
    app_turn       TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL REFERENCES sessions(session_id),
    process        TEXT,
    input_id       TEXT,
    turn_id        TEXT,
    worktree_id    TEXT,
    -- The canonical worktree root at capture time (recovery reopens it).
    worktree_root  TEXT,
    scope          TEXT NOT NULL,
    start_state    TEXT NOT NULL
        CHECK (start_state IN ('capturing', 'available', 'unavailable')),
    start_value    TEXT,
    end_state      TEXT NOT NULL
        CHECK (end_state IN ('open', 'capturing', 'available', 'unavailable')),
    end_value      TEXT,
    state          TEXT NOT NULL CHECK (state IN ('open', 'finalized', 'abandoned')),
    finalized_seq  INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX turn_checkpoints_input
    ON turn_checkpoints(session_id, input_id) WHERE input_id IS NOT NULL;
CREATE UNIQUE INDEX turn_checkpoints_turn
    ON turn_checkpoints(session_id, process, turn_id) WHERE turn_id IS NOT NULL;
CREATE INDEX turn_checkpoints_open ON turn_checkpoints(state) WHERE state = 'open';
";

const V7: &str = "
-- DL-087: one row per revert (PLAN §6.6). The row is the durable revert
-- fence: the partial unique index admits one active revert per session, and
-- each external step (conversation rollback, file restore) is committed
-- before and after its I/O so recovery works from evidence.
CREATE TABLE reverts (
    revert_id         TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL REFERENCES sessions(session_id),
    process           TEXT NOT NULL,
    turn_id           TEXT NOT NULL,
    -- The first reverted app turn: its start checkpoint is the target.
    app_turn          TEXT NOT NULL REFERENCES turn_checkpoints(app_turn),
    -- The last finalized app turn: its completion checkpoint is the
    -- workspace state that the revert expects before the restore.
    current_app_turn  TEXT NOT NULL REFERENCES turn_checkpoints(app_turn),
    turns             INTEGER NOT NULL,
    target_commit     TEXT NOT NULL,
    current_commit    TEXT NOT NULL,
    worktree_root     TEXT NOT NULL,
    step              TEXT NOT NULL CHECK (step IN ('admitted', 'rollback_sent',
        'rolled_back', 'restoring', 'succeeded', 'failed', 'reconciliation_required')),
    reason            TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX reverts_active ON reverts(session_id) WHERE step IN
    ('admitted', 'rollback_sent', 'rolled_back', 'restoring', 'reconciliation_required');
-- The revert that removed the turn from the conversation and the workspace.
ALTER TABLE turn_checkpoints ADD COLUMN reverted_by TEXT;
";

const V8: &str = "
-- DL-097: the owner's resolution of a revert in `reconciliation_required`.
-- The step list gains `reconciled` (terminal), and `rolled_back_at` records
-- that the provider confirmed the conversation rollback, so a retried file
-- restore never depends on a reason text. SQLite cannot change a CHECK, so
-- the table is rebuilt; no other table references it.
CREATE TABLE reverts_v8 (
    revert_id         TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL REFERENCES sessions(session_id),
    process           TEXT NOT NULL,
    turn_id           TEXT NOT NULL,
    app_turn          TEXT NOT NULL REFERENCES turn_checkpoints(app_turn),
    current_app_turn  TEXT NOT NULL REFERENCES turn_checkpoints(app_turn),
    turns             INTEGER NOT NULL,
    target_commit     TEXT NOT NULL,
    current_commit    TEXT NOT NULL,
    worktree_root     TEXT NOT NULL,
    step              TEXT NOT NULL CHECK (step IN ('admitted', 'rollback_sent',
        'rolled_back', 'restoring', 'succeeded', 'failed', 'reconciliation_required',
        'reconciled')),
    reason            TEXT,
    rolled_back_at    INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
) STRICT;
-- Before V8 only `rollback_uncertain` left a revert in
-- `reconciliation_required` without a confirmed rollback.
INSERT INTO reverts_v8 SELECT revert_id, session_id, process, turn_id, app_turn,
    current_app_turn, turns, target_commit, current_commit, worktree_root, step, reason,
    CASE WHEN step IN ('rolled_back', 'restoring', 'succeeded')
        OR (step = 'reconciliation_required' AND reason IS NOT 'rollback_uncertain')
        THEN updated_at END,
    created_at, updated_at
    FROM reverts;
DROP TABLE reverts;
ALTER TABLE reverts_v8 RENAME TO reverts;
CREATE UNIQUE INDEX reverts_active ON reverts(session_id) WHERE step IN
    ('admitted', 'rollback_sent', 'rolled_back', 'restoring', 'reconciliation_required');
";

const V9: &str = "
-- ADR-005 §3 (slice 4, DL-104): one row per shim. The generation is
-- committed before each attach; the cursor is a committed stdout line
-- boundary; a running shim always has its provider identity (JSON).
CREATE TABLE shims (
    shim_id              TEXT PRIMARY KEY,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    incarnation          TEXT    NOT NULL,
    state                TEXT    NOT NULL CHECK (state IN
        ('starting', 'running', 'exited', 'never_started', 'lost')),
    owner_generation     INTEGER NOT NULL DEFAULT 0,
    socket_path          TEXT    NOT NULL,
    provider_identity    TEXT,
    cursor               INTEGER NOT NULL DEFAULT 0,
    next_write_seq       INTEGER NOT NULL DEFAULT 1,
    reason               TEXT,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL,
    CHECK (state <> 'running' OR provider_identity IS NOT NULL)
) STRICT;
CREATE INDEX shims_state ON shims(state, created_at);
";

const V10: &str = "
-- ADR-005 §4.3 (slice 5, DL-105): the durable identity of every stdin frame
-- of a shim, claimed before the frame leaves. `effect_id` is NULL for an
-- adapter-internal write. A session whose shim reported a partial write
-- takes no input until the owner stops it (`input_blocked`).
CREATE TABLE shim_writes (
    shim_id      TEXT    NOT NULL REFERENCES shims(shim_id),
    write_seq    INTEGER NOT NULL,
    write_id     TEXT    NOT NULL,
    effect_id    TEXT REFERENCES effects(effect_id),
    ordinal      INTEGER NOT NULL,
    generation   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (shim_id, write_seq),
    UNIQUE (shim_id, write_id)
) STRICT;
CREATE INDEX shim_writes_effect ON shim_writes(effect_id);
ALTER TABLE shims ADD COLUMN input_blocked TEXT;
";

const V11: &str = "
-- ADR-005 §4.3 (DL-107): the complete frame count of the effect, stored
-- with frame 0 before any byte leaves. Recovery treats every frame without
-- a row as never sent, so an effect whose frame 0 was written and whose
-- frame 1 was never claimed blocks input instead of looking complete.
ALTER TABLE shim_writes ADD COLUMN frames INTEGER NOT NULL DEFAULT 1;
";

const V12: &str = "
-- ADR-005 §2.1 and §6 (DL-108): the latest committed adapter checkpoint of
-- a shim, written in the claim transaction of an outbound frame and in the
-- output transaction; and whether the request time of a runtime request is
-- proven (live read time or the shim's read_at). An unprovable time makes a
-- deny_after request due at once. Old rows used the live read time.
ALTER TABLE shims ADD COLUMN adapter_checkpoint TEXT;
ALTER TABLE runtime_requests ADD COLUMN time_proven INTEGER NOT NULL DEFAULT 1;
";

const V13: &str = "
-- ADR-005 §2.1 (DL-110): the durable driver of a shim session.
-- shim_frames: adapter-generated stdin frames (for example `initialized`,
-- `thread/start`, a queued follow-up's `turn/start`), committed with the
-- output transaction that generated them. A frame is claimed (a shim_writes
-- row) before it leaves; recovery sends `pending` frames, asks the shim about
-- `claimed` ones, and never resends a frame that may have been written.
-- shim_transitions: the command effects applied to the adapter after its
-- latest checkpoint, committed before their frames are claimed; an output
-- checkpoint covers and removes them.
CREATE TABLE shim_frames (
    shim_id     TEXT    NOT NULL REFERENCES shims(shim_id),
    frame_seq   INTEGER NOT NULL,
    payload     TEXT    NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN ('pending', 'claimed', 'done')),
    write_id    TEXT,
    write_seq   INTEGER,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (shim_id, frame_seq)
);
CREATE TABLE shim_transitions (
    shim_id     TEXT    NOT NULL REFERENCES shims(shim_id),
    seq         INTEGER NOT NULL,
    effect_id   TEXT    NOT NULL REFERENCES effects(effect_id),
    frames      INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (shim_id, seq)
);
";

const V14: &str = "
-- DL-124: adapter invalidation is independent of the first input-block reason.
ALTER TABLE shims ADD COLUMN adapter_gap INTEGER NOT NULL DEFAULT 0;
-- Preserve pre-V14 invalidation, including a gap hidden by an earlier block.
-- A stale output transaction could have restored the checkpoint after the gap.
-- Do not rewrite historical events or the original input-block reason.
UPDATE shims SET adapter_gap = 1, adapter_checkpoint = NULL
WHERE input_blocked = 'adapter_state_gap' OR EXISTS (
    SELECT 1 FROM events
    WHERE events.session_id = shims.session_id AND events.at >= shims.created_at
      AND events.kind = 'error'
      AND json_extract(events.payload, '$.code') = 'adapter_state_gap'
);
";

const V15: &str = "
-- M3 7-3 (DL-128): the shim protocol version that each shim row speaks, so
-- that an upgrade trial reads it without an attach (ADR-005 §3.3, §5.3).
-- Every row before this version speaks protocol 1.
ALTER TABLE shims ADD COLUMN protocol INTEGER NOT NULL DEFAULT 1;
";

const V16: &str = "
-- DL-405 (M3 6-11; renumbered from V15 to V16 at the merge with lane A's V15): the durable effects of the human shell. A shell
-- command commits its receipt and one `queued` row; the dispatcher claims the
-- row before the PTY I/O and ends it as `confirmed` or `failed`. Station
-- start recovery retires `queued` rows and marks `claimed` rows
-- `outcome_unknown`: a station restart ends every shell (A10), so a shell
-- effect is never dispatched by a later process.
CREATE TABLE shell_effects (
    effect_id   TEXT    PRIMARY KEY,
    command_id  TEXT    NOT NULL UNIQUE,
    shell_id    TEXT    NOT NULL,
    op          TEXT    NOT NULL CHECK (op IN ('open', 'resize', 'close')),
    change      TEXT    NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN ('queued', 'claimed', 'confirmed', 'failed',
                                                  'retired_not_dispatched', 'outcome_unknown')),
    detail      TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX shell_effects_open ON shell_effects (state, created_at)
    WHERE state IN ('queued', 'claimed');
";

const V17: &str = "
-- DL-218 (M3 6-5, ADR-006 §6.5): the durable station coordinator outbox. A
-- coordinator tool call that maps to `coordinator.call` stores its complete
-- envelope here before any send; the envelope never changes (G3). `sent_at`
-- commits before each network write, so recovery never takes a possibly sent
-- row for an unsent one. `sends` counts the transmissions.
CREATE TABLE coordinator_outbox (
    command_id      TEXT    PRIMARY KEY,
    binding_id      TEXT    NOT NULL,
    generation      INTEGER NOT NULL,
    idempotency_key TEXT,
    args_digest     TEXT    NOT NULL,
    envelope        TEXT    NOT NULL,
    state           TEXT    NOT NULL CHECK (state IN ('pending', 'done', 'failed', 'ambiguous')),
    reason          TEXT,
    receipt         TEXT,
    retry_until     INTEGER NOT NULL,
    sent_at         INTEGER,
    sends           INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE UNIQUE INDEX coordinator_outbox_by_key ON coordinator_outbox (binding_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX coordinator_outbox_pending ON coordinator_outbox (state, created_at)
    WHERE state = 'pending';
";

const V18: &str = "
-- DL-140: a `shell.handover` is a durable shell effect too. SQLite cannot
-- change a CHECK constraint in place, so the table is rebuilt with `handover`
-- among the ops (`release` never makes an effect row). Every row, column and
-- the partial index stay as V16 defined them.
CREATE TABLE shell_effects_v18 (
    effect_id   TEXT    PRIMARY KEY,
    command_id  TEXT    NOT NULL UNIQUE,
    shell_id    TEXT    NOT NULL,
    op          TEXT    NOT NULL CHECK (op IN ('open', 'resize', 'close', 'handover')),
    change      TEXT    NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN ('queued', 'claimed', 'confirmed', 'failed',
                                                  'retired_not_dispatched', 'outcome_unknown')),
    detail      TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
INSERT INTO shell_effects_v18
    (effect_id, command_id, shell_id, op, change, state, detail, created_at, updated_at)
SELECT effect_id, command_id, shell_id, op, change, state, detail, created_at, updated_at
FROM shell_effects;
DROP TABLE shell_effects;
ALTER TABLE shell_effects_v18 RENAME TO shell_effects;
CREATE INDEX shell_effects_open ON shell_effects (state, created_at)
    WHERE state IN ('queued', 'claimed');
";

const V19: &str = "
-- DL-148: a setup command of a session start. The row is durable before the
-- command can run; it holds the process group so that a station restart can
-- find and stop a process that outlived the station.
CREATE TABLE setup_runs (
    effect_id  TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    pgid       INTEGER NOT NULL,
    started_at INTEGER NOT NULL
);
";

const V20: &str = "
-- DL-150: the worktree trash. One row per trash operation; the open rows
-- (every state but restored, purged and released) hold the folder and the
-- trash path against any other use. `main` and `admin_dir` are read from Git
-- in the first effect step.
CREATE TABLE worktree_ops (
    op_id         TEXT    PRIMARY KEY,
    session_id    TEXT    NOT NULL,
    main          TEXT    NOT NULL,
    admin_dir     TEXT    NOT NULL,
    original_path TEXT    NOT NULL,
    trash_path    TEXT    NOT NULL,
    dev           INTEGER NOT NULL,
    ino           INTEGER NOT NULL,
    branch        TEXT,
    state         TEXT    NOT NULL,
    phase         TEXT,
    message       TEXT,
    trashed_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX worktree_ops_open ON worktree_ops(session_id)
    WHERE state NOT IN ('restored', 'purged', 'released');
";

const V21: &str = "
-- R12, DL-165: artifacts. The bytes of a version live under
-- `<station data>/artifacts/<artifact_id>/<version>/`; these rows are the
-- index. `manifest_sha256` is the hash of the bundle's path and file hashes,
-- so a publish of identical content makes no new version. A suggestion row
-- keeps `artifact.suggested` to one per path and session.
CREATE TABLE artifacts (
    artifact_id    TEXT    PRIMARY KEY,
    session_id     TEXT,
    title          TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    latest_version INTEGER NOT NULL,
    author         TEXT    NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX artifacts_by_session ON artifacts (session_id, updated_at);
CREATE TABLE artifact_versions (
    artifact_id     TEXT    NOT NULL REFERENCES artifacts (artifact_id),
    version         INTEGER NOT NULL,
    size            INTEGER NOT NULL,
    sha256          TEXT    NOT NULL,
    manifest_sha256 TEXT    NOT NULL,
    files           TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, version)
);
CREATE TABLE artifact_suggestions (
    session_id TEXT    NOT NULL,
    path       TEXT    NOT NULL,
    at         INTEGER NOT NULL,
    PRIMARY KEY (session_id, path)
);
";

const V22: &str = "
-- DL-167 (R10): the cache and the daily token use of the model help. A cache
-- row is keyed by the SHA-256 of the kind, the model and the prompt, so the
-- same input never costs twice; `day` is the UTC day number (Unix days).
CREATE TABLE assist_cache (
    input_hash    TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL,
    output        TEXT    NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    created_at    INTEGER NOT NULL
) STRICT;
CREATE INDEX assist_cache_age ON assist_cache (created_at);
CREATE TABLE assist_usage (
    day    INTEGER PRIMARY KEY,
    tokens INTEGER NOT NULL
) STRICT;
";

const V23: &str = "
-- DL-182 (resume on the shim host). Forward only, like the other shim columns;
-- no row is rewritten. Every old row reads as `inspection_version = 0`, which
-- the queries treat as an unverified exit.
--   inspection_version  the inspection version of the shim's exit report
--   shim_pid            the process id of the shim itself (escaped-descendant check)
--   proof               NULL, 'ended_by_reboot' or 'reconciled_by_owner'; next to the
--                       row's state and reason, which stay as they are
--   proof_boot_id       the boot id that the reboot proof read
--   launch_boot_id      the boot id the station read before the spawn
--   native_target       the native id that a resume or an import continues
ALTER TABLE shims ADD COLUMN inspection_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shims ADD COLUMN shim_pid INTEGER;
ALTER TABLE shims ADD COLUMN proof TEXT
    CHECK (proof IS NULL OR proof IN ('ended_by_reboot', 'reconciled_by_owner'));
ALTER TABLE shims ADD COLUMN proof_boot_id TEXT;
ALTER TABLE shims ADD COLUMN launch_boot_id TEXT;
ALTER TABLE shims ADD COLUMN native_target TEXT;
CREATE INDEX shims_native_target ON shims(native_target) WHERE native_target IS NOT NULL;
";

const V24: &str = "
-- DL-189: the session history. One row per session whose transcript the
-- station captured (or tried to). Forward only; no row is rewritten.
--   capture        import (taken before the import resumed), lazy (taken on the
--                  first read of an older import) or failed
--   reason         why a capture failed
--   prefix_size    bytes of the provider transcript that the snapshot holds
--   prefix_sha256  SHA-256 of those bytes
--   snapshot       the path of the station's own copy
CREATE TABLE session_transcripts (
    session_id    TEXT PRIMARY KEY REFERENCES sessions(session_id),
    home          TEXT,
    path          TEXT,
    capture       TEXT NOT NULL CHECK (capture IN ('import', 'lazy', 'failed')),
    reason        TEXT,
    prefix_size   INTEGER,
    prefix_sha256 TEXT,
    snapshot      TEXT,
    item_count    INTEGER,
    captured_at   INTEGER NOT NULL
) STRICT;
";

/// The `station.db` migrations. V4 is the link epochs table of
/// `fleetdeck-link` (G1).
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(all())
}

/// The first `version` migrations only: the list of an older build.
#[cfg(test)]
pub(crate) fn migrations_upto(version: usize) -> Migrations<'static> {
    let mut list = all();
    list.truncate(version);
    Migrations::new(list)
}

fn all() -> Vec<M<'static>> {
    vec![
        M::up(V1),
        M::up(V2),
        M::up(V3),
        M::up(fleetdeck_link::LINK_EPOCHS_SQL),
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
        M::up(V15),
        M::up(V16),
        M::up(V17),
        M::up(V18),
        M::up(V19),
        M::up(V20),
        M::up(V21),
        M::up(V22),
        M::up(V23),
        M::up(V24),
        M::up(super::subagent_logs::V25),
        M::up(super::schema_v26::V26),
        M::up(super::schema_v27::V27),
        M::up(super::schema_v28::V28),
        M::up(super::schema_v29::V29),
        M::up(super::schema_v30::V30),
        M::up(super::schema_v31::V31),
        M::up(super::schema_v32::V32),
        M::up(super::schema_v33::V33),
        M::up(super::schema_v34::V34),
        M::up(super::schema_v35::V35),
        M::up(super::schema_v36::V36),
        M::up(super::schema_v37::V37),
        M::up(super::schema_v38::V38),
        M::up(super::schema_v39::V39),
        M::up(super::schema_v40::V40),
        M::up(super::schema_v41::V41),
        M::up(super::schema_v42::V42),
        M::up(super::schema_v43::V43),
        M::up(super::schema_v44::V44),
        M::up(super::schema_v45::V45),
        M::up(super::schema_v46::V46),
    ]
}
