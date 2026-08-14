// db.ts — SQLite store for fleetd (WAL mode). All timestamps are ms epoch
// integers. The SQLite handle comes from ./sqlite.ts, the one guarded seam that
// picks node:sqlite or bun:sqlite by runtime (the ExperimentalWarning suppression
// the Node driver needs now lives there); everything below is driver-agnostic.
//
// The store is versioned with PRAGMA user_version: openDb() runs the numbered
// migration ladder below, each migration wrapped in its own transaction. A fresh
// DB and an existing (unversioned) DB both report user_version 0, so migration #1
// must — and does — converge both to the same schema with ZERO data loss; a crash
// partway through any migration rolls the transaction (schema AND version bump)
// back, so a clean re-run resumes untouched. See the ladder comment for why.

import { chmodSync, statSync } from 'node:fs';
import { openDatabase, type SqliteHandle } from './sqlite.ts';
import { errCode, errText } from './errors.ts';

// The one row shape this module reads back: `PRAGMA table_info(<t>)` yields a row
// per column, and migrate() only ever touches the `name` cell to decide whether an
// additive ALTER still needs to run. Asserting just this field at each prepare()
// keeps the map callback on dot access (the full pragma row is cid/name/type/
// notnull/dflt_value/pk, but nothing here needs the rest).
interface PragmaColumnInfo {
  name: string;
}

// The slice of node:fs that openDb() needs to pin fleetd.db to 0600. A minimal
// structural shape rather than `typeof import('node:fs')` on purpose: the tests
// pass a stand-in whose statSync returns only `{ mode }`, and that stand-in must
// satisfy this type too (the real fs functions do, structurally).
interface DbFsImpl {
  chmodSync(path: string, mode: number): void;
  statSync(path: string): { mode: number };
}

// Connection-level pragmas. These configure the CONNECTION and must run at open
// time, OUTSIDE any transaction — never inside a migration: journal_mode = WAL
// cannot even be entered from within a transaction (SQLite would refuse it), and
// busy_timeout is a per-connection setting, not schema state. openDb() execs
// these before running the migration ladder.
const PRAGMAS = `
PRAGMA busy_timeout = 5000;
PRAGMA journal_mode = WAL;
`;

