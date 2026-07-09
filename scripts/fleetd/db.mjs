// db.mjs — SQLite store for fleetd (node:sqlite DatabaseSync, WAL mode).
// All timestamps are ms epoch integers.

// Suppress ONLY the node:sqlite ExperimentalWarning, even when the process was
// started without --no-warnings=ExperimentalWarning (e.g. `node fleetd.mjs`
// by hand). Other warnings are re-printed in the default format.
process.removeAllListeners('warning');
process.on('warning', (w) => {
  if (w.name === 'ExperimentalWarning' && /sqlite/i.test(String(w.message))) return;
  console.error(`(node:${process.pid}) ${w.name}: ${w.message}`);
});

const { DatabaseSync } = await import('node:sqlite');

const DDL = `
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
  notification_type TEXT
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
  delivered_at INTEGER
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
  status        TEXT DEFAULT 'spawning',  -- spawning | live | pane-dead | killed | gone
  skip_permissions INTEGER DEFAULT 0     -- v1.3 unsupervised spawn (either bypass form)
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
function migrate(db) {
  const cols = db.prepare('PRAGMA table_info(sessions)').all().map(r => r.name);
  if (!cols.includes('source')) {
    db.exec("ALTER TABLE sessions ADD COLUMN source TEXT DEFAULT 'hooks'");
  }
  if (!cols.includes('notification_type')) {
    db.exec('ALTER TABLE sessions ADD COLUMN notification_type TEXT');
  }
  const spawnCols = db.prepare('PRAGMA table_info(spawns)').all().map(r => r.name);
  if (spawnCols.length && !spawnCols.includes('skip_permissions')) {
    db.exec('ALTER TABLE spawns ADD COLUMN skip_permissions INTEGER DEFAULT 0');
  }
}

export function openDb(file) {
  const db = new DatabaseSync(file);
  db.exec(DDL);
  migrate(db);
  return db;
}
