// helpers.mjs — pure, closure-free helpers shared across the fleetd core
// modules. Nothing here reads `db`, the prepared statements, or any per-core
// state: every value comes in through arguments, so these functions are safe
// to import anywhere (and to unit-test in isolation).

import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';
import { CLAUDE_ENV_MARKERS } from './env-scrub.mjs';

// v1.2 env knobs are resolved once per core via this reader; see the knob doc
// in derive.mjs where each threshold is bound.
export function envInt(name, fallback, { min = 0 } = {}) {
  const n = Number(process.env[name]);
  return Number.isFinite(n) && n >= min ? Math.floor(n) : fallback;
}

// Claude stores one project directory per absolute cwd by replacing every
// slash and dot with a dash. Keep this pure and exported: revive eligibility,
// the launch guard, and unit tests must all agree on the exact on-disk name.
export function mungeClaudeProjectCwd(cwd) {
  return path.resolve(cwd).replace(/[\/.]/g, '-');
}

export function claudeTranscriptPath(cwd, sessionId, homeDir = os.homedir()) {
  return path.join(homeDir, '.claude', 'projects', mungeClaudeProjectCwd(cwd), `${sessionId}.jsonl`);
}

export function spawnRowRevivable(row) {
  const runCwd = row?.worktree_path ?? row?.cwd;
  return !!runCwd
    && ['pane-dead', 'killed', 'gone'].includes(row.status)
    && fs.existsSync(runCwd)
    && fs.existsSync(claudeTranscriptPath(runCwd, row.session_id));
}

// 0.7.0 Move-to-tmux: the "adopt NOW" predicate (snapshot `adopt.eligible ===
// 'now'`), shared by the snapshot and adoptSession so both agree on exactly
// when an OFFLINE card can be resumed into a board pane immediately:
//   • a hook-PROVEN end — ended_at set AND end_reason names a real hook end.
//     end_reason is an ALLOWLIST, not a blocklist: NULL means "no provenance
//     stamped" (a pre-0.7.0 row, an agents-cli absence guess, or one of the
//     tombstone writers that condemn without proof — liveness condemn,
//     reconcile-gone). Absence of proof is not proof of death: `claude
//     --resume` against a still-live CLI is a duplicate billed session, so
//     only 'presumed'-or-NULL-free rows are adopt-now-eligible (arm the rest —
//     truly dead cards just let the arm expire).
//   • ZERO spawn lineage — not merely "no ACTIVE row". Any spawn row, dead or
//     alive, means the board owns this session's pane story: revive owns dead
//     lineages (a second lineage would fight the first over the window and the
//     worktree bookkeeping).
//   • resume evidence still on disk: cwd is a DIRECTORY (statSync, matching
//     adoptSession's enforcement exactly — existsSync would pass a regular
//     file the launch then 410s on) + transcript exists. runCwd is
//     sessions.cwd (what claudeTranscriptPath munges), NEVER sessions.worktree.
// Two fs probes — same uncached cost contract as spawnRowRevivable; the
// snapshot runs it ONLY for offline cards (a live card takes the no-fs 'arm'
// path), so a frame never fs-probes the whole fleet.
function cwdIsDirectory(p) {
  if (!p) return false;
  try { return fs.statSync(p).isDirectory(); } catch { return false; }
}

// Ends that must never be resumed, and why each one is not a green light:
//   • null       — no provenance was ever stamped (a pre-0.7.0 row, an
//                  agents-CLI absence, a condemn without proof). Absence of
//                  proof is not proof: the CLI may still be alive.
//   • 'presumed' — retention GUESSED, from 3h of silence.
//   • 'superseded' (0.7.1) — the session did not stop, it CONTINUED under a new
//                  id after a /clear. The heir owns the card, the pane and the
//                  name; the retired id's transcript is a closed chapter.
// Resuming any of these mints a second billed session against a conversation
// that is either still live or already moved on. One owner, so the snapshot
// predicate below and adoptSession's own guard can never drift apart.
export const NOT_RESUMABLE_END = new Set([null, 'presumed', 'superseded']);

export function sessionAdoptableNow(session, hasSpawnRow) {
  if (!session) return false;
  if (session.ended_at == null) return false;  // still live → arm, not now
  if (NOT_RESUMABLE_END.has(session.end_reason ?? null)) return false;
  if (hasSpawnRow) return false;               // board-owned lineage → revive owns it
  const cwd = session.cwd;
  return cwdIsDirectory(cwd)
    && fs.existsSync(claudeTranscriptPath(cwd, session.session_id));
}

