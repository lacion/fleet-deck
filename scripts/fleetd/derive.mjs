// derive.mjs — fleetd core: the spike's applyEvent state derivation ported
// onto SQLite, plus file ledger / conflicts (F4), mail (F2), the SessionStart
// brief (F1) and the /state snapshot.
//
// Columns are DERIVED, never self-reported:
//   queued → working → verifying → needsyou → idle → offline
// Transition rules are a faithful port of fleetdeck-spike/server/fleetd.mjs.

import path from 'node:path';
import fs from 'node:fs';
import { randomUUID } from 'node:crypto';
import { execFile } from 'node:child_process';
import { deriveRepo, branchOf, ledgerKey } from './repo-identity.mjs';
import { createQuestions, resolveHoldMs, detectTrailingQuestion, lastAssistantText } from './questions.mjs';
import * as tmuxAdapter from './spawn.mjs';

const CALLSIGNS = ['falcon', 'otter', 'raven', 'lynx', 'orca', 'wren', 'viper', 'heron', 'badger', 'comet', 'ember', 'drift'];
const CONFLICT_WINDOW_MS = 30 * 60 * 1000;
const EDIT_TOOLS = ['Edit', 'Write', 'MultiEdit', 'NotebookEdit'];
const TEST_RUNNER_RE = /\b(pytest|jest|vitest|go test|cargo test|npm (run )?test)\b/; // spike regex, verbatim

// v1.2 env knobs (resolved once per core; tests spawn fresh daemons):
//   FLEETDECK_MAX_SPAWNED — spawn cap, counting live spawn rows (default 5)
//   FLEETDECK_STALE_MS    — stale badge threshold for working/verifying cards
//                           with no events (default 600 000 = 10 min)
//   FLEETDECK_NUDGE_MS    — bring-up nudge delay (contract: 8 s; env is a
//                           test hook only, same spirit as FLEETDECK_AGENTS_POLL_MS)
function envInt(name, fallback, { min = 0 } = {}) {
  const n = Number(process.env[name]);
  return Number.isFinite(n) && n >= min ? Math.floor(n) : fallback;
}

