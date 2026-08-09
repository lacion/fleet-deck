// helpers.ts — pure, closure-free helpers shared across the fleetd core
// modules. Nothing here reads `db`, the prepared statements, or any per-core
// state: every value comes in through arguments, so these functions are safe
// to import anywhere (and to unit-test in isolation).

import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';
import { CLAUDE_ENV_MARKERS, GATEWAY_ENV_VARS, SPAWN_ENV_VARS } from './env-scrub.ts';

// v1.2 env knobs are resolved once per core via this reader; see the knob doc
// in derive.mjs where each threshold is bound.
export function envInt(name: string, fallback: number, { min = 0 }: { min?: number } = {}): number {
  const n = Number(process.env[name]);
  return Number.isFinite(n) && n >= min ? Math.floor(n) : fallback;
}

// Claude stores one project directory per absolute cwd by replacing every
// slash and dot with a dash. Keep this pure and exported: revive eligibility,
// the launch guard, and unit tests must all agree on the exact on-disk name.
export function mungeClaudeProjectCwd(cwd: string): string {
  return path.resolve(cwd).replace(/[/.]/g, '-');
}

export function claudeTranscriptPath(
  cwd: string,
  sessionId: string,
  homeDir: string = os.homedir(),
): string {
  return path.join(
    homeDir,
    '.claude',
    'projects',
    mungeClaudeProjectCwd(cwd),
    `${sessionId}.jsonl`,
  );
}

// The subset of a spawns row spawnRowRevivable reads. Structural, not the full
// SpawnRow — this leaf never touches the store, so it names only its own inputs.
interface RevivableSpawnRow {
  worktree_path?: string | null;
  cwd?: string | null;
  status: string;
  session_id: string;
}

