// db.mjs — SQLite store for fleetd (node:sqlite DatabaseSync, WAL mode).
// All timestamps are ms epoch integers.

// Suppress ONLY the warning raised while node:sqlite itself is imported. WHY:
// removing `warning` listeners here clobbers handlers installed by launchers,
// test runners and observability tooling, while installing our own formatter
// also loses Node's normal warning detail. Intercepting the one emission at its
// source leaves every pre-existing listener and every unrelated warning alone.
const emitWarning = process.emitWarning;
process.emitWarning = function fleetdSqliteWarningFilter(warning, type, ...args) {
  const name = warning instanceof Error
    ? warning.name
    : (typeof type === 'string' ? type : type?.type);
  const message = warning instanceof Error ? warning.message : String(warning);
  if (name === 'ExperimentalWarning' && /^SQLite is an experimental feature\b/i.test(message)) return;
  return emitWarning.call(this, warning, type, ...args);
};

let DatabaseSync;
try {
  ({ DatabaseSync } = await import('node:sqlite'));
} finally {
  process.emitWarning = emitWarning;
}

const DDL = `
PRAGMA busy_timeout = 5000;
PRAGMA journal_mode = WAL;
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
  prev_callsign     TEXT                -- birth callsign, write-once on the FIRST rename (stale-ref anchor for mail)
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
  expired_at   INTEGER
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
  requested_at  INTEGER,
  status        TEXT DEFAULT 'spawning',  -- spawning | stalled | live | pane-dead | killed | gone
  skip_permissions INTEGER DEFAULT 0,    -- v1.3 unsupervised spawn (either bypass form)
  remote_control INTEGER DEFAULT 0,      -- remote-control wished/enabled for this launch
  remote_url     TEXT                    -- harvested claude.ai URL; NULL until/if observed
);
CREATE INDEX IF NOT EXISTS idx_spawns_session ON spawns(session_id);
CREATE INDEX IF NOT EXISTS idx_spawns_status ON spawns(status);
CREATE TABLE IF NOT EXISTS plans (
  plan_id      INTEGER PRIMARY KEY AUTOINCREMENT,  -- v1.3 plan library (CONTRACT)
  session_id   TEXT,
  callsign     TEXT,
  repo_id      TEXT,
  repo_name    TEXT,
  question_id  INTEGER,             -- the held ExitPlanMode permission question
  plan_md      TEXT,                -- tool_input.plan, raw markdown (board derives titles)
  created_at   INTEGER,
  status       TEXT DEFAULT 'proposed', -- proposed | approved | captured | rejected | executed | archived
  executed_via TEXT                 -- optional {via} recorded at mark {status:"executed"}
);
CREATE INDEX IF NOT EXISTS idx_plans_status ON plans(status);
CREATE INDEX IF NOT EXISTS idx_plans_question ON plans(question_id);
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
function migrate(db) {
  const cols = db.prepare('PRAGMA table_info(sessions)').all().map(r => r.name);
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
  const mailCols = db.prepare('PRAGMA table_info(mail)').all().map(r => r.name);
  if (!mailCols.includes('expired_at')) {
    db.exec('ALTER TABLE mail ADD COLUMN expired_at INTEGER');
  }
  const spawnCols = db.prepare('PRAGMA table_info(spawns)').all().map(r => r.name);
  if (spawnCols.length && !spawnCols.includes('skip_permissions')) {
    db.exec('ALTER TABLE spawns ADD COLUMN skip_permissions INTEGER DEFAULT 0');
  }
  if (spawnCols.length && !spawnCols.includes('remote_control')) {
    db.exec('ALTER TABLE spawns ADD COLUMN remote_control INTEGER DEFAULT 0');
  }
  if (spawnCols.length && !spawnCols.includes('remote_url')) {
    db.exec('ALTER TABLE spawns ADD COLUMN remote_url TEXT');
  }
}

export function openDb(file) {
  const db = new DatabaseSync(file);
  db.exec(DDL);
  migrate(db);
  return db;
}