// The full current schema — the body of migration #1. Every statement is
// idempotent (CREATE ... IF NOT EXISTS): on a fresh DB it builds the whole store;
// on any pre-existing DB it is a no-op, leaving migrateAdditiveColumns() below to
// ALTER-in whatever late columns an older on-disk shape is missing.
const SCHEMA = `
CREATE TABLE IF NOT EXISTS sessions (
  session_id        TEXT PRIMARY KEY,
  callsign          TEXT,
  model             TEXT,
  cwd               TEXT,
  repo_id           TEXT,
  repo_name         TEXT,
  branch            TEXT,
  worktree          TEXT,
  col               TEXT DEFAULT 'queued',
  note              TEXT,
  task              TEXT,
  last_tool         TEXT,
  events            INTEGER DEFAULT 0,
  started_at        INTEGER,
  last_seen         INTEGER,
  ended_at          INTEGER,
  blocked_this_turn INTEGER DEFAULT 0,
  source            TEXT DEFAULT 'hooks',
  notification_type TEXT,
  archived_at       INTEGER,
  ticket            TEXT,               -- current Jira key (raven-PROJ-123's PROJ-123) or NULL
  ticket_source     TEXT,               -- 'branch' | 'manual'; NULL = never set (auto path still open)
  prev_callsign     TEXT,               -- birth callsign, write-once on the FIRST rename (stale-ref anchor for mail);
                                        -- the anchor never moves, even when a rename gives the slot a new owner
                                        -- (a ticket-clear revert writes the lineage's birth name, not the dropped one)
  -- 0.7.0 Move-to-tmux (adopt): three additive columns, all NULL for pre-0.7.0
  -- rows (never armed, never proven-ended). adopt_armed_until stores the arm
  -- DEADLINE (ms epoch) so a consumer just checks it against now() in JS --
  -- restart-durable, snapshot-visible, disarm = NULL, expiry needs no sweep.
  adopt_armed_until INTEGER,            -- Move-to-tmux arm deadline (ms epoch); NULL = not armed
  adopt_armed_skip  INTEGER,            -- bypass choice stored at arm time (0/1); read by the auto-adopt trigger, cleared with the arm
  end_reason        TEXT,               -- how the session ended: hook reason ('end'/'logout'/…), 'presumed' (a guess), or 'superseded' (a /clear fork); NULL = never ended, or ended before provenance existed
  -- 0.7.1 /clear succession. The CLI mints a NEW session id on /clear: the old
  -- id fires SessionEnd(reason='clear') and a brand-new id fires
  -- SessionStart(source='clear') in the same cwd, same second. Nothing in the
  -- payload links them, so cleared_at opens a short correlation window and
  -- succeeded_by records the verdict (auditable + makes the boot heal idempotent).
  cleared_at        INTEGER,            -- when this session last ran /clear; opens the succession window
  succeeded_by      TEXT,               -- the session id that inherited this card's identity (pane, callsign, mail)
  -- 0.7.1 custom names: the human-chosen suffix of <animal>-<suffix>. Presence
  -- means "a human named this card", which is what blocks branch auto-detection
  -- from renaming over it.
  custom_suffix     TEXT,
  -- Run generation (BUG-025): SessionEnd is an ASYNC hook while SessionStart is
  -- synchronous, so a claude --resume (a NEW process reusing the SAME session
  -- id) can register before the previous process's SessionEnd lands — and the
  -- late end would then tombstone the live resumed card. The hook shims mint one
  -- fleet_run nonce per PROCESS and attach it to every event they send; the
  -- active run is persisted here at SessionStart and a SessionEnd applies only
  -- when its nonce matches. NULL on rows whose hooks predate the shims.
  run_id            TEXT
);
CREATE TABLE IF NOT EXISTS file_touches (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  repo_id    TEXT,
  rel_path   TEXT,
  abs_path   TEXT,
  session_id TEXT,
  worktree   TEXT,
  at         INTEGER
);
CREATE INDEX IF NOT EXISTS idx_touches_key ON file_touches(repo_id, rel_path, at);
CREATE INDEX IF NOT EXISTS idx_touches_sid ON file_touches(session_id);
CREATE TABLE IF NOT EXISTS mail (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  to_session   TEXT,
  from_id      TEXT,
  text         TEXT,
  at           INTEGER,
  delivered_at INTEGER,
  expired_at   INTEGER,
  -- BUG-034: an in-flight delivery LEASE. Set (with delivered_at still NULL)
  -- when a claim path hands the text to a consumer whose acknowledgement has
  -- not yet landed (/api/watch response, owned-pane paste, board /mail GET);
  -- finalized (delivered_at set) only on explicit ack or a completed side
  -- effect. Stamped as the lease DEADLINE — a daemon that exits mid-flight
  -- leaves rows whose deadline simply passes, so a restarted daemon claims
  -- them again.
  claimed_at   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_mail_to ON mail(to_session, delivered_at);
CREATE TABLE IF NOT EXISTS events (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT,
  hook_event TEXT,
  tool_name  TEXT,
  note       TEXT,
  at         INTEGER
);
CREATE INDEX IF NOT EXISTS idx_events_at ON events(at);
CREATE INDEX IF NOT EXISTS idx_events_sid_at ON events(session_id, at);
CREATE TABLE IF NOT EXISTS ticker (
  id  INTEGER PRIMARY KEY AUTOINCREMENT,
  at  INTEGER,
  msg TEXT
);
CREATE TABLE IF NOT EXISTS conflicts (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  at            INTEGER,
  repo_id       TEXT,
  rel_path      TEXT,
  severity      TEXT,
  sessions_json TEXT
);
CREATE TABLE IF NOT EXISTS commands (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  at          INTEGER,
  text        TEXT,
  parsed_json TEXT
);
CREATE TABLE IF NOT EXISTS questions (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id   TEXT,
  kind         TEXT,               -- 'permission' | 'elicitation' | 'choice' | 'freeform'
  payload_json TEXT,               -- raw hook payload (hold kinds) or {text} (freeform)
  status       TEXT DEFAULT 'pending',  -- pending | answered | expired
  answer_json  TEXT,
  created_at   INTEGER,
  expires_at   INTEGER,            -- hold deadline; NULL for freeform (no hold)
  answered_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_questions_pending ON questions(status, session_id);
CREATE TABLE IF NOT EXISTS spawns (
  spawn_id      TEXT PRIMARY KEY,   -- v1.2 board-spawned sessions (CONTRACT)
  session_id    TEXT,               -- pre-issued UUID handed to claude --session-id
  callsign      TEXT,
  tmux_session  TEXT,               -- fleetdeck-<port>
  tmux_window   TEXT,               -- fd<port>-<callsign> (scoped, kill-verified)
  cwd           TEXT,               -- requested cwd (the form value)
  worktree_path TEXT,               -- effective cwd when worktree:true, else NULL
  worktree_owned INTEGER,           -- 1: this spawn CREATED the worktree (boot cleanup may remove it);
                                    -- 0: the worktree pre-existed and was only reused; NULL: unknown (pre-fix row)
  requested_at  INTEGER,
  status        TEXT DEFAULT 'spawning',  -- spawning | stalled | live | pane-dead | killed | gone
  skip_permissions INTEGER DEFAULT 0,    -- v1.3 unsupervised spawn (either bypass form)
  remote_control INTEGER DEFAULT 0,      -- remote-control wished/enabled for this launch
  remote_url     TEXT,                   -- harvested claude.ai URL; NULL until/if observed
  origin_url     TEXT,                   -- repo-mode clone source; NULL for cwd-mode spawns
  requested_branch TEXT,                 -- repo-mode branch requested by the board
  branch_mode    TEXT,                   -- repo-mode worktree | in-place
  gateway        INTEGER DEFAULT 0,      -- routed through the LLM gateway (settings.gateway_*)
  kind           TEXT DEFAULT 'claude',  -- claude | shell
  setup_cmd      TEXT,                   -- visible pre-Claude POSIX sh setup
  stall_detail   TEXT,                   -- bounded/redacted pane excerpt captured when registration stalls
  -- Sibling of stall_detail, same budget and posture: the bounded/redacted git
  -- stderr excerpt kept when a repo-mode spawn's clone or fetch fails, so the
  -- REMEDY git printed above its fatal verdict survives to the card instead of
  -- only reaching fleetd.log. NULL whenever nothing failed — and on every
  -- pre-existing row, which is the truthful backfill (no detail was ever
  -- captured). NOTE: the status enum comment above is already stale
  -- ('provisioning' is missing from it, and is exactly the status a row holds
  -- while its clone runs) — do not rely on it.
  fail_detail    TEXT
);
CREATE INDEX IF NOT EXISTS idx_spawns_session ON spawns(session_id);
CREATE INDEX IF NOT EXISTS idx_spawns_status ON spawns(status);
CREATE TABLE IF NOT EXISTS repos (
  repo_id        TEXT PRIMARY KEY,
  repo_name      TEXT,
  root           TEXT,
  origin_url     TEXT,
  default_branch TEXT,
  first_seen_at  INTEGER,
  last_used_at   INTEGER,
  source         TEXT
);
CREATE INDEX IF NOT EXISTS idx_repos_name ON repos(repo_name);
CREATE TABLE IF NOT EXISTS settings (
  key        TEXT PRIMARY KEY,
  value      TEXT,
  updated_at INTEGER
);
CREATE TABLE IF NOT EXISTS plans (
  plan_id      INTEGER PRIMARY KEY AUTOINCREMENT,  -- v1.3 plan library (CONTRACT)
  session_id   TEXT,
  callsign     TEXT,
  repo_id      TEXT,
  repo_name    TEXT,
  question_id  INTEGER,             -- the held ExitPlanMode permission question
  plan_md      TEXT,                -- tool_input.plan, raw markdown (board derives titles)
  created_at   INTEGER,
  status       TEXT DEFAULT 'proposed', -- proposed | approved | captured | rejected | handled-in-terminal | executed | archived
                                        -- handled-in-terminal: the ExitPlanMode question was retired
                                        -- unanswered AND the session then showed activity — the human
                                        -- decided in the terminal (UX 2.2; derive.mjs planRetired).
  executed_via TEXT                 -- optional {via} recorded at mark {status:"executed"}
);
CREATE INDEX IF NOT EXISTS idx_plans_status ON plans(status);
CREATE INDEX IF NOT EXISTS idx_plans_question ON plans(question_id);
-- Alias table (BUG-107): every callsign a card has ever worn, in the order it
-- wore it. prev_callsign is ONE slot — a ticket-clear revert used to overwrite
-- the birth-name anchor with the dropped ticketed name, and the next rename
-- then permanently forgot the SessionStart callsign. Every rename INSERT OR
-- IGNOREs the outgoing name here (idempotent), and mail/assign/command target
-- resolution falls back to this set after current names and the anchor, so a
-- supported ticket/name/clear sequence can never orphan a name a peer or an
-- automation is still using.
CREATE TABLE IF NOT EXISTS session_aliases (
  session_id TEXT,
  callsign   TEXT,
  at         INTEGER,                   -- when this card stopped wearing the name
  PRIMARY KEY (session_id, callsign)
);
CREATE INDEX IF NOT EXISTS idx_aliases_callsign ON session_aliases(callsign);
`;