// CONTRACT: fresh spawn and revive share one environment wrapper. This is
// the single source of truth for inherited-agent/fleet scrubbing; callers add
// only the Claude invocation and its operation-specific argv. The Claude/agent
// session markers come from env-scrub.mjs (shared with fleet-sessionstart's
// bootEnv); the tmux plumbing + FLEETDECK_* tuning knobs below are this
// wrapper's own context-specific additions.
export function claudeEnvArgvPrefix(port, home) {
  const scrub = [
    ...CLAUDE_ENV_MARKERS,
    // FLEETDECK_*_CMD name fixture commands the daemon execs in place of a real
    // subprocess (SPAWN_CMD → the `claude` pane; TERM_CMD → termbridge's tmux
    // control client). A leaked one riding a pane's env into the next
    // SessionStart would make a fresh daemon exec the fixture instead of the
    // real thing — the same scar class as the test seams below, so scrub both.
    'FLEETDECK_AGENTS_CMD', 'FLEETDECK_SPAWN_CMD', 'FLEETDECK_TERM_CMD',
    'TMUX', 'TMUX_PANE', 'FLEETDECK_TMUX_SOCKET',
    'FLEETDECK_AGENTS_POLL_MS', 'FLEETDECK_HOLD_MS', 'FLEETDECK_STALE_MS',
    'FLEETDECK_NUDGE_MS', 'FLEETDECK_WATCH_MAX_MS',
    'FLEETDECK_WATCH_POLL_MS', 'FLEETDECK_SPAWN_REGISTER_MS',
    'FLEETDECK_PANE_MAIL_GRACE_MS', 'FLEETDECK_PRESUME_DEAD_MS',
    'FLEETDECK_RETAIN_OFFLINE_MS', 'FLEETDECK_RC_HARVEST_MS',
    'FLEETDECK_ADOPT_ARM_MS', 'FLEETDECK_ADOPT_DELAY_MS',
    // Test seams that must NEVER ride a pane's env into the next SessionStart:
    // a leaked FLEETDECK_TEST_DAEMON_SCRIPT would make every future daemon
    // (re)spawn launch an arbitrary script, and a leaked VERSION_OVERRIDE
    // permanently skews the upgrade-takeover comparison (the 2026-07-11 tmux
    // env-poisoning scar, new tenants).
    'FLEETDECK_TEST_DAEMON_SCRIPT', 'FLEETDECK_VERSION_OVERRIDE',
  ];
  return [
    'env', ...scrub.flatMap(name => ['-u', name]),
    `FLEETDECK_PORT=${port}`, `FLEETDECK_HOME=${home}`,
  ];
}

// Bounded-concurrency map: run `fn` over `items` with at most `limit` in
// flight, preserving input order in the result. Used by worktree inspection
// (four probes at a time) but generic.
export async function mapLimit(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  async function worker() {
    for (;;) {
      const i = next++;
      if (i >= items.length) return;
      out[i] = await fn(items[i]);
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return out;
}

// Restores write permission on the directories WE own — a read-only build
// artifact is our mess to clear — and silently steps over anything owned by
// someone else. Never chmods what it does not own, never recurses outside the
// worktree. Only ever called inside a path the daemon itself created (the
// caller has already proved that against the spawns table).
export function chmodWritableWhereOwned(root) {
  const uid = typeof process.getuid === 'function' ? process.getuid() : null;
  const walk = (dir, depth = 0) => {
    if (depth > 12) return; // a worktree is not a filesystem crawl
    let entries = [];
    try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      let st;
      try { st = fs.lstatSync(full); } catch { continue; }
      if (uid != null && st.uid !== uid) continue; // not ours — leave it alone
      try { fs.chmodSync(full, st.mode | 0o200); } catch { /* best effort */ }
      if (entry.isDirectory() && !entry.isSymbolicLink()) walk(full, depth + 1);
    }
  };
  try { walk(root); } catch { /* best effort: the retry will tell the truth */ }
}

// What actually stands in the way, named. A path we cannot unlink is one whose
// PARENT we cannot write to (that is what unlink(2) checks) — reporting the
// child alone would send the human chasing the wrong file.
export function blockedPaths(root, limit = 8) {
  const uid = typeof process.getuid === 'function' ? process.getuid() : null;
  const owners = new Map();
  const out = [];
  const ownerOf = st => {
    if (owners.has(st.uid)) return owners.get(st.uid);
    let name = `uid ${st.uid}`;
    try { name = st.uid === 0 ? 'root' : (os.userInfo().uid === st.uid ? os.userInfo().username : name); } catch { /* keep uid */ }
    owners.set(st.uid, name);
    return name;
  };
  const walk = (dir, depth = 0) => {
    if (out.length >= limit || depth > 12) return;
    let entries = [];
    try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      if (out.length >= limit) return;
      const full = path.join(dir, entry.name);
      let st;
      try { st = fs.lstatSync(full); } catch { continue; }
      if (uid != null && st.uid !== uid) {
        out.push({ path: full, owner: ownerOf(st) });
        continue; // do not descend into someone else's tree
      }
      if (entry.isDirectory() && !entry.isSymbolicLink()) walk(full, depth + 1);
    }
  };
  try { walk(root); } catch { /* nothing to add */ }
  return out;
}

