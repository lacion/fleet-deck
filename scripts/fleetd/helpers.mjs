// helpers.mjs — pure, closure-free helpers shared across the fleetd core
// modules. Nothing here reads `db`, the prepared statements, or any per-core
// state: every value comes in through arguments, so these functions are safe
// to import anywhere (and to unit-test in isolation).

import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';
import { execFile } from 'node:child_process';
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

// CONTRACT: fresh spawn and revive share one environment wrapper. This is
// the single source of truth for inherited-agent/fleet scrubbing; callers add
// only the Claude invocation and its operation-specific argv. The Claude/agent
// session markers come from env-scrub.mjs (shared with fleet-sessionstart's
// bootEnv); the tmux plumbing + FLEETDECK_* tuning knobs below are this
// wrapper's own context-specific additions.
export function claudeEnvArgvPrefix(port, home) {
  const scrub = [
    ...CLAUDE_ENV_MARKERS,
    'FLEETDECK_AGENTS_CMD', 'FLEETDECK_SPAWN_CMD',
    'TMUX', 'TMUX_PANE', 'FLEETDECK_TMUX_SOCKET',
    'FLEETDECK_AGENTS_POLL_MS', 'FLEETDECK_HOLD_MS', 'FLEETDECK_STALE_MS',
    'FLEETDECK_NUDGE_MS', 'FLEETDECK_WATCH_MAX_MS',
    'FLEETDECK_WATCH_POLL_MS', 'FLEETDECK_SPAWN_REGISTER_MS',
    'FLEETDECK_PANE_MAIL_GRACE_MS', 'FLEETDECK_PRESUME_DEAD_MS',
    'FLEETDECK_RETAIN_OFFLINE_MS', 'FLEETDECK_RC_HARVEST_MS',
  ];
  return [
    'env', ...scrub.flatMap(name => ['-u', name]),
    `FLEETDECK_PORT=${port}`, `FLEETDECK_HOME=${home}`,
  ];
}

export function execFileP(cmd, args, { timeout = 30_000 } = {}) {
  return new Promise((resolve) => {
    try {
      execFile(cmd, args, { timeout, windowsHide: true }, (err, stdout, stderr) => {
        if (err) return resolve({ ok: false, code: err.code, err: String(stderr || err.message || err).trim() });
        resolve({ ok: true, out: stdout });
      });
    } catch (err) {
      resolve({ ok: false, err: String(err.message || err) });
    }
  });
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
  return { cmd: 'note', text: t };
}

// Bare-shell command names: a remain-on-exit pane keeps reporting the ORIGINAL
// command after death, so `claude` alone reads live forever — a bare shell is
// how we recognise a dead/exited pane. Shared by the liveness tick, revive,
// and the silence sweep.
export const SHELL_RE = /^(sh|bash|zsh|zsh-.*)$/;