// Additive schema migration: DBs created before the agents-cli ingest
// feature (handoff F1, `claude agents --json` as a secondary session source)
// predate the `source` column. `CREATE TABLE IF NOT EXISTS` above is a no-op
// against an already-existing sessions table, so backfill the column here.
// Default 'hooks' matches every pre-existing row's true provenance.
// Same story for `notification_type` (Phase 3, F3e: docs §8 values like
// permission_prompt / idle_prompt / elicitation_dialog / agent_needs_input,
// stored on the card so the board can say WHY a session needs you).
// v1.2 needs no ALTERs of its own: the `spawns` table is new wholesale, so
// `CREATE TABLE IF NOT EXISTS` in the DDL above IS the additive migration for
// pre-v1.2 databases, and the new sessions `source` value 'spawned' rides the
// existing TEXT column.
// v1.3: `plans` is new wholesale (CREATE TABLE IF NOT EXISTS covers it), but
// `spawns` shipped in v1.2 — pre-v1.3 databases need the additive
// `skip_permissions` column backfilled here. Default 0 matches every
// pre-existing row's truth (the flag did not exist to be requested).
// Remote control is additive on that same durable row: old launches were not
// born remote and no URL was persisted, so 0/NULL are truthful backfills.
// Retention is additive too: archived/expired timestamps preserve all rows
// for forensics while removing them from live board/delivery queries.
//
// This is the second half of migration #1 (called by its up() right after
// SCHEMA). It is idempotent by construction — every ALTER is guarded by a
// table_info probe and the alias backfill is INSERT OR IGNORE — which is exactly
// why re-running migration #1 over a populated v0 DB loses no data.
//
// CAUTION: this helper is frozen along with migration #1. A NEW forward column
// belongs in a v2 migration appended to the ladder, NOT as another guarded ALTER
// here — a DB already stamped user_version=1 never re-runs #1, so a column added
// here would silently never reach it.
function migrateAdditiveColumns(db: SqliteHandle): void {
  const cols = db
    .prepare<PragmaColumnInfo>('PRAGMA table_info(sessions)')
    .all()
    .map((r) => r.name);
  if (!cols.includes('source')) {
    db.exec("ALTER TABLE sessions ADD COLUMN source TEXT DEFAULT 'hooks'");
  }
  if (!cols.includes('notification_type')) {
    db.exec('ALTER TABLE sessions ADD COLUMN notification_type TEXT');
  }
  if (!cols.includes('archived_at')) {
    db.exec('ALTER TABLE sessions ADD COLUMN archived_at INTEGER');
  }
  // 0.6.0 ticket callsigns: three additive columns. NULL backfill is truthful
  // for every pre-0.6.0 row — those sessions were never ticket-named and never
  // renamed, so ticket / ticket_source / prev_callsign are all genuinely unset.
  if (!cols.includes('ticket')) {
    db.exec('ALTER TABLE sessions ADD COLUMN ticket TEXT');
  }
  if (!cols.includes('ticket_source')) {
    db.exec('ALTER TABLE sessions ADD COLUMN ticket_source TEXT');
  }
  if (!cols.includes('prev_callsign')) {
    db.exec('ALTER TABLE sessions ADD COLUMN prev_callsign TEXT');
  }
  // 0.7.0 Move-to-tmux (adopt): three additive columns. NULL backfill is
  // truthful for every pre-0.7.0 row — those sessions were never armed for a
  // move-to-tmux, and their end (if any) predates the end_reason bookkeeping,
  // so they carry NO proof of how they ended. adopt-now reads end_reason as an
  // ALLOWLIST (a NULL is "unproven", never "proven"), which is what keeps a
  // pre-0.7.0 offline row — possibly a CLI that is still quietly alive — from
  // being resumed into a duplicate billed session.
  if (!cols.includes('adopt_armed_until')) {
    db.exec('ALTER TABLE sessions ADD COLUMN adopt_armed_until INTEGER');
  }
  if (!cols.includes('adopt_armed_skip')) {
    db.exec('ALTER TABLE sessions ADD COLUMN adopt_armed_skip INTEGER');
  }
  if (!cols.includes('end_reason')) {
    db.exec('ALTER TABLE sessions ADD COLUMN end_reason TEXT');
  }
  // 0.7.1 /clear succession + custom names: three additive columns. NULL
  // backfill is truthful — pre-0.7.1 rows were never correlated across a /clear
  // (that is the bug this ships) and were never human-named. The boot heal
  // (reconcileClearForks) repairs already-stranded pairs from the events table,
  // not from these columns, precisely because old rows have them NULL.
  if (!cols.includes('cleared_at')) {
    db.exec('ALTER TABLE sessions ADD COLUMN cleared_at INTEGER');
  }
  if (!cols.includes('succeeded_by')) {
    db.exec('ALTER TABLE sessions ADD COLUMN succeeded_by TEXT');
  }
  if (!cols.includes('custom_suffix')) {
    db.exec('ALTER TABLE sessions ADD COLUMN custom_suffix TEXT');
  }
  // Run generation (BUG-025). NULL backfill is truthful — pre-existing rows
  // registered before the hook shims minted fleet_run nonces, and a NULL here
  // makes any tagged SessionEnd conservatively skip the tombstone (the dead
  // card then converges via retention instead of killing a resumed process).
  if (!cols.includes('run_id')) {
    db.exec('ALTER TABLE sessions ADD COLUMN run_id TEXT');
  }
  const mailCols = db
    .prepare<PragmaColumnInfo>('PRAGMA table_info(mail)')
    .all()
    .map((r) => r.name);
  if (!mailCols.includes('expired_at')) {
    db.exec('ALTER TABLE mail ADD COLUMN expired_at INTEGER');
  }
  // BUG-034 lease column. NULL backfill is truthful for every pre-existing
  // row: nothing was ever claimed under a lease before this shipped.
  if (!mailCols.includes('claimed_at')) {
    db.exec('ALTER TABLE mail ADD COLUMN claimed_at INTEGER');
  }
  const spawnCols = db
    .prepare<PragmaColumnInfo>('PRAGMA table_info(spawns)')
    .all()
    .map((r) => r.name);
  if (spawnCols.length && !spawnCols.includes('skip_permissions')) {
    db.exec('ALTER TABLE spawns ADD COLUMN skip_permissions INTEGER DEFAULT 0');
  }
  if (spawnCols.length && !spawnCols.includes('remote_control')) {
    db.exec('ALTER TABLE spawns ADD COLUMN remote_control INTEGER DEFAULT 0');
  }
  if (spawnCols.length && !spawnCols.includes('remote_url')) {
    db.exec('ALTER TABLE spawns ADD COLUMN remote_url TEXT');
  }
  if (spawnCols.length && !spawnCols.includes('origin_url')) {
    db.exec('ALTER TABLE spawns ADD COLUMN origin_url TEXT');
  }
  if (spawnCols.length && !spawnCols.includes('requested_branch')) {
    db.exec('ALTER TABLE spawns ADD COLUMN requested_branch TEXT');
  }
  if (spawnCols.length && !spawnCols.includes('gateway')) {
    db.exec('ALTER TABLE spawns ADD COLUMN gateway INTEGER DEFAULT 0');
  }
  if (spawnCols.length && !spawnCols.includes('branch_mode')) {
    db.exec('ALTER TABLE spawns ADD COLUMN branch_mode TEXT');
  }
  if (spawnCols.length && !spawnCols.includes('kind')) {
    db.exec("ALTER TABLE spawns ADD COLUMN kind TEXT DEFAULT 'claude'");
  }
  if (spawnCols.length && !spawnCols.includes('setup_cmd')) {
    db.exec('ALTER TABLE spawns ADD COLUMN setup_cmd TEXT');
  }
  if (spawnCols.length && !spawnCols.includes('stall_detail')) {
    db.exec('ALTER TABLE spawns ADD COLUMN stall_detail TEXT');
  }
  // The other half of the DDL above: an existing DB never re-runs CREATE TABLE,
  // so a new column has to arrive here too. NULL is the truthful backfill — no
  // failure detail was ever captured before this shipped. The `spawnCols.length`
  // guard is now belt-and-suspenders: migration #1 runs SCHEMA before this helper,
  // so `spawns` always exists by the time we probe — but the guard keeps the
  // helper self-safe if it is ever driven without SCHEMA (PRAGMA table_info on a
  // missing table returns [], and the bare ALTER would throw).
  if (spawnCols.length && !spawnCols.includes('fail_detail')) {
    db.exec('ALTER TABLE spawns ADD COLUMN fail_detail TEXT');
  }
  // BUG-153: the ownership bit behind boot reconciliation's worktree removal.
  // NULL is the truthful backfill — pre-fix rows never recorded whether their
  // worktree was created or reused, so boot cleanup must leave those trees
  // alone (exactly the pre-fix behaviour) rather than guess.
  if (spawnCols.length && !spawnCols.includes('worktree_owned')) {
    db.exec('ALTER TABLE spawns ADD COLUMN worktree_owned INTEGER');
  }
  // BUG-107 alias-table backfill for pre-existing rows: the current callsign
  // and the write-once prev_callsign anchor are the two names a row provably
  // still answers to. INSERT OR IGNORE makes re-runs free.
  db.exec(`INSERT OR IGNORE INTO session_aliases (session_id, callsign, at)
    SELECT session_id, callsign, NULL FROM sessions WHERE callsign IS NOT NULL`);
  db.exec(`INSERT OR IGNORE INTO session_aliases (session_id, callsign, at)
    SELECT session_id, prev_callsign, NULL FROM sessions WHERE prev_callsign IS NOT NULL`);
}