export const shellQuote = s => (/^[A-Za-z0-9_@%+=:,./-]+$/.test(s) ? s : `'${String(s).replace(/'/g, `'\\''`)}'`);

// A live pid check — the agents-cli registry can outlive the process.
export function pidAlive(pid) {
  if (!Number.isFinite(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch { return false; }
}

export function colFromAgentState(raw, isNew) {
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

export function parseCommand(text) {
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
  // 0.6.0 ticket callsigns: `ticket <target> <PROJ-123|clear>`. Exactly two
  // tokens (the value is anchored last), so `ticket foo BAR extra` does NOT
  // match here and falls to the malformed branch below. A malformed or bare
  // `ticket …` must NEVER silently become a note — it carries an explicit error
  // the command handler surfaces loudly (an operator who fat-fingers a key
  // deserves a usage line, not a note that looks like it worked).
  if ((m = /^ticket\s+(\S+)\s+(\S+)\s*$/i.exec(t))) {
    return { cmd: 'ticket', target: m[1], ticket: m[2] };
  }
  if (/^ticket\b/i.test(t)) {
    return { cmd: 'ticket', error: 'usage: ticket <callsign-or-session-id> <PROJ-123|clear>' };
  }
  // 0.7.1 custom names: `name <target> <suffix|clear>`. Same shape and the same
  // never-silently-a-note rule as `ticket` above — the human renames a card by
  // its ID part, the animal is never theirs to choose.
  if ((m = /^name\s+(\S+)\s+(\S+)\s*$/i.exec(t))) {
    return { cmd: 'name', target: m[1], suffix: m[2] };
  }
  if (/^name\b/i.test(t)) {
    return { cmd: 'name', error: 'usage: name <callsign-or-session-id> <new-suffix|clear>' };
  }
  return { cmd: 'note', text: t };
}

// 0.7.1: the suffix half of <animal>-<suffix>, when a human chooses it.
// The charset is NOT cosmetic — the board's ticker filter matches callsigns on
// a [^A-Za-z0-9-] boundary and tmux window names are built as fd<port>-<callsign>,
// so a space, dot or underscore here would silently break a card's timeline and
// its pane addressing. Must start alphanumeric; 24 chars is plenty for a label
// and keeps the card from wrapping.
const NAME_SUFFIX_RE = /^[A-Za-z0-9][A-Za-z0-9-]{0,23}$/;
// Reserved: the mail router resolves these before it ever looks at a callsign,
// so a card named `all` could never be messaged directly.
const RESERVED_NAMES = new Set(['all', 'everyone', 'clear']);

export function validateNameSuffix(suffix) {
  if (!NAME_SUFFIX_RE.test(suffix)) {
    return 'a name is letters, digits and dashes only (start with a letter or digit, max 24)';
  }
  if (RESERVED_NAMES.has(suffix.toLowerCase()) || suffix.toLowerCase().startsWith('repo:')) {
    return `"${suffix}" is reserved — mail routing needs it`;
  }
  return null;
}

// Bare-shell command names: a remain-on-exit pane keeps reporting the ORIGINAL
// command after death, so `claude` alone reads live forever — a bare shell is
// how we recognise a dead/exited pane. Shared by the liveness tick, revive,
// and the silence sweep.
export const SHELL_RE = /^(sh|bash|zsh|zsh-.*)$/;
