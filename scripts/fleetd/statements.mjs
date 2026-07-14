// statements.mjs — the prepared-statement map (`q`) plus the cached-UPDATE
// session writer. Everything here is bound to one `db` handle; createStatements
// compiles every statement once at core boot and hands back the map + the
// updateSession writer the rest of the core threads through `ctx`.

export function createStatements(db) {
  // ------------------------------------------------------------- statements
  const q = {
    getSession: db.prepare('SELECT * FROM sessions WHERE session_id = ?'),
    // 0.6.0 ticket callsigns: is a candidate name already held by ANOTHER row?
    // Scope is archived_at IS NULL (not ended_at IS NULL) because resolveTargets
    // routes mail against every non-archived row including dead-but-retained
    // tombstones — a name held by a corpse still up to 24h must not be reissued,
    // or mail would fork between the corpse and its usurper. Both callsign and
    // prev_callsign count as "held" (a birth name kept as a stale-ref anchor is
    // still a live mail target). The session's own row is excluded so a rename
    // that keeps the animal never collides with itself.
    callsignTaken: db.prepare('SELECT 1 FROM sessions WHERE (callsign = ? OR prev_callsign = ?) AND archived_at IS NULL AND session_id != ? LIMIT 1'),
    allSessions: db.prepare('SELECT * FROM sessions ORDER BY started_at'),
    visibleSessions: db.prepare('SELECT * FROM sessions WHERE archived_at IS NULL ORDER BY started_at'),
    countSessions: db.prepare('SELECT COUNT(*) AS n FROM sessions'),
    insertSession: db.prepare(`INSERT INTO sessions
      (session_id, callsign, col, note, events, started_at, last_seen, blocked_this_turn)
      VALUES (?, ?, 'queued', 'registered', 0, ?, ?, 0)`),
    // agents-cli ingest (F1): a card created from `claude agents --json`
    // output rather than hook telemetry. source='agents-cli' from birth so
    // the precedence rule (hooks always win) has something to check against.
    insertAgentSession: db.prepare(`INSERT INTO sessions
      (session_id, callsign, cwd, repo_id, repo_name, branch, worktree, col, note, task, events, started_at, last_seen, blocked_this_turn, source)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 0, 'agents-cli')`),
    setBlocked: db.prepare('UPDATE sessions SET blocked_this_turn = ? WHERE session_id = ?'),
    // 0.7.1 /clear succession. The CLI forks a new session id on /clear, and
    // nothing in the SessionStart payload names the predecessor — so the link is
    // inferred: same cwd, a /clear stamped moments ago, not already succeeded,
    // still on the board. Ordered so the caller can apply the tie-break rule
    // (prefer the candidate holding a pane; else the most recent clear).
    clearedPredecessors: db.prepare(`SELECT * FROM sessions
      WHERE cwd = ? AND cleared_at IS NOT NULL AND cleared_at > ?
        AND succeeded_by IS NULL AND ended_at IS NULL AND archived_at IS NULL
        AND session_id != ?
      ORDER BY cleared_at DESC`),
    // The pane follows the session across a /clear. Only NON-terminal rows move:
    // the live pane is the successor's to drive, while terminal history stays
    // with the id that actually lived it. spawns.callsign / tmux_window are
    // deliberately NOT rewritten — the successor INHERITS the callsign, so the
    // frozen window name still matches windowName(port, callsign).
    reassignActiveSpawns: db.prepare(`UPDATE spawns SET session_id = ?
      WHERE session_id = ? AND status IN ('provisioning', 'spawning', 'stalled', 'live')`),
    // Undelivered mail follows the card, not the id the human never sees.
    reassignPendingMail: db.prepare(`UPDATE mail SET to_session = ?
      WHERE to_session = ? AND delivered_at IS NULL AND expired_at IS NULL`),
    // The human's surviving question queue follows too (hold-kind rows were
    // already expired by hookSessionEnd's expireAllForSession on the /clear).
    reassignPendingQuestions: db.prepare(`UPDATE questions SET session_id = ?
      WHERE session_id = ? AND status = 'pending'`),
    // The file ledger follows as well, and this one is not bookkeeping: the
    // conflict radar counts a rival as "a DIFFERENT session_id whose row still
    // exists" (ledger.mjs). The retired predecessor's row does still exist
    // (archived, not deleted), so leaving its touches behind would make the
    // successor collide with its own past self — a hazard banner reading
    // "wren-a9e1 and wren-a9e1 both touching X".
    reassignTouches: db.prepare('UPDATE file_touches SET session_id = ? WHERE session_id = ?'),
    // Boot heal (reconcileClearForks): pre-0.7.1 rows have no cleared_at, so a
    // stranded pair is reconstructed from the event log instead. These two read
    // the 24h-retained events table.
    lastEventOf: db.prepare(`SELECT hook_event, note, at FROM events
      WHERE session_id = ? ORDER BY at DESC LIMIT 1`),
    clearBornSessionsSince: db.prepare(`SELECT DISTINCT e.session_id, e.at FROM events e
      WHERE e.hook_event = 'SessionStart' AND e.note = 'session clear' AND e.at BETWEEN ? AND ?
      ORDER BY e.at ASC`),
    // A session can be succeeded by exactly ONE heir, and an heir can continue
    // exactly ONE lineage. Without this, the boot heal could hand the same heir
    // to two different predecessors — merging two unrelated conversations onto
    // one card, which is strictly worse than the split it set out to fix.
    successorClaimed: db.prepare('SELECT session_id FROM sessions WHERE succeeded_by = ? LIMIT 1'),
    insertTouch: db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?, ?, ?, ?, ?, ?)'),
    recentTouches: db.prepare('SELECT * FROM file_touches WHERE repo_id = ? AND rel_path = ? AND at > ? ORDER BY at'),
    // M-G1: windowed by time. The snapshot GROUP-BY used to scan the WHOLE
    // (never-pruned-for-live-sessions) file_touches table on every frame; it
    // now only aggregates touches newer than the ledger window (retentionSweep
    // deletes the rest), and the snapshot caps the per-session list on top.
    // R2-7: rank each file by its MOST RECENT touch, newest first, so the
    // snapshot's per-session cap keeps the files a card touched most recently.
    // (Was MIN(at) ASC, which kept a busy card's OLDEST 50 and dropped its
    // newest — the exact files the human is watching.)
    filesBySession: db.prepare('SELECT session_id, abs_path, MAX(at) AS recent FROM file_touches WHERE at > ? GROUP BY session_id, abs_path ORDER BY recent DESC'),
    insertMail: db.prepare('INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?, ?, ?, ?, NULL)'),
    pendingMail: db.prepare('SELECT * FROM mail WHERE to_session = ? AND delivered_at IS NULL AND expired_at IS NULL ORDER BY at, id'),
    // /api/watch v2 claim: oldest undelivered mail from ANY sender (v1
    // claimed fleetdeck-answer rows only).
    nextMail: db.prepare('SELECT * FROM mail WHERE to_session = ? AND delivered_at IS NULL AND expired_at IS NULL ORDER BY at, id LIMIT 1'),
    // v1.1 `assign auto` routing (POST /command contract): deterministic,
    // zero model calls. Candidates = non-ended sessions whose col is not
    // offline/needsyou (stuck sessions get no new work), scoped to a repo
    // when given (matched against repo_id OR repo_name; all three ?s receive
    // the same repo value, NULL = unscoped). Rank: col idle → queued →
    // (working|verifying); ties → fewest undelivered mail, then most recent
    // last_seen. LIMIT 1 = the winner.
    autoCandidate: db.prepare(`SELECT s.*,
        (SELECT COUNT(*) FROM mail m
          WHERE m.to_session = s.session_id AND m.delivered_at IS NULL AND m.expired_at IS NULL) AS undelivered
      FROM sessions s
      WHERE s.ended_at IS NULL
        AND s.col NOT IN ('offline', 'needsyou')
        AND (? IS NULL OR s.repo_id = ? OR s.repo_name = ?)
      ORDER BY CASE s.col WHEN 'idle' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,
        undelivered ASC,
        s.last_seen DESC
      LIMIT 1`),
    pendingCounts: db.prepare(`SELECT to_session, COUNT(*) AS n, MIN(at) AS oldest_at
      FROM mail WHERE delivered_at IS NULL AND expired_at IS NULL GROUP BY to_session`),
    markDelivered: db.prepare('UPDATE mail SET delivered_at = ? WHERE id = ?'),
    unmarkDelivered: db.prepare('UPDATE mail SET delivered_at = NULL WHERE id = ?'),
    insertEvent: db.prepare('INSERT INTO events (session_id, hook_event, tool_name, note, at) VALUES (?, ?, ?, ?, ?)'),
    sparkline: db.prepare('SELECT session_id, (at / 60000) AS minute, COUNT(*) AS n FROM events WHERE at > ? GROUP BY session_id, minute'),
    insertTicker: db.prepare('INSERT INTO ticker (at, msg) VALUES (?, ?)'),
    recentTicker: db.prepare('SELECT at, msg FROM ticker ORDER BY id DESC LIMIT 40'),
    trimTicker: db.prepare('DELETE FROM ticker WHERE id <= (SELECT MAX(id) FROM ticker) - 500'),
    insertConflict: db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)'),
    recentConflicts: db.prepare('SELECT * FROM conflicts ORDER BY id DESC LIMIT 20'),
    // Manual cleanup (CONTRACT): "Clear" means the board shows only what is
    // still alive. A conflict between two dead sessions, a feed narrating
    // yesterday, and a file ledger full of ghosts are all noise the human
    // explicitly asked to be rid of — the radar re-raises a real conflict the
    // moment two live sessions touch the same file again.
    allConflicts: db.prepare('SELECT id, sessions_json FROM conflicts'),
    deleteConflict: db.prepare('DELETE FROM conflicts WHERE id = ?'),
    clearTicker: db.prepare('DELETE FROM ticker'),
    aliveSessionIds: db.prepare('SELECT session_id FROM sessions WHERE ended_at IS NULL AND archived_at IS NULL'),
    deleteDeadTouches: db.prepare(`DELETE FROM file_touches WHERE session_id IN (
      SELECT session_id FROM sessions WHERE ended_at IS NOT NULL OR archived_at IS NOT NULL)`),
    deleteArchivedMail: db.prepare(`DELETE FROM mail WHERE to_session IN (
      SELECT session_id FROM sessions WHERE archived_at IS NOT NULL)`),
    insertCommand: db.prepare('INSERT INTO commands (at, text, parsed_json) VALUES (?, ?, ?)'),
    pruneEvents: db.prepare('DELETE FROM events WHERE at < ?'),
    // M-G1: the append-only ledgers grew unbounded (file_touches for live
    // sessions, commands, conflicts, and settled mail). Age them out of the DB
    // on the retention cadence. Pending mail (delivered_at IS NULL AND
    // expired_at IS NULL) is a real queue and is NEVER pruned by age here — the
    // existing archival/expiry paths own its lifecycle.
    pruneTouches: db.prepare('DELETE FROM file_touches WHERE at < ?'),
    pruneCommands: db.prepare('DELETE FROM commands WHERE at < ?'),
    pruneConflicts: db.prepare('DELETE FROM conflicts WHERE at < ?'),
    pruneSettledMail: db.prepare('DELETE FROM mail WHERE at < ? AND (delivered_at IS NOT NULL OR expired_at IS NOT NULL)'),
    // v1.2 board-spawned sessions. "Active" = status spawning|stalled|live — the
    // rows that get liveness-checked, and the number the board shows as "N live".
    insertSpawn: db.prepare(`INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status, skip_permissions, remote_control)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'spawning', ?, ?)`),
    // H-R6: a spawn's durable row now exists BEFORE any external op (worktree
    // add / tmux window) so a crash in that gap can never orphan a worktree or
    // pane with no owning row. It is born 'provisioning' — excluded from
    // activeSpawns (never liveness-checked or counted live) until its pane
    // exists — and flipped to 'spawning' once launch succeeds.
    insertProvisionalSpawn: db.prepare(`INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status, skip_permissions, remote_control)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'provisioning', ?, ?)`),
    setSpawnWorktree: db.prepare('UPDATE spawns SET worktree_path = ? WHERE spawn_id = ?'),
    staleProvisioningSpawns: db.prepare("SELECT * FROM spawns WHERE status = 'provisioning'"),
    // H-R5 / R2-5: the newest spawn row still laying claim to a tmux window (a
    // revive reuses the dead row's window name, so a lineage can have several
    // rows naming one window). 'killed'/'gone' rows have RELEASED the window.
    // 'provisioning' rows DO count now: spawn() and revive() insert the durable
    // row as a provisional owner BEFORE the pane is created, precisely so a
    // stale-id kill arriving during the newWindow await sees the NEW row as the
    // window's owner and is refused. Without that, a revive's just-created pane
    // could be killed by the OLD (terminal) spawn_id while the new row did not
    // yet exist (R2-5). A kill by any OTHER id than this current owner is a
    // stale/historical request and must be refused.
    currentWindowOwner: db.prepare(`SELECT * FROM spawns
      WHERE tmux_window = ? AND status IN ('provisioning', 'spawning', 'stalled', 'live', 'pane-dead')
      ORDER BY requested_at DESC, rowid DESC LIMIT 1`),
    getSpawn: db.prepare('SELECT * FROM spawns WHERE spawn_id = ?'),
    spawnBySession: db.prepare('SELECT * FROM spawns WHERE session_id = ? ORDER BY requested_at DESC, rowid DESC LIMIT 1'),
    allSpawns: db.prepare('SELECT * FROM spawns ORDER BY requested_at, rowid'),
    activeSpawns: db.prepare("SELECT * FROM spawns WHERE status IN ('spawning', 'stalled', 'live')"),
    // BUG 3: 'pane-dead' and 'gone' were a ONE-WAY DOOR — activeSpawns never
    // re-checked them, so a spawn wrongly condemned (BUG 1 /clear, BUG 2
    // silence, or a transient tmux misread) stayed dead on the board forever
    // even while its pane kept running claude. The liveness tick re-checks
    // these against tmux and RESURRECTS any whose window is a live claude.
    // 'killed' is deliberately absent: a human kill is a decision, not a
    // mistake, and must stay killed.
    resurrectableSpawns: db.prepare("SELECT * FROM spawns WHERE status IN ('pane-dead', 'gone')"),
    activeSpawnBySession: db.prepare("SELECT * FROM spawns WHERE session_id = ? AND status IN ('spawning', 'stalled', 'live') ORDER BY requested_at DESC, rowid DESC LIMIT 1"),
    // HIGH (revive single-flight): activeSpawnBySession deliberately EXCLUDES
    // 'provisioning' (a provisional row is not yet a live-eligible spawn). But
    // revive()'s duplicate-guard must ALSO see a provisioning row: a revive
    // inserts its durable row 'provisioning' before the pane is up, and a
    // second revive arriving while the first is still bringing that pane up
    // must be refused, not allowed to launch a second pane for the one session.
    provisioningSpawnBySession: db.prepare("SELECT * FROM spawns WHERE session_id = ? AND status = 'provisioning' ORDER BY requested_at DESC, rowid DESC LIMIT 1"),
    countActiveSpawns: db.prepare("SELECT COUNT(*) AS n FROM spawns WHERE status IN ('spawning', 'stalled', 'live')"),
    setSpawnStatus: db.prepare('UPDATE spawns SET status = ? WHERE spawn_id = ?'),
    setSpawnRemote: db.prepare('UPDATE spawns SET remote_control = 1, remote_url = ? WHERE spawn_id = ?'),
    // WORKTREE OWNERSHIP CONTRACT: this is the allow-list behind the removal
    // API. A path is removable only when it appears here; no path supplied by
    // a browser is ever promoted into a git argv before this lookup succeeds.
    // Newest-first lets one revive lineage collapse to its current spawn row.
    worktreeSpawns: db.prepare(`SELECT spawns.*, sessions.ended_at AS session_ended_at
      FROM spawns LEFT JOIN sessions ON sessions.session_id = spawns.session_id
      WHERE spawns.worktree_path IS NOT NULL
      ORDER BY spawns.requested_at DESC, spawns.rowid DESC`),
    deleteWorktreeSpawns: db.prepare('DELETE FROM spawns WHERE worktree_path = ?'),
    deleteEndedSession: db.prepare('DELETE FROM sessions WHERE session_id = ? AND ended_at IS NOT NULL'),
    presumeDeadSessions: db.prepare(`SELECT * FROM sessions
      WHERE source = 'hooks' AND ended_at IS NULL
        AND col IN ('queued', 'idle', 'needsyou') AND last_seen < ?`),
    archiveCandidates: db.prepare(`SELECT * FROM sessions
      WHERE col = 'offline' AND archived_at IS NULL
        AND COALESCE(ended_at, last_seen) < ?`),
    setArchived: db.prepare('UPDATE sessions SET archived_at = ? WHERE session_id = ? AND archived_at IS NULL'),
    archiveAllOffline: db.prepare("UPDATE sessions SET archived_at = ? WHERE col = 'offline' AND archived_at IS NULL"),
    expireRetainedMail: db.prepare(`UPDATE mail SET expired_at = ?
      WHERE delivered_at IS NULL AND expired_at IS NULL
        AND to_session IN (SELECT session_id FROM sessions
          WHERE archived_at IS NOT NULL OR ended_at < ?)`),
    expireArchivedMail: db.prepare(`UPDATE mail SET expired_at = ?
      WHERE delivered_at IS NULL AND expired_at IS NULL
        AND to_session IN (SELECT session_id FROM sessions WHERE archived_at IS NOT NULL)`),
    goneArchivedSpawns: db.prepare(`UPDATE spawns SET status = 'gone'
      WHERE status NOT IN ('killed', 'pane-dead', 'gone')
        AND session_id IN (SELECT session_id FROM sessions WHERE archived_at IS NOT NULL)`),
    orphanWorktrees: db.prepare(`SELECT DISTINCT spawns.worktree_path FROM spawns
      JOIN sessions ON sessions.session_id = spawns.session_id
      WHERE spawns.worktree_path IS NOT NULL
        AND (sessions.col = 'offline' OR sessions.archived_at IS NOT NULL)
      ORDER BY spawns.worktree_path`),
    // Pre-created card for a board spawn (CONTRACT flow step 1): source
    // 'spawned' from birth so (a) the agents-cli absence sweep — which only
    // touches source='agents-cli' — never tombstones a still-booting spawn,
    // and (b) the first real hook event flips it to 'hooks' as usual.
    insertSpawnedSession: db.prepare(`INSERT INTO sessions
      (session_id, callsign, cwd, repo_id, repo_name, branch, worktree, col, note, task, events, started_at, last_seen, blocked_this_turn, source)
      VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', 'spawning…', ?, 0, ?, ?, 0, 'spawned')`),
    // v1.3 plan library (CONTRACT "B. Plan library"). Capture happens at
    // ExitPlanMode PermissionRequest intake (hookHoldQuestion), synchronously
    // with the question row insert. /state ships raw plan_md only — title
    // derivation is a board concern.
    insertPlan: db.prepare(`INSERT INTO plans
      (session_id, callsign, repo_id, repo_name, question_id, plan_md, created_at, status)
      VALUES (?, ?, ?, ?, ?, ?, ?, 'proposed')`),
    getPlan: db.prepare('SELECT * FROM plans WHERE plan_id = ?'),
    planByQuestion: db.prepare('SELECT * FROM plans WHERE question_id = ? ORDER BY plan_id DESC LIMIT 1'),
    plansForState: db.prepare(`SELECT * FROM plans WHERE status != 'archived'
      ORDER BY created_at DESC, plan_id DESC LIMIT 20`),
    setPlanStatus: db.prepare('UPDATE plans SET status = ? WHERE plan_id = ?'),
    setPlanExecuted: db.prepare("UPDATE plans SET status = 'executed', executed_via = ? WHERE plan_id = ?"),
  };

  const FIELDS = ['callsign', 'model', 'cwd', 'repo_id', 'repo_name', 'branch', 'worktree',
    'col', 'note', 'task', 'last_tool', 'events', 'last_seen', 'ended_at', 'blocked_this_turn', 'source',
    'notification_type', 'archived_at', 'ticket', 'ticket_source', 'prev_callsign',
    // 0.7.0 Move-to-tmux (adopt): the arm deadline + bypass choice + the
    // hook-proven end reason are all written through updateSession. No new
    // INSERT — insertProvisionalSpawn already attaches a spawns row to any
    // session_id without minting a card, which is exactly what adopt needs.
    'adopt_armed_until', 'adopt_armed_skip', 'end_reason',
    // 0.7.1 /clear succession + custom names.
    'cleared_at', 'succeeded_by', 'custom_suffix'];
  // M-P8: updateSession is the hottest write path (every hook event runs it
  // one to three times). Each call used to compile a brand-new UPDATE, so
  // SQLite re-parsed and re-planned identical statements forever. The set of
  // distinct column-shapes is small and enumerable (one per updater code
  // path), so cache the prepared statement keyed by the joined column list and
  // reuse it. The `?` order still matches `keys` order within a shape.
  const updateStmts = new Map(); // "col,col,…" -> prepared UPDATE
  // Set membership beats a linear FIELDS.includes() scan on this same hottest
  // path — FIELDS is 24 entries and growing, and every update filters every key.
  const FIELD_SET = new Set(FIELDS);
  function updateSession(sid, upd) {
    const keys = Object.keys(upd).filter(k => FIELD_SET.has(k));
    if (!keys.length) return;
    const shape = keys.join(',');
    let stmt = updateStmts.get(shape);
    if (!stmt) {
      stmt = db.prepare(`UPDATE sessions SET ${keys.map(k => `${k} = ?`).join(', ')} WHERE session_id = ?`);
      updateStmts.set(shape, stmt);
    }
    stmt.run(...keys.map(k => upd[k] ?? null), sid);
  }

  return { q, FIELDS, updateSession };
}