export function createCore(db, { port = 4711, holdMs = resolveHoldMs() } = {}) {
  const t0 = Date.now();
  let onMutate = () => {};
  const MAX_SPAWNED = envInt('FLEETDECK_MAX_SPAWNED', 5);
  const STALE_MS = envInt('FLEETDECK_STALE_MS', 600_000, { min: 1 });
  const NUDGE_MS = envInt('FLEETDECK_NUDGE_MS', 8_000, { min: 1 });

  // ------------------------------------------------------------- statements
  const q = {
    getSession: db.prepare('SELECT * FROM sessions WHERE session_id = ?'),
    allSessions: db.prepare('SELECT * FROM sessions ORDER BY started_at'),
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
    insertTouch: db.prepare('INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?, ?, ?, ?, ?, ?)'),
    recentTouches: db.prepare('SELECT * FROM file_touches WHERE repo_id = ? AND rel_path = ? AND at > ? ORDER BY at'),
    filesBySession: db.prepare('SELECT session_id, abs_path, MIN(at) AS first FROM file_touches GROUP BY session_id, abs_path ORDER BY first'),
    insertMail: db.prepare('INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?, ?, ?, ?, NULL)'),
    pendingMail: db.prepare('SELECT * FROM mail WHERE to_session = ? AND delivered_at IS NULL ORDER BY at'),
    // /api/watch v2 claim: oldest undelivered mail from ANY sender (v1
    // claimed fleetdeck-answer rows only).
    nextMail: db.prepare('SELECT * FROM mail WHERE to_session = ? AND delivered_at IS NULL ORDER BY at, id LIMIT 1'),
    // v1.1 `assign auto` routing (POST /command contract): deterministic,
    // zero model calls. Candidates = non-ended sessions whose col is not
    // offline/needsyou (stuck sessions get no new work), scoped to a repo
    // when given (matched against repo_id OR repo_name; all three ?s receive
    // the same repo value, NULL = unscoped). Rank: col idle → queued →
    // (working|verifying); ties → fewest undelivered mail, then most recent
    // last_seen. LIMIT 1 = the winner.
    autoCandidate: db.prepare(`SELECT s.*,
        (SELECT COUNT(*) FROM mail m
          WHERE m.to_session = s.session_id AND m.delivered_at IS NULL) AS undelivered
      FROM sessions s
      WHERE s.ended_at IS NULL
        AND s.col NOT IN ('offline', 'needsyou')
        AND (? IS NULL OR s.repo_id = ? OR s.repo_name = ?)
      ORDER BY CASE s.col WHEN 'idle' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,
        undelivered ASC,
        s.last_seen DESC
      LIMIT 1`),
    pendingCounts: db.prepare('SELECT to_session, COUNT(*) AS n FROM mail WHERE delivered_at IS NULL GROUP BY to_session'),
    markDelivered: db.prepare('UPDATE mail SET delivered_at = ? WHERE id = ?'),
    insertEvent: db.prepare('INSERT INTO events (session_id, hook_event, tool_name, note, at) VALUES (?, ?, ?, ?, ?)'),
    sparkline: db.prepare('SELECT session_id, (at / 60000) AS minute, COUNT(*) AS n FROM events WHERE at > ? GROUP BY session_id, minute'),
    insertTicker: db.prepare('INSERT INTO ticker (at, msg) VALUES (?, ?)'),
    recentTicker: db.prepare('SELECT at, msg FROM ticker ORDER BY id DESC LIMIT 40'),
    trimTicker: db.prepare('DELETE FROM ticker WHERE id <= (SELECT MAX(id) FROM ticker) - 500'),
    insertConflict: db.prepare('INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)'),
    recentConflicts: db.prepare('SELECT * FROM conflicts ORDER BY id DESC LIMIT 20'),
    insertCommand: db.prepare('INSERT INTO commands (at, text, parsed_json) VALUES (?, ?, ?)'),
    pruneEvents: db.prepare('DELETE FROM events WHERE at < ?'),
    // v1.2 board-spawned sessions. "Active" = status spawning|live — the rows
    // that count against FLEETDECK_MAX_SPAWNED and get liveness-checked.
    insertSpawn: db.prepare(`INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status, skip_permissions)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'spawning', ?)`),
    getSpawn: db.prepare('SELECT * FROM spawns WHERE spawn_id = ?'),
    spawnBySession: db.prepare('SELECT * FROM spawns WHERE session_id = ? ORDER BY requested_at DESC LIMIT 1'),
    allSpawns: db.prepare('SELECT * FROM spawns ORDER BY requested_at'),
    activeSpawns: db.prepare("SELECT * FROM spawns WHERE status IN ('spawning', 'live')"),
    countActiveSpawns: db.prepare("SELECT COUNT(*) AS n FROM spawns WHERE status IN ('spawning', 'live')"),
    setSpawnStatus: db.prepare('UPDATE spawns SET status = ? WHERE spawn_id = ?'),
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

  // F3 needs-you relay (questions.mjs): question rows + hold-open management.
  // mail/tick are function declarations below — hoisted, so passing them here
  // is safe; onMutate is captured lazily through the arrow.
  const questions = createQuestions(db, {
    holdMs,
    mail: (sid, from, text) => mail(sid, from, text),
    tick: msg => tick(msg),
    callsignOf: sid => q.getSession.get(sid)?.callsign ?? null,
    onChange: () => onMutate(),
    // v1.3 plan library: the plans table lives here; questions.mjs only needs
    // the link (plan_id for /state) and the answer-path status flips. The
    // flip is guarded to 'proposed' — the answer paths describe the freshly
    // captured plan, and a plan the human already archived/marked from the
    // library keeps that verdict.
    planIdFor: questionId => q.planByQuestion.get(questionId)?.plan_id ?? null,
    planAnswered: (questionId, behavior) => {
      const p = q.planByQuestion.get(questionId);
      if (!p || p.status !== 'proposed') return;
      const status = behavior === 'allow' ? 'approved'
        : behavior === 'capture' ? 'captured'
        : 'rejected';
      q.setPlanStatus.run(status, p.plan_id);
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) ${status}`);
    },
  });

  const FIELDS = ['callsign', 'model', 'cwd', 'repo_id', 'repo_name', 'branch', 'worktree',
    'col', 'note', 'task', 'last_tool', 'events', 'last_seen', 'ended_at', 'blocked_this_turn', 'source',
    'notification_type'];
  function updateSession(sid, upd) {
    const keys = Object.keys(upd).filter(k => FIELDS.includes(k));
    if (!keys.length) return;
    db.prepare(`UPDATE sessions SET ${keys.map(k => `${k} = ?`).join(', ')} WHERE session_id = ?`)
      .run(...keys.map(k => upd[k] ?? null), sid);
  }

  // ------------------------------------------------------------------ cards
  function assignCallsign(sid) {
    const idx = q.countSessions.get().n;
    return CALLSIGNS[idx % CALLSIGNS.length] + '-' + String(sid).slice(0, 4);
  }

  function card(sid) {
    let c = q.getSession.get(sid);
    if (!c) {
      const callsign = assignCallsign(sid);
      const now = Date.now();
      q.insertSession.run(sid, callsign, now, now);
      c = q.getSession.get(sid);
      tick(`${callsign} joined the fleet`);
    }
    return c;
  }

  function tick(msg) {
    q.insertTicker.run(Date.now(), msg);
    q.trimTicker.run();
  }

  function logEvent(sid, hookEvent, toolName, note) {
    q.insertEvent.run(sid, hookEvent ?? null, toolName ?? null, note ?? null, Date.now());
  }

  // ------------------------------------------------------------------- mail
  function mail(toSession, from, text) {
    q.insertMail.run(toSession, from, String(text ?? '').slice(0, 500), Date.now());
    // v1.1 mail-wake: ANY mail landing in the mailbox wakes any /api/watch
    // long-poll for that session — board answers, [FLEETDECK ASSIGNMENT]
    // routing and plain board/session mail alike (v1 nudged only on
    // fleetdeck-answer). The poll does its own undelivered check and never
    // claims for an offline session — this is only a nudge, never a delivery.
    notifyWatchers(toSession);
  }

  function drainMail(sid) {
    const box = q.pendingMail.all(sid);
    const now = Date.now();
    for (const m of box) q.markDelivered.run(now, m.id);
    return box.map(m => ({ from: m.from_id, text: m.text, at: m.at }));
  }

  // resolve a /mail "to" target to session ids
  function resolveTargets(to) {
    const all = q.allSessions.all();
    const active = all.filter(s => s.ended_at == null);
    if (to === 'all') return active.map(s => s.session_id);
    const m = /^repo:(.+)$/.exec(String(to ?? ''));
    if (m) {
      const key = m[1];
      return active
        .filter(s => s.repo_id === key || s.repo_name === key)
        .map(s => s.session_id);
    }
    return all
      .filter(s => s.session_id === to || s.callsign === to)
      .map(s => s.session_id);
  }

  // --------------------------------------------- file ledger + conflict radar
  // A recent touch counts even if that session already ended — its uncommitted
  // edits are exactly what you're about to clobber (spike rule, kept).
  function recordFile(sid, absFile, editorCard) {
    if (!absFile) return null;
    const now = Date.now();
    const abs = path.isAbsolute(absFile) ? absFile : path.resolve(editorCard.cwd || '/', absFile);
    const key = ledgerKey(abs, editorCard);

    const touches = q.recentTouches.all(key.repo_id ?? '', key.rel_path, now - CONFLICT_WINDOW_MS);
    const rivalTouches = touches.filter(t => t.session_id !== sid && q.getSession.get(t.session_id));
    const rivals = [...new Set(rivalTouches.map(t => t.session_id))];
    q.insertTouch.run(key.repo_id ?? '', key.rel_path, abs, sid, key.worktree ?? null, now);

    if (!rivals.length) return null;

    // Severity: warning same worktree, info across worktrees of one repo.
    const sameTree = rivalTouches.some(t => (t.worktree ?? null) === (key.worktree ?? null));
    const severity = sameTree ? 'warning' : 'info';
    const rivalNames = rivals.map(r => card(r).callsign).join(', ');
    q.insertConflict.run(now, key.repo_id ?? '', key.rel_path, severity, JSON.stringify([sid, ...rivals]));
    tick(`⚠ conflict: ${editorCard.callsign} and ${rivalNames} both touching ${path.basename(key.rel_path)}`);
    for (const r of rivals) {
      mail(r, 'fleetdeck', severity === 'warning'
        ? `Heads up: ${editorCard.callsign} is also editing ${key.rel_path}. Coordinate before you overwrite each other.`
        : `Heads up: ${editorCard.callsign} is editing ${key.rel_path} in another worktree of this repo — a future merge conflict announcing itself early.`);
    }
    return { file: key.rel_path, abs, rivals: rivalNames, severity };
  }

  function whisperText(conflict) {
    const base = `[FLEETDECK] ⚠ Session(s) ${conflict.rivals} recently edited ${conflict.file} too`;
    return conflict.severity === 'info'
      ? `${base} (in another worktree of this repo — a future merge conflict). Check their intent before building on this file, and mention the coordination in your final summary.`
      : `${base}. Re-read the file before further edits and avoid reverting their work. Mention this coordination in your final summary.`;
  }

  // ---------------------------------------------- hook event -> card state
  // Faithful port of the spike's applyEvent switch.
  function applyEvent(ev) {
    const sid = ev.session_id || 'unknown';
    let c = card(sid);
    // Precedence rule (handoff F1): hook-derived state ALWAYS wins. The
    // moment a real hook event arrives for a session — even one first
    // discovered via the agents-cli poller — its source flips to 'hooks' for
    // good, and the poller must never touch its column again after this.
    if (c.source !== 'hooks') {
      updateSession(sid, { source: 'hooks' });
      // v1.2: the session's FIRST hook event is the bring-up proof for a
      // board spawn — its spawn row (if still 'spawning') flips to 'live'.
      // Later statuses (pane-dead/killed/gone) are never revived here.
      const sp = q.spawnBySession.get(sid);
      if (sp && sp.status === 'spawning') {
        q.setSpawnStatus.run('live', sp.spawn_id);
        tick(`🛰 ${c.callsign} pane is live (first hook event)`);
      }
      c = { ...c, source: 'hooks' };
    }
    const upd = { last_seen: Date.now(), events: c.events + 1 };
    if (ev.cwd) {
      upd.cwd = ev.cwd;
      const repo = deriveRepo(ev.cwd);
      upd.repo_id = repo.repo_id;
      upd.repo_name = repo.repo_name;
      upd.worktree = repo.worktree;
      const branch = branchOf(ev.cwd) || ev.git_branch || null; // server-side; payload value only as fallback
      if (branch) upd.branch = branch;
    }
    if (ev.model?.display_name || ev.model?.id || typeof ev.model === 'string') {
      upd.model = ev.model.display_name || ev.model.id || ev.model;
    }
    updateSession(sid, upd);
    c = { ...c, ...upd };

    let conflict = null;
    const set = {};
    switch (ev.hook_event_name) {
      case 'SessionStart':
        set.col = 'queued';
        set.note = `session ${ev.source || 'startup'}`;
        break;
      case 'UserPromptSubmit':
        set.col = 'working';
        set.task = c.task || (ev.prompt || '').slice(0, 80);
        set.note = 'prompt: ' + (ev.prompt || '').slice(0, 60);
        set.notification_type = null; // activity clears the needs-you reason (F3e)
        tick(`${c.callsign} got a prompt`);
        break;
      case 'PreToolUse':
      case 'PostToolUse': {
        set.col = c.col === 'needsyou' ? 'working' : (c.col === 'queued' ? 'working' : c.col);
        set.notification_type = null; // activity clears the needs-you reason (F3e)
        set.last_tool = ev.tool_name ?? null;
        const input = ev.tool_input || {};
        const file = input.file_path || input.notebook_path;
        if (EDIT_TOOLS.includes(ev.tool_name) && file) {
          conflict = recordFile(sid, file, c);
          set.note = `editing ${path.basename(file)}`;
        } else if (ev.tool_name === 'Bash' && input.command) {
          const cmd = String(input.command);
          if (TEST_RUNNER_RE.test(cmd)) {
            set.col = 'verifying';
            set.note = 'running tests';
          } else {
            set.note = 'sh: ' + cmd.slice(0, 50);
          }
        } else {
          set.note = ev.tool_name;
        }
        break;
      }
      case 'FileChanged': {
        // Bash-side edits net (F4): feed the ledger, do not move the column.
        const file = ev.file_path || ev.tool_input?.file_path || ev.path || null;
        if (file) {
          conflict = recordFile(sid, file, c);
          set.note = `changed ${path.basename(file)}`;
        }
        break;
      }
      case 'Notification': {
        // F3e safety net: the board must always SHOW a stuck session, with
        // the reason. notification_type values (docs §8): permission_prompt,
        // idle_prompt, elicitation_dialog, agent_needs_input are needs-you
        // situations; auth_success / elicitation_complete /
        // elicitation_response / agent_completed are progress reports, not
        // requests for attention — those update the note but don't move the
        // column. Unknown/absent types keep the Phase 1 behavior (needsyou).
        const ntype = ev.notification_type ?? null;
        const RESOLVED_TYPES = ['auth_success', 'elicitation_complete', 'elicitation_response', 'agent_completed'];
        set.notification_type = ntype;
        set.note = (ev.message || ntype || 'needs attention').slice(0, 80);
        if (!RESOLVED_TYPES.includes(ntype)) {
          set.col = 'needsyou';
          tick(`🖐 ${c.callsign} needs you${ntype ? ` (${ntype})` : ''}: ${(ev.message || '').slice(0, 50)}`);
        }
        break;
      }
      case 'PermissionRequest':
        // F3a: the session is waiting on a human decision (relay card is
        // created by hookHoldQuestion; this is the telemetry side).
        // F3c side effect (validated): AskUserQuestion rides the permission
        // machinery — its PermissionRequest fires right before the native
        // terminal chooser renders (and only on the {} path: a board answer
        // denies at PreToolUse and short-circuits this event entirely). The
        // http layer already answered {} without holding; this is telemetry.
        set.col = 'needsyou';
        if (ev.tool_name === 'AskUserQuestion') {
          set.note = 'question open in the terminal';
          tick(`🖐 ${c.callsign} has a question open in the terminal`);
        } else {
          set.note = `permission: ${ev.tool_name || 'tool'}`;
          tick(`🖐 ${c.callsign} awaits permission: ${ev.tool_name || 'tool'}`);
        }
        break;
      case 'AskUserQuestion': {
        // F3c: the model asked a structured question (AskUserQuestion
        // PreToolUse, held by hookHoldQuestion). The terminal chooser renders
        // only AFTER the held hook responds, so during the hold the board is
        // the only place it can be answered.
        const first = (Array.isArray(ev.tool_input?.questions) && ev.tool_input.questions[0]?.question)
          || 'structured question';
        set.col = 'needsyou';
        set.note = ('choice: ' + first).slice(0, 80);
        tick(`🖐 ${c.callsign} asks: ${String(first).slice(0, 50)}`);
        break;
      }
      case 'Elicitation':
        // F3b: an MCP server is waiting on form input.
        set.col = 'needsyou';
        set.note = `elicitation: ${ev.message || ev.matcher || 'MCP input requested'}`.slice(0, 80);
        tick(`🖐 ${c.callsign} awaits input (elicitation)`);
        break;
      case 'Stop':
        set.col = 'idle';
        set.note = 'turn finished, waiting';
        tick(`${c.callsign} finished a turn`);
        break;
      case 'SessionEnd':
        set.col = 'offline';
        set.ended_at = Date.now();
        set.note = 'session ended' + (ev.reason ? ` (${ev.reason})` : '');
        tick(`${c.callsign} left the fleet`);
        break;
      default:
        set.note = ev.hook_event_name;
    }
    updateSession(sid, set);
    c = { ...c, ...set };
    logEvent(sid, ev.hook_event_name, ev.tool_name, c.note);
    onMutate();
    return { card: c, conflict };
  }

  // ------------------------------------------------------ hook endpoints
  function hookSessionStart(ev) {
    const { card: c } = applyEvent({ ...ev, hook_event_name: 'SessionStart' });
    return { ok: true, callsign: c.callsign, brief: composeBrief(c) };
  }

  function composeBrief(c) {
    const others = q.allSessions.all()
      .filter(s => s.session_id !== c.session_id && s.ended_at == null);
    const sameRepo = others.filter(s => (s.repo_id ?? null) === (c.repo_id ?? null));
    const elsewhere = others.filter(s => (s.repo_id ?? null) !== (c.repo_id ?? null));
    const otherRepos = new Set(elsewhere.map(s => s.repo_id ?? '(none)')).size;
    const repoLabel = c.repo_name ? ` in ${c.repo_name}` : '';
    const lines = [
      `[FLEETDECK] You are on the fleet board as "${c.callsign}" — live at http://127.0.0.1:${port}`,
      sameRepo.length
        ? `Other active sessions${repoLabel} (${sameRepo.length}):`
        : `No other sessions active${repoLabel} right now.`,
      ...sameRepo.map(s =>
        `  - ${s.callsign} [${s.col}] ${s.note}${s.branch ? ' — ' + s.branch : ''}${s.worktree && s.worktree !== c.worktree ? ' @ ' + s.worktree : ''}`),
    ];
    if (elsewhere.length) {
      lines.push(`${elsewhere.length} more session${elsewhere.length === 1 ? '' : 's'} across ${otherRepos} other repo${otherRepos === 1 ? '' : 's'}.`);
    }
    lines.push('Fleetdeck will warn you in-context if you touch files another session is editing. Take those warnings seriously: coordinate, don’t clobber.');
    return lines.join('\n');
  }

  function hookUserPromptSubmit(ev) {
    const sid = ev.session_id || 'unknown';
    applyEvent({ ...ev, hook_event_name: 'UserPromptSubmit' });
    q.setBlocked.run(0, sid); // new turn started — clear the one-block-per-turn flag
    // F3e auto-resolution: activity settles this session's pending
    // permission/elicitation questions (live holds fail open with {};
    // freeform questions stay pending — they're the human's queue).
    questions.expireOnActivity(sid);
    const box = drainMail(sid);
    if (!box.length) return {};
    onMutate();
    return {
      hookSpecificOutput: {
        hookEventName: 'UserPromptSubmit',
        additionalContext: '[FLEETDECK]\n' + box.map(m => `✉ from ${m.from}: ${m.text}`).join('\n'),
      },
    };
  }

  function hookPostToolUse(ev) {
    const { conflict } = applyEvent({ ...ev, hook_event_name: ev.hook_event_name || 'PostToolUse' });
    questions.expireOnActivity(ev.session_id || 'unknown'); // F3e auto-resolution
    if (!conflict) return {};
    return {
      hookSpecificOutput: {
        hookEventName: 'PostToolUse',
        additionalContext: whisperText(conflict),
      },
    };
  }

  // Turn boundary: deliver queued mail by refusing to stop — AT MOST ONCE per
  // turn per session, enforced server-side via blocked_this_turn. The flag
  // clears on the next UserPromptSubmit or the next Stop that passes with no
  // mail. NEVER reads stop_hook_active. Stop is never a tombstone.
  function hookStop(ev) {
    const sid = ev.session_id || 'unknown';
    const c = card(sid);
    if (!c.blocked_this_turn) {
      const box = drainMail(sid);
      if (box.length) {
        q.setBlocked.run(1, sid);
        // telemetry for the blocked stop (card stays in-turn; no idle transition)
        updateSession(sid, { last_seen: Date.now(), events: c.events + 1, col: 'working', note: 'processing fleet mail' });
        logEvent(sid, 'Stop', null, 'mail delivered via block');
        tick(`✉ ${c.callsign} got fleet mail at the turn boundary`);
        onMutate();
        const msgs = box.map(m => `from ${m.from}: ${m.text}`).join(' | ');
        return {
          decision: 'block',
          reason: `[FLEETDECK MAIL] ${msgs} — Act on this if it affects your work (briefly), then finish your turn. Do not start unrelated work.`,
        };
      }
    }
    // Stop passes.
    applyEvent({ ...ev, hook_event_name: 'Stop' });
    const stillPending = q.pendingMail.all(sid).length > 0;
    if (!stillPending) q.setBlocked.run(0, sid); // cleared on a Stop that passes with no mail
    detectFreeform(ev); // F3d — only on a PASSING Stop (a block continues the turn)
    return {};
  }

  // ------------------------------------------------ F3d free-text questions
  // Runs on every Stop that PASSES (a Stop answered with a mail block skips
  // detection — the turn continues and a later Stop will look again).
  // Input preference: payload.last_assistant_message when the CLI sends one
  // (docs §6 say 2.1.206 does NOT — hook-payloads.jsonl capture pins the
  // truth), else the transcript tail at payload.transcript_path.
  function detectFreeform(ev) {
    const sid = ev.session_id || 'unknown';
    try {
      const fromPayload = typeof ev.last_assistant_message === 'string' && ev.last_assistant_message.trim()
        ? ev.last_assistant_message : null;
      const text = fromPayload ?? (ev.transcript_path ? lastAssistantText(ev.transcript_path) : null);
      const question = detectTrailingQuestion(text);
      if (!question) return;
      // one pending card per distinct question text per session (a re-Stop
      // with the same trailing question must not spam the queue)
      const dup = questions.pendingOf(sid).some(r => {
        if (r.kind !== 'freeform') return false;
        try { return JSON.parse(r.payload_json || '{}').text === question; } catch { return false; }
      });
      if (dup) return;
      questions.create('freeform', sid, { text: question });
      const c = card(sid);
      updateSession(sid, { col: 'needsyou', note: ('Q: ' + question).slice(0, 80) });
      logEvent(sid, 'Stop', null, 'trailing question → needsyou');
      tick(`❓ ${c.callsign} asked: ${question.slice(0, 60)}`);
      onMutate();
    } catch { /* detection is best-effort — never disturb the Stop response */ }
  }

  // ----------------------------------------- F3a/F3b/F3c hold-open intake
  // Creates the durable question row and applies telemetry (card → needsyou).
  // The HTTP layer parks the response and registers the hold via
  // questions.attachHold; all resolution paths live in questions.mjs.
  //
  // v1.3 plan CAPTURE (CONTRACT "B. Plan library"): an ExitPlanMode
  // PermissionRequest carries the full plan markdown in tool_input.plan.
  // Capture it BEFORE the hold, in the SAME synchronous tick as the question
  // row insert — a daemon crash mid-hold still has the plan. Capture is
  // unconditional and survives whatever happens to the planner/hold; the
  // question then holds NORMALLY (unlike the AskUserQuestion instant-{}
  // guard in http.mjs, which stays untouched).
  function hookHoldQuestion(ev, eventName) {
    const kind = eventName === 'Elicitation' ? 'elicitation'
      : eventName === 'AskUserQuestion' ? 'choice'
      : 'permission';
    applyEvent({ ...ev, hook_event_name: eventName });
    const sid = ev.session_id || 'unknown';
    const row = questions.create(kind, sid, ev);
    if (eventName === 'PermissionRequest' && ev?.tool_name === 'ExitPlanMode') {
      try {
        const c = q.getSession.get(sid); // applyEvent ensured the card exists
        const planMd = typeof ev.tool_input?.plan === 'string'
          ? ev.tool_input.plan
          : String(ev.tool_input?.plan ?? '');
        const info = q.insertPlan.run(sid, c?.callsign ?? null, c?.repo_id ?? null,
          c?.repo_name ?? null, row.id, planMd, Date.now());
        tick(`📋 ${c?.callsign ?? sid} proposed a plan — captured to the library (#${Number(info.lastInsertRowid)})`);
      } catch (err) {
        // capture must never break the hold — the question still relays
        console.error('fleetd plan capture error:', err);
      }
    }
    onMutate();
    return row;
  }

  // SessionEnd: THE tombstone — pending hold-kind questions die with it;
  // freeform questions outlive the session (answer deliverable on --resume).
  function hookSessionEnd(ev) {
    applyEvent({ ...ev, hook_event_name: 'SessionEnd' });
    questions.expireAllForSession(ev.session_id || 'unknown');
    // v1.2: SessionEnd on a spawned session does NOT kill its pane — the
    // human may want the scrollback (CONTRACT). It just updates the row: the
    // pane no longer hosts a live claude session, so the spawn stops counting
    // against FLEETDECK_MAX_SPAWNED right now (the ~10 s liveness tick would
    // reach the same verdict once the pane's claude exits, but under the
    // FLEETDECK_SPAWN_CMD override there is no pane to observe at all — this
    // direct update is the only path that frees the cap there).
    const sp = q.spawnBySession.get(ev.session_id || 'unknown');
    if (sp && (sp.status === 'spawning' || sp.status === 'live')) {
      q.setSpawnStatus.run('pane-dead', sp.spawn_id);
    }
    // F3d-2: wake any /api/watch long-poll so its watcher sees
    // session_alive:false and exits now instead of at its hold timeout.
    notifyWatchers(ev.session_id || 'unknown');
    return {};
  }

  // ---------------------------------------- F3d-2 /api/watch core surface
  // Consumed by http.mjs GET /api/watch (which documents the full response
  // contract) on behalf of scripts/fleet-watch.mjs, the asyncRewake watcher.
  const watchWaiters = new Map(); // session_id -> Set<fn>

  function notifyWatchers(sid) {
    for (const fn of [...(watchWaiters.get(sid) ?? [])]) {
      try { fn(); } catch { /* a dead waiter must not break the notifier */ }
    }
  }

  // Register a nudge callback for a session's watch long-polls. Returns the
  // unregister function. Callbacks fire on ANY mail insert (v1.1 mail-wake)
  // and SessionEnd; they carry NO payload — the poll re-runs its own
  // undelivered check.
  function addWatchWaiter(sid, fn) {
    if (!watchWaiters.has(sid)) watchWaiters.set(sid, new Set());
    watchWaiters.get(sid).add(fn);
    return () => {
      const set = watchWaiters.get(sid);
      if (set) { set.delete(fn); if (!set.size) watchWaiters.delete(sid); }
    };
  }

  // ATOMIC claim of the oldest undelivered mail for a session — ANY sender
  // (/api/watch v2; v1 claimed board answers only). mail.delivered_at is THE
  // single source of truth for delivery: this claim, the UserPromptSubmit
  // drain, the Stop-block drain and GET /mail all run synchronously on the
  // daemon's only thread and all filter on delivered_at IS NULL — whichever
  // runs first wins, every other path sees nothing. No mail can ever be
  // delivered twice.
  // `text` is returned RAW, its own frame included ([FLEETDECK ANSWER] …,
  // [FLEETDECK ASSIGNMENT] …, or plain board/session mail) — v2's
  // rewakeMessage is neutral, so each mail must carry its own frame.
  function claimMail(sid) {
    const m = q.nextMail.get(sid);
    if (!m) return null;
    q.markDelivered.run(Date.now(), m.id);
    onMutate();
    return { mail_id: m.id, at: m.at, from: m.from_id, text: m.text };
  }

  function watchInfo(sid) {
    const c = q.getSession.get(sid);
    return {
      session_alive: !!c && c.ended_at == null,
      // Informational in v2: the watcher keeps polling while session_alive
      // is true even at pending:0, because mail can arrive for an idle
      // session at any time. Still counts FREEFORM questions only —
      // permission/elicitation/choice answers ride their held hook response
      // and never become mail (a choice whose hold expired belongs to the
      // native terminal chooser permanently; a late board answer 409s).
      pending: questions.pendingOf(sid).filter(r => r.kind === 'freeform').length,
    };
  }

  // ------------------------------------------- agents-cli ingest (F1)
  // Secondary session source: `claude agents --json` catches sessions that
  // predate plugin install — no hook ever fired for them, so they'd
  // otherwise never appear on the board. Polled by scripts/fleetd/agents-poll.mjs
  // (~10s cadence); this function is the merge step.
  //
  // Precedence rule (critical, see also the source flip in applyEvent
  // above): hook-derived state ALWAYS wins. This may only:
  //   (a) create a card for a sessionId never seen before at all — marked
  //       source='agents-cli', callsign assigned normally, cwd/repo identity
  //       derived as usual, col mapped from state/status, note "seen via
  //       agents CLI", name -> task.
  //   (b) update col/note/lastSeen on a card whose source is STILL
  //       'agents-cli'. The instant a real hook event lands for a session,
  //       applyEvent flips its source to 'hooks' and this function leaves it
  //       completely alone from then on — including lastSeen.
  // Trust rules (learned on install day — the CLI's agent registry lies):
  //   1. Only `kind: "interactive"` entries are fleet sessions. Background
  //      entries are subagents living INSIDE a parent session, and the
  //      registry keeps them for hours after completion (observed: two
  //      "blocked" background agents from that morning's work rendered as
  //      phantom WORKING cards). They never belong on the board.
  //   2. An interactive entry must have a LIVE pid (kill(pid, 0)) — the
  //      registry can outlive the process.
  //   3. Absence tombstones agents-cli cards ONLY: a card this poller
  //      created, that hooks never claimed, and that the (filtered) poll no
  //      longer reports, is marked offline — the poller is the only
  //      lifecycle those cards have. Hook-sourced cards are untouched;
  //      SessionEnd remains their only tombstone.
  function pidAlive(pid) {
    if (!Number.isFinite(pid) || pid <= 0) return false;
    try { process.kill(pid, 0); return true; } catch { return false; }
  }

  function colFromAgentState(raw, isNew) {
    const s = String(raw ?? '').toLowerCase();
    if (s === 'busy' || s === 'running') return 'working';
    // 'waiting' is undocumented (the official hooks docs only list
    // busy/blocked state examples) but observed live on interactive sessions
    // paired with a waitingFor: "permission prompt" field — exactly the
    // needsyou situation Notification exists for, so it's grouped with
    // 'blocked' rather than treated as idle.
    if (s === 'blocked' || s === 'waiting') return 'needsyou';
    if (s === 'idle') return 'idle';
    // Unknown/missing state: a freshly-discovered card starts queued (never
    // yet observed working); an already-tracked agents-cli card falls back
    // to idle rather than flapping into an invented column.
    return isNew ? 'queued' : 'idle';
  }

  function ingestAgentsPoll(records) {
    if (!Array.isArray(records)) return;
    // Trust rules 1+2: interactive entries with a live pid are the only
    // records that count — for creation, update AND the absence sweep below.
    const live = records.filter(rec =>
      rec && typeof rec === 'object' && rec.sessionId
      && rec.kind === 'interactive' && pidAlive(rec.pid));

    for (const rec of live) {
      const sid = rec.sessionId;
      const rawState = rec.state ?? rec.status;
      const existing = q.getSession.get(sid);
      if (!existing) {
        const callsign = assignCallsign(sid);
        const cwd = rec.cwd || null;
        const repo = cwd ? deriveRepo(cwd) : { repo_id: null, repo_name: null, worktree: null };
        const branch = cwd ? branchOf(cwd) : null;
        const now = Date.now();
        const startedAt = Number.isFinite(rec.startedAt) ? rec.startedAt : now;
        q.insertAgentSession.run(
          sid, callsign, cwd, repo.repo_id ?? null, repo.repo_name ?? null,
          branch ?? null, repo.worktree ?? null, colFromAgentState(rawState, true),
          'seen via agents CLI', rec.name ?? null, startedAt, now,
        );
        tick(`${callsign} joined the fleet (agents CLI)`);
        onMutate();
      } else if (existing.source === 'agents-cli') {
        updateSession(sid, {
          col: colFromAgentState(rawState, false),
          note: 'seen via agents CLI',
          last_seen: Date.now(),
          ended_at: null, // reappearance revives an absence-tombstoned card
        });
        onMutate();
      }
      // existing.source === 'hooks': hook-derived state always wins here —
      // leave the card completely alone.
    }

    // Trust rule 3: absence sweep, agents-cli cards only.
    const liveSids = new Set(live.map(r => r.sessionId));
    for (const s of q.allSessions.all()) {
      if (s.source !== 'agents-cli' || s.ended_at != null) continue;
      if (liveSids.has(s.session_id)) continue;
      updateSession(s.session_id, {
        col: 'offline',
        note: 'no longer reported by agents CLI',
        ended_at: Date.now(),
      });
      tick(`${s.callsign} left the fleet (agents CLI)`);
      onMutate();
    }
  }

  // ------------------------------------- v1.2 board-spawned sessions (spawns)
  // CONTRACT "v1.2 — dynamic fleet". Spawn = explicit human click ONLY; the
  // spawned command is plain interactive `claude` (zero wrapper); no
  // auto-respawn, ever. All tmux construction is argv arrays (spawn.mjs).
  //
  // Row state machine:
  //   'spawning'  insert at POST /api/spawn
  //     → 'live'       first hook event for the pre-issued session_id
  //                    (the source flip in applyEvent above)
  //     → 'pane-dead'  liveness tick saw #{pane_dead}=1 or a bare shell
  //                    (crash, no SessionEnd), OR SessionEnd landed
  //                    (hookSessionEnd above — graceful end, pane kept)
  //     → 'gone'       boot reconciliation: window (exact scoped name) missing
  //     → 'killed'     POST /api/spawn/:id/kill succeeded (name-verified)
  //   'live' → pane-dead | gone | killed (same triggers)
  //   'pane-dead' → killed (the dead pane's window still exists until killed)
  //               → gone   (kill found the window already gone: 410)
  //   'killed' / 'gone' are terminal. Only spawning|live count against the cap.

  function execFileP(cmd, args, { timeout = 30_000 } = {}) {
    return new Promise((resolve) => {
      try {
        execFile(cmd, args, { timeout, windowsHide: true }, (err, stdout, stderr) => {
          if (err) return resolve({ ok: false, err: String(stderr || err.message || err).trim() });
          resolve({ ok: true, out: stdout });
        });
      } catch (err) {
        resolve({ ok: false, err: String(err.message || err) });
      }
    });
  }

  // Capability flag (/health and /state). available=false when tmux is absent
  // or FLEETDECK_SPAWN=off; the FLEETDECK_SPAWN_CMD override reports
  // available:true with reason 'test-override'. The board hides ALL spawn UI
  // when available is false.
  function spawnCapability() {
    const base = { max: MAX_SPAWNED, active: q.countActiveSpawns.get().n };
    if (String(process.env.FLEETDECK_SPAWN ?? '').toLowerCase() === 'off') {
      return { available: false, reason: 'disabled (FLEETDECK_SPAWN=off)', ...base };
    }
    if (tmuxAdapter.spawnOverrideCmd()) {
      return { available: true, reason: 'test-override', ...base };
    }
    if (!tmuxAdapter.hasTmux()) {
      return { available: false, reason: 'tmux not found on PATH', ...base };
    }
    return { available: true, ...base };
  }

  // CONTRACT flow step 1: pre-create the card so the callsign exists before
  // the pane does. Note is EXACTLY "spawning…"; col queued; source 'spawned'.
  function createSpawnedCard(sid, cwd, prompt) {
    const callsign = assignCallsign(sid);
    const repo = deriveRepo(cwd);
    const branch = cwd ? branchOf(cwd) : null;
    const now = Date.now();
    q.insertSpawnedSession.run(
      sid, callsign, cwd, repo.repo_id ?? null, repo.repo_name ?? null,
      branch ?? null, repo.worktree ?? null,
      prompt ? String(prompt).slice(0, 80) : null, now, now,
    );
    return q.getSession.get(sid);
  }

  function spawnFailed(sid, callsign, reason) {
    updateSession(sid, {
      col: 'offline', ended_at: Date.now(),
      note: `spawn failed: ${reason}`.slice(0, 80),
    });
    tick(`✗ spawn failed for ${callsign}: ${reason.slice(0, 60)}`);
    onMutate();
  }

  // Bring-up nudge — THE one keystroke injection permitted, ever: if the
  // spawn produced no hook event within NUDGE_MS (contract: 8 s) and the pane
  // is alive, send ONE Enter (trust dialog). Never again for that spawn —
  // `nudged` is checked-and-set before the keystroke goes out.
  const nudged = new Set();
  function scheduleNudge(spawn_id, window, callsign) {
    const t = setTimeout(async () => {
      try {
        if (nudged.has(spawn_id)) return;
        const row = q.getSpawn.get(spawn_id);
        if (!row || row.status !== 'spawning') return; // hook event already landed, or terminal
        const win = (await tmuxAdapter.listScopedWindows(port)).find(w => w.window === window);
        if (!win || win.pane_dead) return; // pane not alive → no keystroke
        nudged.add(spawn_id);
        await tmuxAdapter.sendBringupEnter(win.window_id);
        logEvent(row.session_id, 'SpawnNudge', null, 'bring-up Enter sent (trust dialog)');
        tick(`⏎ nudged ${callsign} through the trust dialog`);
        onMutate();
      } catch { /* nudge is best-effort; never disturb the daemon */ }
    }, NUDGE_MS);
    t.unref?.();
  }

  // POST /api/spawn — control API, fail-loud (CONTRACT: 4xx {ok:false,
  // reason} for no-tmux / bad-cwd / cap / bad-worktree; 409 when worktree is
  // requested outside git). Returns {status, body} for the HTTP layer.
  async function spawn(body) {
    const cap = spawnCapability();
    if (!cap.available) {
      return { status: 400, body: { ok: false, reason: `spawning unavailable: ${cap.reason}` } };
    }
    for (const k of ['cwd', 'prompt', 'model', 'permission_mode']) {
      if (body?.[k] != null && typeof body[k] !== 'string') {
        return { status: 400, body: { ok: false, reason: `${k} must be a string` } };
      }
    }
    if (body?.worktree != null && typeof body.worktree !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'worktree must be a boolean' } };
    }
    if (body?.dangerously_skip_permissions != null && typeof body.dangerously_skip_permissions !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'dangerously_skip_permissions must be a boolean' } };
    }
    // v1.3 unsupervised spawns (CONTRACT "A"): either form of bypass —
    // dangerously_skip_permissions:true (its own CLI flag) or
    // permission_mode:"bypassPermissions" (plain passthrough) — marks the row
    // and the /state spawn object. Both together are allowed (CLI semantics
    // decide what wins in the session).
    const skipPermissions = body?.dangerously_skip_permissions === true
      || body?.permission_mode === 'bypassPermissions';
    const cwd = body?.cwd || '';
    let st = null;
    try { st = fs.statSync(cwd); } catch { /* missing */ }
    if (!cwd || !st?.isDirectory()) {
      return { status: 400, body: { ok: false, reason: 'cwd missing or not a directory' } };
    }
    if (cap.active >= cap.max) {
      return { status: 409, body: { ok: false, reason: `spawn cap reached (${cap.active} of ${cap.max} live — FLEETDECK_MAX_SPAWNED)` } };
    }
    if (body?.worktree === true && !deriveRepo(cwd).is_git) {
      return { status: 409, body: { ok: false, reason: 'cwd is not a git repository — cannot spawn into a worktree' } };
    }

    // Step 1: pre-issue the session UUID and create the card (callsign exists
    // from here on; the first real hook event flips source to 'hooks').
    const session_id = randomUUID();
    const spawn_id = randomUUID();
    const c = createSpawnedCard(session_id, cwd, body?.prompt);
    const callsign = c.callsign;

    // Step 2: optional fresh worktree — the session cwd becomes the new path.
    let worktree_path = null;
    if (body?.worktree === true) {
      worktree_path = path.join(path.dirname(cwd), `${path.basename(cwd)}--fd-${callsign}`);
      const res = await execFileP('git', ['-C', cwd, 'worktree', 'add', '-b', `fd/${callsign}`, worktree_path]);
      if (!res.ok) {
        spawnFailed(session_id, callsign, `git worktree add: ${res.err}`);
        return { status: 409, body: { ok: false, reason: `git worktree add failed: ${res.err}`.slice(0, 300) } };
      }
      const repo = deriveRepo(worktree_path);
      updateSession(session_id, {
        cwd: worktree_path, repo_id: repo.repo_id, repo_name: repo.repo_name,
        worktree: repo.worktree, branch: branchOf(worktree_path),
      });
    }
    const runCwd = worktree_path ?? cwd;

    // Step 3: plain interactive `claude` — zero wrapper. Order pinned by the
    // contract: --session-id, then --model, --permission-mode, then the
    // prompt as ONE positional argv element. v1.3 appends
    // --dangerously-skip-permissions (before the positional prompt) when
    // requested; permission_mode "bypassPermissions" needs no argv change —
    // it rides --permission-mode as plain passthrough.
    const argv = ['claude', '--session-id', session_id];
    if (body?.model) argv.push('--model', body.model);
    if (body?.permission_mode) argv.push('--permission-mode', body.permission_mode);
    if (body?.dangerously_skip_permissions === true) argv.push('--dangerously-skip-permissions');
    if (body?.prompt) argv.push(body.prompt);

    const tmux_session = tmuxAdapter.sessionName(port);
    const tmux_window = tmuxAdapter.windowName(port, callsign);
    const override = tmuxAdapter.spawnOverrideCmd();
    if (override) {
      // Test override: argv [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)].
      const spec = {
        spawn_id, session_id, callsign, port,
        cwd: runCwd, requested_cwd: cwd,
        prompt: body?.prompt ?? null, model: body?.model ?? null,
        permission_mode: body?.permission_mode ?? null, worktree_path,
        dangerously_skip_permissions: body?.dangerously_skip_permissions === true,
        skip_permissions: skipPermissions,
        tmux: { session: tmux_session, window: tmux_window },
        argv, // the exact claude argv tmux would have run
      };
      tmuxAdapter.launchOverride(override, spec, err =>
        spawnFailed(session_id, callsign, `spawn override: ${err.message || err}`));
    } else {
      try {
        await tmuxAdapter.ensureSession(port);
        await tmuxAdapter.newWindow({ port, callsign, cwd: runCwd, argv });
      } catch (err) {
        spawnFailed(session_id, callsign, String(err.message || err));
        return { status: 500, body: { ok: false, reason: `tmux spawn failed: ${err.message || err}` } };
      }
    }

    // Step 4: durable spawn row; step 5: bring-up nudge timer.
    q.insertSpawn.run(spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, Date.now(), skipPermissions ? 1 : 0);
    tick(`🚀 spawned ${callsign} — tmux window ${tmux_window}${skipPermissions ? ' (unsupervised)' : ''}`);
    scheduleNudge(spawn_id, tmux_window, callsign);
    onMutate();
    return {
      status: 200,
      body: { ok: true, spawn_id, session_id, callsign, tmux: { session: tmux_session, window: tmux_window } },
    };
  }

  // POST /api/spawn/:id/kill — name-verified tmux kill-window (404 unknown
  // id, 409 card not offline without force, 410 window already gone).
  // "Stop" needs no endpoint: the board mails the session instead.
  async function spawnKill(spawn_id, force) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    const c = q.getSession.get(row.session_id);
    if (c && c.col !== 'offline' && force !== true) {
      return {
        status: 409,
        body: { ok: false, reason: `session ${c.callsign} is ${c.col}, not offline — pass force:true to kill anyway` },
      };
    }
    const res = await tmuxAdapter.killWindowVerified(row.tmux_window);
    if (res.gone) {
      // Discovery: the pane is already gone — settle the row AND the card
      // (same tombstone every terminal row state applies; only reachable for
      // a non-offline card via an explicit force:true).
      if (['spawning', 'live', 'pane-dead'].includes(row.status)) {
        q.setSpawnStatus.run('gone', spawn_id);
        if (c && c.ended_at == null) {
          updateSession(row.session_id, { col: 'offline', ended_at: Date.now(), note: 'spawned pane window gone' });
          notifyWatchers(row.session_id);
        }
        onMutate();
      }
      return { status: 410, body: { ok: false, reason: 'window already gone' } };
    }
    if (!res.ok) return { status: 500, body: { ok: false, reason: res.error || 'tmux kill-window failed' } };
    q.setSpawnStatus.run('killed', spawn_id);
    if (c && c.ended_at == null) {
      updateSession(row.session_id, { col: 'offline', ended_at: Date.now(), note: 'pane killed from the board' });
      notifyWatchers(row.session_id);
    }
    tick(`🗡 killed pane ${row.tmux_window}${force === true ? ' (forced)' : ''}`);
    onMutate();
    return { status: 200, body: { ok: true, spawn_id, status: 'killed' } };
  }

  // Owned-pane liveness (CONTRACT) — rides the agents-poll tick (~10 s), for
  // spawns rows in spawning|live only:
  //   pane_dead or a bare shell (sh|bash|zsh|zsh-*)  → confidently dead:
  //     row 'pane-dead', card offline with the --resume note
  //   'claude'                                        → alive
  //   anything else (incl. window/tmux unreachable)   → UNKNOWN, NO action —
  //     never confidently dead (firstmate's hard-won rule; a wrong "dead"
  //     costs a duplicate billed session, a wrong "alive" costs nothing).
  // The pane_dead flag matters because remain-on-exit panes keep reporting
  // the ORIGINAL command name after death (verified on tmux 3.7b) — the
  // command string alone would read "claude" forever.
  const SHELL_RE = /^(sh|bash|zsh|zsh-.*)$/;
  async function spawnLivenessTick() {
    const rows = q.activeSpawns.all();
    if (!rows.length && !spawnOrphans.length) return;
    const wins = await tmuxAdapter.listScopedWindows(port);
    for (const row of rows) {
      const win = wins.find(w => w.window === row.tmux_window);
      if (!win) continue; // gone/unreachable at runtime = unknown; boot reconciliation owns 'gone'
      if (!win.pane_dead && win.pane_cmd === 'claude') continue; // alive
      if (win.pane_dead || SHELL_RE.test(win.pane_cmd)) {
        q.setSpawnStatus.run('pane-dead', row.spawn_id);
        const c = q.getSession.get(row.session_id);
        if (c && c.ended_at == null) {
          updateSession(row.session_id, {
            col: 'offline', ended_at: Date.now(),
            note: `pane idle — resume with claude --resume ${row.session_id}`,
          });
          tick(`💀 ${c.callsign} pane died (claude no longer running) — window kept for scrollback`);
          notifyWatchers(row.session_id);
        }
        onMutate();
      }
      // anything else: unknown → no action
    }
    // Keep the boot-computed orphan list honest: windows that disappear
    // stop being listed (informational only — no ops are ever offered).
    const owned = new Set(q.allSpawns.all().map(r => r.tmux_window));
    const orphans = wins.filter(w => !owned.has(w.window)).map(w => ({ window: w.window }));
    if (JSON.stringify(orphans) !== JSON.stringify(spawnOrphans)) {
      spawnOrphans = orphans;
      onMutate();
    }
  }

  // Restart reconciliation (fleetd boot): spawn rows outlive the daemon in
  // SQLite while the panes outlive it in tmux — re-join the two. Rows in
  // spawning|live whose window (exact scoped name) is gone → 'gone' + card
  // offline; scoped windows with no row at all → spawn_orphans ("unadopted"
  // on the board; surfaced, never operated on).
  let spawnOrphans = [];
  async function reconcileSpawns() {
    const wins = await tmuxAdapter.listScopedWindows(port);
    const names = new Set(wins.map(w => w.window));
    for (const row of q.activeSpawns.all()) {
      if (names.has(row.tmux_window)) continue;
      q.setSpawnStatus.run('gone', row.spawn_id);
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        updateSession(row.session_id, {
          col: 'offline', ended_at: Date.now(),
          note: 'spawned pane gone (daemon restart reconciliation)',
        });
        tick(`${c.callsign} pane gone — noticed at daemon restart`);
      }
      onMutate();
    }
    const owned = new Set(q.allSpawns.all().map(r => r.tmux_window));
    spawnOrphans = wins.filter(w => !owned.has(w.window)).map(w => ({ window: w.window }));
    if (spawnOrphans.length) {
      tick(`⚠ ${spawnOrphans.length} unadopted fleetdeck window(s) in tmux (fd${port}-* with no spawn row)`);
      onMutate();
    }
  }

  // ------------------------------------------- v1.3 plan library (mark)
  // POST /api/plans/:id/mark {status, via?} — control API, real status codes.
  // Transition matrix (CONTRACT "B. Plan library" LIBRARY):
  //   → executed:  from proposed | approved | captured  (optional {via}
  //                string recorded on the row); rejected/executed/archived
  //                → 409 bad transition.
  //   → archived:  from ANY non-archived status (proposed, approved,
  //                captured, rejected, executed); archived → 409.
  // Any other target status → 400 (this endpoint only marks executed /
  // archived; the answer paths own approved/captured/rejected). 404 unknown.
  // Execution/assignment is client-composed on existing machinery — this
  // endpoint only records the verdict.
  const EXECUTABLE_FROM = new Set(['proposed', 'approved', 'captured']);
  function planMark(plan_id, body) {
    const p = q.getPlan.get(Number(plan_id));
    if (!p) return { status: 404, body: { ok: false, err: 'no such plan' } };
    const target = body?.status;
    if (target !== 'executed' && target !== 'archived') {
      return { status: 400, body: { ok: false, err: 'status must be "executed" or "archived"' } };
    }
    if (body?.via != null && typeof body.via !== 'string') {
      return { status: 400, body: { ok: false, err: 'via must be a string' } };
    }
    if (target === 'executed') {
      if (!EXECUTABLE_FROM.has(p.status)) {
        return { status: 409, body: { ok: false, err: `cannot mark a ${p.status} plan executed` } };
      }
      const via = body?.via?.trim() ? body.via.trim().slice(0, 200) : null;
      q.setPlanExecuted.run(via, p.plan_id);
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) marked executed${via ? ` via ${via}` : ''}`);
    } else {
      if (p.status === 'archived') {
        return { status: 409, body: { ok: false, err: 'plan is already archived' } };
      }
      q.setPlanStatus.run('archived', p.plan_id);
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) archived`);
    }
    onMutate();
    return { status: 200, body: { ok: true, plan_id: p.plan_id, status: target } };
  }

  // ------------------------------------------------------------- commands
  function parseCommand(text) {
    const t = String(text ?? '').trim();
    let m;
    if ((m = /^broadcast\s+(.+)$/is.exec(t))) return { cmd: 'broadcast', text: m[1].trim() };
    if ((m = /^assign\s+(\S+)\s+(.+)$/is.exec(t))) {
      const target = m[1];
      // v1.1 auto-routing: `assign auto <text>` / `assign auto:<repo> <text>`.
      // Repo names can contain dots and dashes (and repo_ids are absolute
      // paths), so split the target on the FIRST colon only — everything
      // after it is the repo key, verbatim. Bare `auto:` degrades to
      // unscoped auto.
      if (target === 'auto' || target.startsWith('auto:')) {
        const repo = target.length > 'auto:'.length ? target.slice('auto:'.length) : null;
        return { cmd: 'assign_auto', repo, text: m[2].trim() };
      }
      return { cmd: 'assign', target, text: m[2].trim() };
    }
    return { cmd: 'note', text: t };
  }

  function command(text) {
    const parsed = parseCommand(text);
    const logCommand = extra =>
      q.insertCommand.run(Date.now(), String(text ?? ''), JSON.stringify(extra ? { ...parsed, ...extra } : parsed));
    let delivered = 0;
    if (parsed.cmd === 'broadcast') {
      const targets = resolveTargets('all');
      targets.forEach(sid => mail(sid, 'orchestrator', parsed.text));
      delivered = targets.length;
      tick(`📣 orchestrator broadcast → ${delivered} session(s)`);
    } else if (parsed.cmd === 'assign_auto') {
      // v1.1 deterministic auto-routing (POST /command contract). The
      // candidate/ranking policy lives entirely in q.autoCandidate above —
      // zero model calls, one SQL round. The same repo key feeds all three
      // placeholders (NULL = unscoped, else matched against repo_id OR
      // repo_name).
      const repo = parsed.repo ?? null;
      const winner = q.autoCandidate.get(repo, repo, repo);
      if (!winner) {
        logCommand({ unrouted: true });
        tick('⚠ assign auto: no available session — task logged');
        onMutate();
        // v1.2 unrouted CTA: carry the task text so the board can render a
        // "spawn a session for this" button with the prompt prefilled.
        // Routing itself NEVER spawns.
        return { ok: false, unrouted: true, text: parsed.text };
      }
      const assigned_to = { session_id: winner.session_id, callsign: winner.callsign };
      mail(winner.session_id, 'orchestrator', `[FLEETDECK ASSIGNMENT] ${parsed.text}`);
      tick(`⚡ orchestrator → ${winner.callsign}: ${parsed.text.slice(0, 60)}`);
      logCommand({ assigned_to });
      onMutate();
      return { ok: true, assigned_to };
    } else if (parsed.cmd === 'assign') {
      const targets = resolveTargets(parsed.target);
      // Same frame as auto-routing (v1.1): every routed task carries
      // [FLEETDECK ASSIGNMENT] so the wake path / doctrine skill can treat
      // assignments uniformly regardless of how they were targeted.
      targets.forEach(sid => mail(sid, 'orchestrator', `[FLEETDECK ASSIGNMENT] ${parsed.text}`));
      delivered = targets.length;
      tick(`📌 orchestrator assign → ${parsed.target}${delivered ? '' : ' (no such session)'}`);
    } else {
      tick(`📝 orchestrator note: ${parsed.text.slice(0, 60)}`);
    }
    logCommand();
    onMutate();
    return { ok: true, parsed, delivered };
  }

  function postMail({ to, from, text }) {
    const targets = resolveTargets(to);
    targets.forEach(sid => mail(sid, from || 'human', text));
    tick(`✉ mail from ${from || 'human'} → ${to}`);
    onMutate();
    return { ok: true, delivered: targets.length };
  }

  // -------------------------------------------------------------- snapshot
  // Spike field names preserved; adds repo fields + sparkline + uptime.
  function snapshot() {
    const now = Date.now();
    const filesBySid = new Map();
    for (const row of q.filesBySession.all()) {
      if (!filesBySid.has(row.session_id)) filesBySid.set(row.session_id, []);
      filesBySid.get(row.session_id).push(row.abs_path);
    }
    const sparkBySid = new Map();
    const nowMin = Math.floor(now / 60000);
    for (const row of q.sparkline.all(now - 30 * 60000)) {
      if (!sparkBySid.has(row.session_id)) sparkBySid.set(row.session_id, new Array(30).fill(0));
      const idx = 29 - (nowMin - row.minute);
      if (idx >= 0 && idx < 30) sparkBySid.get(row.session_id)[idx] = row.n;
    }
    // v1.2: per-card spawn ownership (spawn object when a spawns row owns the
    // session) and the stale badge (working/verifying with no events for
    // FLEETDECK_STALE_MS — derived from lastSeen, zero new machinery).
    const spawnBySid = new Map();
    for (const r of q.allSpawns.all()) spawnBySid.set(r.session_id, r);
    const sessions = q.allSessions.all().map(s => {
      const sp = spawnBySid.get(s.session_id);
      return {
        session_id: s.session_id,
        callsign: s.callsign,
        model: s.model,
        cwd: s.cwd,
        branch: s.branch,
        col: s.col,
        note: s.note,
        task: s.task,
        files: filesBySid.get(s.session_id) || [],
        lastTool: s.last_tool,
        events: s.events,
        startedAt: s.started_at,
        lastSeen: s.last_seen,
        endedAt: s.ended_at,
        repo_id: s.repo_id,
        repo_name: s.repo_name,
        worktree: s.worktree,
        source: s.source,
        notification_type: s.notification_type ?? null, // F3e: WHY it needs you
        sparkline: sparkBySid.get(s.session_id) || new Array(30).fill(0),
        stale: (s.col === 'working' || s.col === 'verifying')
          && s.last_seen != null && (now - s.last_seen > STALE_MS),
        ...(sp ? {
          spawn: {
            spawn_id: sp.spawn_id,
            tmux_window: sp.tmux_window,
            status: sp.status,
            skip_permissions: !!sp.skip_permissions, // v1.3 unsupervised chip
          },
        } : {}),
      };
    });
    const repoMap = new Map();
    for (const s of sessions) {
      const key = s.repo_id ?? '(none)';
      if (!repoMap.has(key)) repoMap.set(key, { repo_id: s.repo_id, repo_name: s.repo_name, active: 0, total: 0 });
      const r = repoMap.get(key);
      r.total++;
      if (!s.endedAt) r.active++;
      if (s.repo_name) r.repo_name = s.repo_name;
    }
    const mailPending = {};
    for (const row of q.pendingCounts.all()) mailPending[row.to_session] = row.n;
    for (const s of sessions) if (!(s.session_id in mailPending)) mailPending[s.session_id] = 0;
    return {
      up_ms: now - t0,          // spike name, preserved
      uptime_ms: now - t0,      // contract addition
      sessions,
      repos: [...repoMap.values()],
      ticker: q.recentTicker.all(),
      conflicts: q.recentConflicts.all().map(c => ({
        at: c.at,
        repo_id: c.repo_id,
        rel_path: c.rel_path,
        file: c.rel_path,       // spike board reads .file
        severity: c.severity,
        sessions: JSON.parse(c.sessions_json || '[]'),
      })),
      mail_pending: mailPending,
      questions: questions.listForState(), // F3: pending + last few resolved
      spawn: spawnCapability(),            // v1.2 capability flag
      spawn_orphans: spawnOrphans,         // v1.2 "unadopted" scoped windows
      // v1.3 plan library: non-archived, newest first, cap 20. RAW plan_md
      // only — title derivation (first heading / first line) is a board
      // concern, never the daemon's.
      plans: q.plansForState.all().map(p => ({
        plan_id: p.plan_id,
        session_id: p.session_id,
        callsign: p.callsign,
        repo_id: p.repo_id,
        repo_name: p.repo_name,
        plan_md: p.plan_md,
        created_at: p.created_at,
        status: p.status,
      })),
    };
  }

  function fleetSize() {
    return q.countSessions.get().n;
  }

  // periodic hygiene: keep the events table from growing without bound
  setInterval(() => {
    try { q.pruneEvents.run(Date.now() - 24 * 3600 * 1000); } catch { /* hygiene only */ }
  }, 10 * 60 * 1000).unref();

  return {
    applyEvent,
    hookSessionStart,
    hookUserPromptSubmit,
    hookPostToolUse,
    hookStop,
    hookSessionEnd,
    hookHoldQuestion,
    questions, // F3 relay surface: attachHold / socketClosed / answer / …
    addWatchWaiter,  // F3d-2 watch surface (GET /api/watch v2)
    claimMail,       // "
    watchInfo,       // "
    drainMail,
    postMail,
    command,
    snapshot,
    fleetSize,
    ingestAgentsPoll,
    // v1.2 dynamic fleet
    spawn,             // POST /api/spawn flow → {status, body}
    spawnKill,         // POST /api/spawn/:id/kill → {status, body}
    spawnCapability,   // /health + /state `spawn` object
    spawnLivenessTick, // owned-pane liveness, rides the agents-poll cadence
    reconcileSpawns,   // fleetd boot: rows ↔ tmux windows
    // v1.3 plan library
    planMark,          // POST /api/plans/:id/mark → {status, body}
    set onMutate(fn) { onMutate = fn; },
  };
}