// ---------------------------------------------------------------------------
// Versioned migration ladder (PRAGMA user_version)
//
// Each migration runs inside its OWN transaction and stamps user_version LAST:
//   BEGIN; up(db); PRAGMA user_version = <version>; COMMIT   (ROLLBACK on throw)
// In SQLite both DDL (CREATE/ALTER) and `PRAGMA user_version = N` are
// transactional, so a crash partway through up() rolls back the partial schema
// AND leaves user_version untouched — a clean re-run re-attempts that same
// migration from an unchanged starting point (invariant: crash → rollback →
// clean re-run). The connection pragmas (busy_timeout, journal_mode = WAL) are
// deliberately NOT migrations — WAL cannot be entered inside a transaction — so
// openDb() execs PRAGMAS separately, at open time.
//
// THE v0 TRAP: a fresh empty DB and a populated DB written by the OLD unversioned
// code BOTH report user_version 0 — indistinguishable by version. So migration #1
// must be safe on both, and it is: its up() is exactly the (SCHEMA then additive
// backfill) that openDb has always run. On a fresh DB the CREATEs build the store;
// on an existing v0 DB every CREATE ... IF NOT EXISTS is a no-op, each ALTER is
// guarded by a table_info probe, and the alias backfill is INSERT OR IGNORE — so
// nothing is dropped or rewritten (zero data loss). Only after up() succeeds is
// user_version advanced to 1, so subsequent opens skip #1 entirely.
export interface Migration {
  version: number;
  up(db: SqliteHandle): void;
}