export function spawnRowRevivable(row: RevivableSpawnRow | null | undefined): boolean {
  const runCwd = row?.worktree_path ?? row?.cwd;
  return (
    !!runCwd &&
    // runCwd truthy already implies row is non-null at runtime; the compiler
    // can't infer that through the optional-chain, so state it for `.status`.
    row != null &&
    ['pane-dead', 'killed', 'gone'].includes(row.status) &&
    fs.existsSync(runCwd) &&
    fs.existsSync(claudeTranscriptPath(runCwd, row.session_id))
  );
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
function cwdIsDirectory(p: string | null | undefined): p is string {
  if (!p) return false;
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
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
export const NOT_RESUMABLE_END = new Set<string | null>([null, 'presumed', 'superseded']);

// The subset of a sessions row sessionAdoptableNow reads — structural, mirrors
// spawnRowRevivable: this leaf names only the columns it inspects.
interface AdoptableSession {
  ended_at?: number | string | null;
  end_reason?: string | null;
  cwd?: string | null;
  session_id: string;
}

export function sessionAdoptableNow(
  session: AdoptableSession | null | undefined,
  hasSpawnRow: boolean,
): boolean {
  if (!session) return false;
  if (session.ended_at == null) return false; // still live → arm, not now
  if (NOT_RESUMABLE_END.has(session.end_reason ?? null)) return false;
  if (hasSpawnRow) return false; // board-owned lineage → revive owns it
  const cwd = session.cwd;
  return cwdIsDirectory(cwd) && fs.existsSync(claudeTranscriptPath(cwd, session.session_id));
}

// CONTRACT: fresh spawn and revive share one environment wrapper. This is
// the single source of truth for inherited-agent/fleet scrubbing; callers add
// only the Claude invocation and its operation-specific argv. The Claude/agent
// session markers come from env-scrub.mjs (shared with fleet-sessionstart's
// bootEnv); the tmux plumbing + FLEETDECK_* tuning knobs below are this
// wrapper's own context-specific additions.
//
// `keep` (0.15.0) names the variables this particular launch is DELIBERATELY
// supplying — today, the LLM-gateway set a `gateway:true` spawn hands to tmux
// via `new-window -e` (spawn.mjs). They must be excluded from the `-u` list or
// the scrub would win: tmux sets them in the pane's environment, then this very
// `env -u` strips them right back off before exec'ing claude, and the spawn
// would silently route to Anthropic after all. Excluding EXACTLY the injected
// names keeps the guarantee intact for every variable the launch did NOT set —
// an ambient ANTHROPIC_API_KEY is still scrubbed from a spawn that only supplies
// ANTHROPIC_AUTH_TOKEN. Default `[]` ⇒ byte-identical to the pre-0.15.0 prefix.
export function claudeEnvArgvPrefix(
  port: number,
  home: string,
  { keep = [] }: { keep?: readonly string[] } = {},
): string[] {
  const keepSet = new Set(keep);
  const scrub = [
    ...CLAUDE_ENV_MARKERS,
    // FLEETDECK_*_CMD name fixture commands the daemon execs in place of a real
    // subprocess (SPAWN_CMD → the `claude` pane; TERM_CMD → termbridge's tmux
    // control client). A leaked one riding a pane's env into the next
    // SessionStart would make a fresh daemon exec the fixture instead of the
    // real thing — the same scar class as the test seams below, so scrub both.
    'FLEETDECK_AGENTS_CMD',
    'FLEETDECK_SPAWN_CMD',
    'FLEETDECK_TERM_CMD',
    'TMUX',
    'TMUX_PANE',
    'FLEETDECK_TMUX_SOCKET',
    'FLEETDECK_AGENTS_POLL_MS',
    'FLEETDECK_HOLD_MS',
    'FLEETDECK_STALE_MS',
    'FLEETDECK_REARM_GRACE_MS',
    'FLEETDECK_NUDGE_MS',
    'FLEETDECK_WATCH_MAX_MS',
    'FLEETDECK_WATCH_POLL_MS',
    'FLEETDECK_SPAWN_REGISTER_MS',
    'FLEETDECK_SETUP_REGISTER_MS',
    'FLEETDECK_PANE_MAIL_GRACE_MS',
    'FLEETDECK_PRESUME_DEAD_MS',
    'FLEETDECK_PRESUME_DEAD_WORKING_MS',
    'FLEETDECK_RETAIN_OFFLINE_MS',
    'FLEETDECK_RC_HARVEST_MS',
    'FLEETDECK_ADOPT_ARM_MS',
    'FLEETDECK_ADOPT_DELAY_MS',
    // Test seams that must NEVER ride a pane's env into the next SessionStart:
    // a leaked FLEETDECK_TEST_DAEMON_SCRIPT would make every future daemon
    // (re)spawn launch an arbitrary script, and a leaked VERSION_OVERRIDE
    // permanently skews the upgrade-takeover comparison (the 2026-07-11 tmux
    // env-poisoning scar, new tenants).
    'FLEETDECK_TEST_DAEMON_SCRIPT',
    'FLEETDECK_VERSION_OVERRIDE',
    // The daemon's bearer. When the operator pins FLEETDECK_TOKEN in the env it
    // would otherwise ride tmux's global env into every pane — a live
    // credential handed to every agent (0.16.0). Agents that legitimately call
    // the API read $FLEETDECK_HOME/token instead (same file the shims use).
    'FLEETDECK_TOKEN',
    // LLM-gateway routing (see GATEWAY_ENV_VARS): whether a pane bills your
    // Anthropic account or a local proxy must come from the spawn, never from
    // whatever shell the daemon happened to boot in.
    ...GATEWAY_ENV_VARS,
    // Visible pre-Claude setup is likewise owned by one explicit spawn.
    ...SPAWN_ENV_VARS,
  ].filter((name) => !keepSet.has(name));
  return [
    'env',
    ...scrub.flatMap((name) => ['-u', name]),
    // Fleet Deck already owns the fleet board, so Claude Code's own background
    // agent view is redundant in a fleet pane — and worse, from a spawned pane a
    // human can arrow left into it and start launching nested agents that
    // scribble over the real board. Pin it OFF for every fleet-owned pane.
    // Merely SETTING the variable disables the view (any value works per the
    // Claude Code docs); it is a fixed UI setting, not a secret, so — unlike the
    // gateway credential kept out of argv — it is safe as a plain assignment
    // here. Placed BEFORE the FLEETDECK identity pair so PORT/HOME remain the
    // immediate lead-in to the command, the ordering the adapter/tests pin.
    'CLAUDE_CODE_DISABLE_AGENT_VIEW=1',
    `FLEETDECK_PORT=${port}`,
    `FLEETDECK_HOME=${home}`,
  ];
}

// A canonical-path async mutex: one in-flight holder per key at a time, FIFO
// waiters behind it. The daemon is single-threaded, so "acquire" is just a
// Map check — but the holder then AWAITS multi-second git subprocesses while
// still owning the path, and a second actor for the same path must queue
// behind it rather than interleave into that window. Worktree custody is the
// first user (removal holds the path through filesystem, branch, and DB
// cleanup; every launch/revive into that directory acquires the same claim),
// so a revive can never validate a still-standing directory that a removal is
// about to delete (the BUG-060 post-final-check race). The returned release
// MUST be invoked (idempotently — it releases at most once) on every exit
// path; callers use try/finally.
export function createKeyedMutex(): (key: string) => Promise<() => void> {
  const tails = new Map<string, Promise<void>>(); // canonical key -> promise chain tail
  return async function acquire(key: string): Promise<() => void> {
    const tail = tails.get(key) ?? Promise.resolve();
    let releaseNow: () => void = () => {
      /* replaced synchronously by the Promise executor below */
    };
    const mine = new Promise<void>((resolve) => {
      releaseNow = resolve;
    });
    tails.set(
      key,
      tail.then(() => mine),
    );
    await tail;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      if (tails.get(key) === mine) tails.delete(key);
      releaseNow();
    };
  };
}

// Canonical identity for a claim key: the real path when it resolves,
// path.resolve otherwise (symlinked spellings of the same directory must
// contend; a missing path still needs a stable key). Pure, mirrors
// repos.mjs's canonicalTarget.
export function canonicalPathKey(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}

// Bounded-concurrency map: run `fn` over `items` with at most `limit` in
// flight, preserving input order in the result. Used by worktree inspection
// (four probes at a time) but generic.
export async function mapLimit<T, R>(
  items: readonly T[],
  limit: number,
  fn: (item: T) => Promise<R>,
): Promise<R[]> {
  const out = new Array<R>(items.length);
  let next = 0;
  async function worker(): Promise<void> {
    for (;;) {
      const i = next++;
      if (i >= items.length) return;
      // `items[i]` is in-bounds here, but noUncheckedIndexedAccess types it
      // `T | undefined`; the guard is unreachable for the dense arrays this
      // ever runs over. (No behavior move: a hole would be skipped, not mapped.)
      const item = items[i];
      if (item === undefined) continue;
      out[i] = await fn(item);
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
export function chmodWritableWhereOwned(root: string): void {
  const getuid = process.getuid;
  const uid = typeof getuid === 'function' ? getuid() : null;
  const walk = (dir: string, depth = 0): void => {
    if (depth > 12) return; // a worktree is not a filesystem crawl
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      let st: fs.Stats;
      try {
        st = fs.lstatSync(full);
      } catch {
        continue;
      }
      if (uid != null && st.uid !== uid) continue; // not ours — leave it alone
      // chmodSync follows links — a symlink we own can point OUTSIDE the
      // worktree, so it must be skipped before any chmod, not merely excluded
      // from recursion below.
      if (entry.isSymbolicLink()) continue;
      try {
        fs.chmodSync(full, st.mode | 0o200);
      } catch {
        /* best effort */
      }
      if (entry.isDirectory()) walk(full, depth + 1);
    }
  };
  try {
    walk(root);
  } catch {
    /* best effort: the retry will tell the truth */
  }
}

// What actually stands in the way, named. A path we cannot unlink is one whose
// PARENT we cannot write to (that is what unlink(2) checks) — reporting the
// child alone would send the human chasing the wrong file.
export function blockedPaths(root: string, limit = 8): { path: string; owner: string }[] {
  const getuid = process.getuid;
  const uid = typeof getuid === 'function' ? getuid() : null;
  const owners = new Map<number, string>();
  const out: { path: string; owner: string }[] = [];
  const ownerOf = (st: fs.Stats): string => {
    const cached = owners.get(st.uid);
    if (cached !== undefined) return cached;
    let name = `uid ${st.uid}`;
    try {
      name = st.uid === 0 ? 'root' : os.userInfo().uid === st.uid ? os.userInfo().username : name;
    } catch {
      /* keep uid */
    }
    owners.set(st.uid, name);
    return name;
  };
  const walk = (dir: string, depth = 0): void => {
    if (out.length >= limit || depth > 12) return;
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (out.length >= limit) return;
      const full = path.join(dir, entry.name);
      let st: fs.Stats;
      try {
        st = fs.lstatSync(full);
      } catch {
        continue;
      }
      if (uid != null && st.uid !== uid) {
        out.push({ path: full, owner: ownerOf(st) });
        continue; // do not descend into someone else's tree
      }
      if (entry.isDirectory() && !entry.isSymbolicLink()) walk(full, depth + 1);
    }
  };
  try {
    walk(root);
  } catch {
    /* nothing to add */
  }
  return out;
}

export const shellQuote = (s: string): string =>
  /^[A-Za-z0-9_@%+=:,./-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;

// A live pid check — the agents-cli registry can outlive the process.
export function pidAlive(pid: number): boolean {
  if (!Number.isFinite(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

// Process start time in ms since the epoch, or null when unverifiable. A bare
// kill(pid, 0) proves only that SOME process holds the numeric pid — after a
// Claude session exits, the OS can hand that pid to an unrelated process and
// the stale registry record would still look alive (the phantom-agent scar).
// The agents-cli registry reports startedAt, so compare it against the real
// process start: a pid that has been reused reports a fresh start that cannot
// match the record's (older) timestamp.
//
// Linux implementation: /proc/<pid>/stat field 22 (starttime, clock ticks
// since boot) converted against live uptime instead of a wall-clock boot
// time. Field 22 is parsed from the LAST ')' in the line so a comm
// containing spaces or parens (field 2, wrapped in parens) cannot shift the
// column positions. Clock ticks are 100/s on effectively every Linux
// (USER_HZ); a non-100 clock only ever inflates the apparent age, which the
// tolerance below absorbs.
//
// Why uptime and not /proc/stat btime: btime is the REAL clock at boot
// plus every subsequent clock correction (NTP steps, VM suspend/resume) —
// on exactly the long-lived WSL/VM fleets this daemon runs on it can be off
// by minutes to hours, which would false-negative every legitimate record.
// starttime and /proc/uptime tick off the SAME monotonic jiffies counter,
// so now − (uptime − startTicks) is immune to wall-clock steps entirely.
// On any platform without /proc (or any read/parse failure) this returns
// null and callers MUST treat ownership as unverifiable — never silently
// fall back to the pid-existence check that caused the bug.
export function processStartMs(pid: number): number | null {
  if (!Number.isFinite(pid) || pid <= 0) return null;
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    const after = stat.slice(stat.lastIndexOf(')') + 2);
    const startTicks = Number(after.split(' ')[19]);
    const uptimeSeconds = Number(fs.readFileSync('/proc/uptime', 'utf8').split(' ')[0]);
    if (!Number.isFinite(startTicks) || !Number.isFinite(uptimeSeconds)) return null;
    return Date.now() - (uptimeSeconds - startTicks / 100) * 1000;
  } catch {
    return null; // dead pid, unparseable /proc, or no /proc at all
  }
}

// ONE ownership verifier for agents-cli records (BUG-106): live pid AND a
// process start that matches the record's startedAt within tolerance. The
// agents registry's startedAt comes from a different clock read than the
// kernel's, so an exact match would flake — 15 s is far wider than any
// observed skew and far narrower than a realistic pid-reuse window. A record
// without a usable startedAt, or a pid whose start cannot be read (dead,
// unreadable, non-Linux), is treated as NOT OWNED — an unverifiable record is
// never allowed to create, update, or revive a card, nor to keep the poller
// in its active cadence.
export const PID_START_TOLERANCE_MS = 15_000;

export function pidOwnedBy(pid: number, startedAt: number): boolean {
  if (!Number.isFinite(startedAt) || startedAt <= 0) return false;
  const startMs = processStartMs(pid);
  if (startMs == null) return false;
  return Math.abs(startMs - startedAt) <= PID_START_TOLERANCE_MS;
}

// The four board columns colFromAgentState can assign a card.
type AgentColumn = 'working' | 'needsyou' | 'idle' | 'queued';

export function colFromAgentState(raw: string | null | undefined, isNew: boolean): AgentColumn {
  const s = (raw ?? '').toLowerCase();
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

// The parsed shape of an operator command line — a discriminated union on
// `cmd`. `ticket`/`name` each have a success arm and an `error` arm (a
// malformed callsign command must surface a usage line, never a silent note).
type ParsedCommand =
  | { cmd: 'broadcast'; text: string }
  | { cmd: 'assign_auto'; repo: string | null; text: string }
  | { cmd: 'assign'; target: string; text: string }
  | { cmd: 'ticket'; target: string; ticket: string }
  | { cmd: 'ticket'; error: string }
  | { cmd: 'name'; target: string; suffix: string }
  | { cmd: 'name'; error: string }
  | { cmd: 'note'; text: string };

export function parseCommand(text: unknown): ParsedCommand {
  // `text` is a raw HTTP-body field (core.command(ev.text)); coerce defensively
  // exactly as command() does. The degenerate object → "[object Object]" path is
  // intentional garbage-in handling, so no-base-to-string is deliberately off
  // here — drop this and take `text: string` once http validates the body.
  // eslint-disable-next-line @typescript-eslint/no-base-to-string -- untrusted wire value, coercion is intentional
  const t = String(text ?? '').trim();
  let m: RegExpExecArray | null;
  if ((m = /^broadcast\s+(.+)$/is.exec(t))) return { cmd: 'broadcast', text: (m[1] ?? '').trim() };
  if ((m = /^assign\s+(\S+)\s+(.+)$/is.exec(t))) {
    const target = m[1] ?? '';
    // v1.1 auto-routing: `assign auto <text>` / `assign auto:<repo> <text>`.
    // Repo names can contain dots and dashes (and repo_ids are absolute
    // paths), so split the target on the FIRST colon only — everything
    // after it is the repo key, verbatim. Bare `auto:` degrades to
    // unscoped auto.
    if (target === 'auto' || target.startsWith('auto:')) {
      const repo = target.length > 'auto:'.length ? target.slice('auto:'.length) : null;
      return { cmd: 'assign_auto', repo, text: (m[2] ?? '').trim() };
    }
    return { cmd: 'assign', target, text: (m[2] ?? '').trim() };
  }
  // 0.6.0 ticket callsigns: `ticket <target> <PROJ-123|clear>`. Exactly two
  // tokens (the value is anchored last), so `ticket foo BAR extra` does NOT
  // match here and falls to the malformed branch below. A malformed or bare
  // `ticket …` must NEVER silently become a note — it carries an explicit error
  // the command handler surfaces loudly (an operator who fat-fingers a key
  // deserves a usage line, not a note that looks like it worked).
  if ((m = /^ticket\s+(\S+)\s+(\S+)\s*$/i.exec(t))) {
    return { cmd: 'ticket', target: m[1] ?? '', ticket: m[2] ?? '' };
  }
  if (/^ticket\b/i.test(t)) {
    return { cmd: 'ticket', error: 'usage: ticket <callsign-or-session-id> <PROJ-123|clear>' };
  }
  // 0.7.1 custom names: `name <target> <suffix|clear>`. Same shape and the same
  // never-silently-a-note rule as `ticket` above — the human renames a card by
  // its ID part, the animal is never theirs to choose.
  if ((m = /^name\s+(\S+)\s+(\S+)\s*$/i.exec(t))) {
    return { cmd: 'name', target: m[1] ?? '', suffix: m[2] ?? '' };
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

export function validateNameSuffix(suffix: string): string | null {
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