// The ordered ladder. Version 1 is the entire current schema; keep it a single
// logical migration until a real FORWARD schema change lands, then append
// { version: 2, up(db) { ... } } — never edit #1 (DBs already past it will not
// re-run it). Authored ascending; migrate() re-sorts defensively regardless.
export const MIGRATIONS: Migration[] = [
  {
    version: 1,
    up(db) {
      db.exec(SCHEMA);
      migrateAdditiveColumns(db);
    },
  },
];

// The user_version a fully-migrated DB reports. Derived from the ladder so
// callers and tests never hard-code the latest number.
export const LATEST_USER_VERSION = MIGRATIONS.reduce((max, m) => Math.max(max, m.version), 0);

// Read PRAGMA user_version as a plain number; a brand-new or old-unversioned DB
// reports 0. (user_version is a 32-bit int, but coerce in case a driver ever
// hands it back as a bigint.)
export function readUserVersion(db: SqliteHandle): number {
  const row = db.prepare<{ user_version: number | bigint }>('PRAGMA user_version').get();
  return row ? Number(row.user_version) : 0;
}

// Bring `db` up to the latest schema version: run every migration whose version
// exceeds the current user_version, in ascending order, each in its own
// transaction (see the ladder comment for the crash-rollback and v0-convergence
// guarantees). `migrations` is injectable ONLY so tests can drive the transaction
// machinery with synthetic migrations; production always uses the default ladder.
export function migrate(db: SqliteHandle, migrations: Migration[] = MIGRATIONS): void {
  let current = readUserVersion(db);
  const ordered = [...migrations].sort((a, b) => a.version - b.version);
  const seen = new Set<number>();
  for (const m of ordered) {
    // Versions are code-controlled and interpolated into the PRAGMA below (which
    // takes no bound parameter), so reject anything that is not a positive int
    // within SQLite's signed-32-bit user_version range (a larger value would
    // silently truncate on write).
    if (!Number.isInteger(m.version) || m.version < 1 || m.version > 0x7fffffff) {
      throw new Error(`invalid migration version: ${String(m.version)}`);
    }
    // A duplicate version silently no-ops the later migration (once current has
    // passed it, its up() never runs) — a ladder-authoring bug, so fail loud.
    if (seen.has(m.version)) {
      throw new Error(`duplicate migration version: ${String(m.version)}`);
    }
    seen.add(m.version);
    if (m.version <= current) continue;
    db.exec('BEGIN');
    try {
      m.up(db);
      db.exec(`PRAGMA user_version = ${m.version}`);
      db.exec('COMMIT');
    } catch (err) {
      // ROLLBACK undoes the partial up() AND the (uncommitted) version bump. An
      // engine-level error can auto-rollback the transaction first, after which an
      // explicit ROLLBACK throws "no transaction is active" (identical message on
      // bun:sqlite and node:sqlite) — that is the rolled-back state we want, so
      // swallow ONLY that. Any other ROLLBACK failure means the transaction may
      // still be open; surface it (with the up() error as cause) rather than hide it.
      try {
        db.exec('ROLLBACK');
      } catch (rollbackErr) {
        const msg = rollbackErr instanceof Error ? rollbackErr.message : String(rollbackErr);
        if (!/no transaction is active/i.test(msg)) {
          throw new Error(`migration ${m.version}: up() failed and ROLLBACK failed: ${msg}`, {
            cause: err,
          });
        }
      }
      throw err;
    }
    current = m.version;
  }
}

export function openDb(file: string, fsImpl: DbFsImpl = { chmodSync, statSync }): SqliteHandle {
  const db = openDatabase(file);
  db.exec(PRAGMAS);
  migrate(db);
  // STATE CONFIDENTIALITY CONTRACT: this DB holds session cwds, callsigns, mail,
  // commands, plan text and raw permission/question payloads — owner-only, like
  // the token and fleetd.log the July audit already hardened to 0600. Neither
  // SQLite driver exposes a creation mode, so SQLite makes fleetd.db (and, on first write, its
  // -wal/-shm sidecars) under the ambient umask — 0644, i.e. world-readable,
  // under the common 022. chmod after open pins the main file to 0600; the WAL/SHM
  // chmods are best-effort because those files are created lazily on first write
  // (and recreated after a checkpoint), so they may be absent right now. The
  // durable guarantee that closes any window where a freshly recreated sidecar is
  // momentarily 0644 is the 0700 FLEETDECK_HOME dir (see fleetd.mjs) — a private
  // state dir keeps other local users out regardless of individual file modes.
  // Only an on-disk DB has files to chmod. Guard the ':memory:' sentinel
  // explicitly: it is NOT a path, and chmod(':memory:') would silently alter an
  // unrelated ./:memory: file if one happened to exist in cwd. Each chmod is also
  // wrapped because the WAL/SHM sidecars are created lazily on first write (and
  // recreated after a checkpoint), so they may be absent right now — ENOENT there
  // must not become a spurious throw. The MAIN file is different: SQLite has
  // already opened it by now, so it exists, and an unverifiable owner-only mode
  // there means the declared confidentiality boundary silently degraded (shared
  // HOME not owned by the daemon UID, permissive pre-created DB, a filesystem
  // that refuses chmod) while other local users keep read access. Refuse rather
  // than serve state the contract says is private. (`fsImpl` exists so tests
  // can simulate a chmod refusal — a real EPERM needs a foreign-owned file,
  // which an unprivileged test cannot construct.)
  if (file !== ':memory:') {
    try {
      fsImpl.chmodSync(file, 0o600);
      const mode = fsImpl.statSync(file).mode & 0o777;
      if (mode & 0o077) {
        throw Object.assign(new Error(`mode still ${mode.toString(8)} after chmod 0600`), {
          code: 'EMODE',
        });
      }
    } catch (err) {
      db.close();
      throw new Error(
        `fleetd.db owner-only confidentiality could not be established (${errText(err, 'unknown error')}); refusing to start with the state database readable by other users`,
        { cause: err },
      );
    }
    for (const sidecar of [`${file}-wal`, `${file}-shm`]) {
      // Lazily absent sidecars are expected — ENOENT only. Any OTHER failure
      // (EPERM on a shared HOME, a mode the stat proves is still permissive)
      // breaks the same contract on a file that already exists, so refuse too.
      try {
        fsImpl.chmodSync(sidecar, 0o600);
        if (fsImpl.statSync(sidecar).mode & 0o077) {
          throw Object.assign(new Error('mode still permissive after chmod 0600'), {
            code: 'EMODE',
          });
        }
      } catch (err) {
        if (errCode(err) === 'ENOENT') continue;
        db.close();
        throw new Error(
          `fleetd.db sidecar owner-only confidentiality could not be established (${errText(err, 'unknown error')}); refusing to start with the state database readable by other users`,
          { cause: err },
        );
      }
    }
  }
  return db;
}
