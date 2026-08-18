#!/usr/bin/env bun
// fleet-sessionstart.ts — dedicated SessionStart hook. Election + spawn +
// registration + brief; the other events use the smaller fleet-hook shim.
//
// Reads the SessionStart hook payload on stdin, makes sure fleetd is up
// (health check → spawn detached → poll ~3 s), POSTs /hook/SessionStart and
// prints the daemon-composed roster brief to stdout (SessionStart stdout is
// added to the session context).
//
// FileChanged is deliberately not registered. This hook also emits no dynamic
// watchPaths: registering the session cwd makes the harness recurse through .git,
// node_modules, and every other subtree, which can wedge startup in large repos.
//
// Design rule #1: this script must NEVER break the session. EVERY failure
// path is a silent exit 0. Cold starts normally have a ~4 s ceiling; a
// positively identified version takeover may extend that ceiling to 8 s.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { CLAUDE_ENV_MARKERS, GATEWAY_ENV_VARS, SPAWN_ENV_VARS } from '../src/daemon/env-scrub.ts';
import { runNonce } from '../src/daemon/run-nonce.ts';
// Version-takeover contract. The release bundle inlines these sibling daemon
// seams, so an installed hook never depends on unpublished TypeScript files.
import {
  shouldTakeOver,
  verifyDaemonPid,
  terminateDaemon,
  replacementMatches,
} from '../src/daemon/takeover.ts';
import { resolveHome, resolvePort, resolveBase, readToken } from '../src/daemon/config.ts';
import { establishClaudeCompatibility } from './claude-compat.ts';
import { trustedRosterBrief } from './hook-output.ts';

let runtime: { port: number; base: string; home: string } | null = null;
try {
  const home = resolveHome();
  // Fleet Deck never constrains Claude Code itself. An unrecognized runtime
  // silently disables this optional integration for the life of the process.
  if (!(await establishClaudeCompatibility(home))) process.exit(0);
  const port = resolvePort();
  runtime = { port, base: resolveBase(port), home };
} catch {
  // A typo in FLEETDECK_PORT must not turn SessionStart into a Claude startup
  // failure. The daemon/doctor report the configuration error; the hook is an
  // optional integration and exits silently.
  process.exit(0);
}
if (!runtime) process.exit(0);
const PORT = runtime.port;
const BASE = runtime.base;
const HERE = path.dirname(fileURLToPath(import.meta.url));
// Prefer the committed bundle (self-contained — git-distributed installs have
// no node_modules); fall back to source for dev checkouts mid-iteration.
const FLEETD_BUNDLE = path.join(HERE, '..', 'src', 'daemon', 'fleetd.bundle.mjs');
// FLEETDECK_TEST_DAEMON_SCRIPT is a test-only seam (mirrors the same env in
// tests/helpers/daemon.ts): it pins the launcher to a specific daemon build so
// the takeover suite can boot the daemon FROM SOURCE while the committed bundle
// is deliberately left stale mid-iteration. Unset in production, which always
// prefers the bundle and falls back to source.
const FLEETD =
  // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty/unset test seam must fall back to the bundle, not be preserved as ''
  process.env['FLEETDECK_TEST_DAEMON_SCRIPT'] ||
  (fs.existsSync(FLEETD_BUNDLE)
    ? FLEETD_BUNDLE
    : path.join(HERE, '..', 'src', 'daemon', 'fleetd.ts'));
const HOME = runtime.home;

// Shapes of the daemon's wire JSON. Trusted like every other /health consumer
// (bin/fleetdeck.ts): the daemon has minted these fields at every boot, and any
// mismatch reproduces the pre-migration untyped behavior rather than inventing a
// new failure mode.
interface Health {
  version: string;
  managed: boolean;
  pid: number;
}

function asHealth(value: unknown): Health | null {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
  const candidate = value as Partial<Health>;
  if (
    typeof candidate.version !== 'string' ||
    !candidate.version ||
    typeof candidate.managed !== 'boolean' ||
    typeof candidate.pid !== 'number' ||
    !Number.isSafeInteger(candidate.pid) ||
    candidate.pid < 1
  )
    return null;
  return candidate as Health;
}
interface Registration {
  ok?: unknown;
  callsign?: unknown;
  brief?: unknown;
  upgrade_lines?: unknown;
}
// Only the fields this hook reads or stamps are named; JSON.stringify still
// serializes every runtime key the CLI sent, so the daemon receives the full
// payload regardless of what is typed here.
interface Payload {
  hook_event_name?: string;
  fleet_run?: unknown;
  fleet_takeover?: unknown;
  cwd?: unknown;
}

// Every /hook/* route requires the bearer, including plain loopback. A warm
// hook reads the persisted token here; a cold hook reads null, boots fleetd,
// then MUST reread after health succeeds because that boot minted the file.
let TOKEN = readToken(HOME);

// Hard deadline: whatever happens, exit 0 well inside the hook's 15s timeout.
// The takeover path can outlast the default 3.8s budget (waiting for an old
// daemon to die + polling the replacement), so the watchdog is re-armable to a
// larger total via rearmWatchdog() — always measured from the hook's start and
// always well inside hooks.json's 15s ceiling.
const HOOK_START = Date.now();
let watchdog = setTimeout(() => process.exit(0), 3800);
function rearmWatchdog(totalMs: number): void {
  clearTimeout(watchdog);
  const remaining = Math.max(0, totalMs - (Date.now() - HOOK_START));
  watchdog = setTimeout(() => process.exit(0), remaining);
}
// Set only on a takeover spawn (see ensureServer): the displaced version, which
// bootEnv folds into FLEETDECK_REPLACED for the replacement daemon's banner.
// The `as string | null` widens past the `null` initializer on purpose:
// ensureServer() assigns this across a closure boundary that TS flow analysis
// can't see, so a plain `= null` narrows every later read to `null` (making the
// real takeover guards below dead code to the type checker).
let replacedVersion = null as string | null;
async function readStdin(): Promise<Payload> {
  const maxBytes = 1024 * 1024;
  let data = '';
  let bytes = 0;
  try {
    for await (const chunk of process.stdin) {
      if (bytes >= maxBytes) continue;
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk));
      const kept = buf.subarray(0, maxBytes - bytes);
      data += kept.toString('utf8');
      bytes += kept.length;
    }
    return JSON.parse(data || '{}') as Payload;
  } catch {
    return {};
  }
}

async function api<T = unknown>(
  pathname: string,
  opts: { method?: string; body?: unknown; timeout?: number } = {},
): Promise<T | null> {
  const { method = 'GET', body, timeout = 400 } = opts;
  const ctl = new AbortController();
  const t = setTimeout(() => {
    ctl.abort();
  }, timeout);
  try {
    const headers: { 'content-type'?: string; authorization?: string } = {};
    if (body) headers['content-type'] = 'application/json';
    if (TOKEN) headers.authorization = `Bearer ${TOKEN}`;
    const res = await fetch(BASE + pathname, {
      method,
      headers,
      signal: ctl.signal,
      // exactOptionalPropertyTypes forbids `body: undefined`; spread it in only
      // when present (an absent body is identical on the wire to undefined).
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    if (!res.ok) return null;
    return (await res.json()) as T;
  } catch {
    return null;
  } finally {
    clearTimeout(t);
  }
}

// The daemon's env seeds any tmux SERVER it creates: tmux bakes the FIRST
// client's environment into the server's global env, which every later pane
// inherits (the 2026-07-11 ghost-daemon scar — a test-run daemon poisoned the
// default server with a test FLEETDECK_PORT/HOME). This hook runs INSIDE a
// Claude session, so scrub the session markers before boot. Deliberately does
// NOT scrub FLEETDECK_* tuning knobs — tests/demos pass those through here on
// purpose. The shared Claude/agent marker list lives in fleetd/env-scrub.ts
// (imported by the spawn() scrub in fleetd/helpers.ts too, so the two can
// never drift); TMUX/TMUX_PANE are this hook's own context-specific additions.
function bootEnv(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    FLEETDECK_PORT: String(PORT),
    FLEETDECK_HOME: HOME,
  };
  // eslint-disable-next-line @typescript-eslint/no-dynamic-delete -- scrubbing a dynamic key list; delete (not `= undefined`) guarantees the key is ABSENT from the child's env, not passed as an empty string
  for (const k of [
    ...CLAUDE_ENV_MARKERS,
    'TMUX',
    'TMUX_PANE',
    // LLM-gateway routing: the daemon never calls an Anthropic API itself, so it
    // has no use for these — but its environment SEEDS any tmux server it
    // creates, and that server's global env reaches every pane. Dropping them
    // here keeps an ambient gateway out of the server in the first place; the
    // pane-level `env -u` in claudeEnvArgvPrefix is the load-bearing guarantee,
    // this is the belt to its braces.
    ...GATEWAY_ENV_VARS,
    ...SPAWN_ENV_VARS,
    // Test seams stop HERE: the hook itself may honor them (that's what tests
    // drive), but they must never ride the daemon's env into a tmux server's
    // global env and come back through a pane's SessionStart (the 2026-07-11
    // scar). A leaked TEST_DAEMON_SCRIPT would hijack every future daemon
    // spawn; a leaked VERSION_OVERRIDE would permanently skew takeover.
    'FLEETDECK_TEST_DAEMON_SCRIPT',
    'FLEETDECK_TEST_CLAUDE_VERSION',
    'FLEETDECK_VERSION_OVERRIDE',
    // The managed bit belongs to `fleetdeck serve` ALONE: a hook that happens
    // to run inside a session whose daemon IS managed would otherwise stamp
    // its unmanaged replacement as a service, and the NEXT hook would then
    // refuse to evict it (the managed no-evict guard) — a daemon nothing
    // supervises, immune to takeover forever.
    'FLEETDECK_MANAGED',
  ])
    delete env[k];
  // Upgrade takeover: ONLY the spawn that just evicted an older daemon carries
  // the displaced version, so the replacement logs the handoff and emits the
  // "replaced" ticker line exactly once. A cold first boot (no daemon was
  // running) leaves it unset; the explicit delete also stops an inherited
  // FLEETDECK_REPLACED from leaking a bogus banner onto an unrelated cold boot.
  if (replacedVersion) env['FLEETDECK_REPLACED'] = replacedVersion;
  else delete env['FLEETDECK_REPLACED'];
  return env;
}

// Our OWN plugin version, read from the package.json one level up from this
// hook (scripts/../package.json). CLAUDE_PLUGIN_ROOT always points Claude at
// the NEWEST installed plugin version's cache dir, so this is the version that
// should own the daemon. null on ANY failure: with no version of our own we
// cannot claim to be newer than anyone, so takeover is skipped and the hook
// behaves exactly as before (fail open onto whatever daemon is running).
function ownVersion(): string | null {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(HERE, '..', 'package.json'), 'utf8')) as {
      version?: unknown;
    } | null;
    const rawV: unknown = pkg?.version;
    const v = typeof rawV === 'string' ? rawV.trim() : '';
    return v || null;
  } catch {
    return null;
  }
}

// Election + version takeover: whoever gets here first launches fleetd. The
// port bind is the lock — a concurrent launcher's daemon exits 3 on EADDRINUSE
// and we just poll. NEW in the takeover era: if a HEALTHY daemon is already
// serving but running a strictly-OLDER build than ours, SIGTERM it (after
// proving it really is our daemon), wait for it to die, and spawn our newer
// build onto the freed port. Every uncertain branch fails open — a stale
// daemon still serving beats a broken session.
// `round` counts re-entries after a competing takeover candidate's build won
// the port election: the loop re-arbitrates against the daemon now serving,
// and hard-caps so two evenly-matched candidates can never re-take over each
// other past the hook's watchdog.
async function ensureServer(round = 0): Promise<boolean> {
  const health = asHealth(await api('/health', { timeout: 250 }));
  if (health) {
    // A same-version JSON shape on loopback is not identity. Before accepting
    // this responder for registration, takeover, or model-visible output,
    // prove that HOME's ownership record names this exact pid AND this exact
    // port and that the live process still has FleetDeck's daemon shape.
    if (!verifyDaemonPid(health.pid, HOME, PORT)) return false;
    // A daemon is already up. String-equality shortcut before ANY semver work:
    // identical versions can never be a takeover candidate, and own==null
    // (our package.json was unreadable) means we can't claim to be newer.
    const own = ownVersion();
    if (own == null || health.version === own) return true;
    // MANAGED DAEMON: started by `fleetdeck serve` under a supervisor, so it is
    // not ours to kill. Evicting it would start a fight we cannot win — we
    // SIGTERM it and spawn a replacement while the supervisor ALSO restarts it,
    // and whichever loses the port bind exits 3. The service owns the port; a
    // version mismatch here is an operator's upgrade to make, so we fail open
    // onto it and say so rather than papering over the drift.
    if (health.managed) {
      return true;
    }
    // Version differs. Take over ONLY when strictly newer AND we can positively
    // identify the /health pid as our daemon (pidfile match + /proc shape). Any
    // doubt on either check → keep using the running daemon.
    if (!shouldTakeOver(own, health.version)) return true;
    // Committed takeover. This branch alone can take ~6.5s (up to 2s for the old
    // daemon to die, then ~3s polling the replacement), so extend the hook's
    // self-watchdog to 8s from its start; hooks.json's 15s ceiling is untouched.
    rearmWatchdog(8000);
    // SIGTERM + wait-for-death. A wedged daemon that ignores the signal is left
    // serving (NO SIGKILL escalation) — return true to fail open onto it; the
    // next SessionStart retries the takeover.
    if (!(await terminateDaemon(health.pid))) return true;
    // The old daemon is gone; port + pidfile are released. Tag the boot env so
    // the replacement logs the handoff and emits the "replaced" ticker line.
    replacedVersion = health.version;
  }
  let out: number | null = null;
  try {
    fs.mkdirSync(HOME, { recursive: true });
    const logFile = path.join(HOME, 'fleetd.log');
    // WHY mode is not enough: open(append) preserves an existing file's old
    // permissions. chmod repairs logs created by older versions before this
    // hook gives a daemon another chance to write credentials into them.
    out = fs.openSync(logFile, 'a', 0o600);
    fs.chmodSync(logFile, 0o600);
    const child = spawn(process.execPath, [FLEETD], {
      detached: true,
      stdio: ['ignore', out, out],
      env: bootEnv(),
    });
    // spawn() reports resource exhaustion and similar launch failures on the
    // next turn. Without a listener that 'error' would violate this hook's
    // foundational promise to fail silently instead of breaking SessionStart.
    child.once('error', () => {
      /* silent hook: launch failures surface via the health poll */
    });
    child.unref();
  } catch {
    return false;
  } finally {
    // The detached child owns duplicated descriptors after a successful
    // spawn; the launcher must release its copy on every success/failure path.
    if (out !== null)
      try {
        fs.closeSync(out);
      } catch {
        /* silent hook */
      }
  }
  for (let i = 0; i < 12; i++) {
    await new Promise<void>((r) => {
      setTimeout(r, 250);
    });
    const spawned = asHealth(await api('/health', { timeout: 250 }));
    if (spawned) {
      // The process that won the port election must also own THIS HOME at THIS
      // port. A foreign/stale responder is never a daemon we register with,
      // recurse against, or expose output from.
      if (!verifyDaemonPid(spawned.pid, HOME, PORT)) return false;
      // A daemon answers, but whose? Every spawn is an election, including a
      // genuinely cold port: hooks from two installed plugin versions can both
      // observe no daemon, spawn different builds, and then poll whichever one
      // bound first. Therefore every post-spawn health result must exactly
      // match THIS hook before it can be accepted. On mismatch, re-enter the
      // normal arbitration path: a newer hook evicts an older winner, an older
      // hook keeps a newer winner, and managed/unknown candidates retain their
      // existing fail-open rules.
      if (!replacementMatches(ownVersion(), spawned.version)) {
        // The recursion cap is the anti-flap guarantee: if the competitor
        // somehow evicts us back (only possible with two equal "newest"
        // builds fighting), a third round is out of budget — return true and
        // serve the session on whatever healthy daemon answered.
        if (round >= 1) return true;
        // A second takeover may outlast this round's remaining budget the
        // same way the first did — re-extend from the hook's start, still
        // well inside hooks.json's 15s ceiling.
        rearmWatchdog(8000);
        return ensureServer(round + 1);
      }
      return true;
    }
  }
  return false;
}

try {
  const payload = await readStdin();
  // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- a missing OR empty event name must default to 'SessionStart', not be preserved as ''
  payload.hook_event_name = payload.hook_event_name || 'SessionStart';
  // Run generation (BUG-025): mint/read THIS CLI process's run nonce and stamp
  // it on the registration. SessionStart is usually the process's first hook,
  // so this hook owns the mint; fleet-hook.ts (same launcher-derived
  // run-<claude-pid> dotfile, mint only when absent) tags the process's later
  // events — SessionEnd included — with the same nonce. The daemon refuses to tombstone a card on a
  // SessionEnd whose nonce is not the active run, which is what stops a
  // delayed async SessionEnd from the PREVIOUS `claude --resume` process
  // (same session id) from killing the live one. Every failure path leaves the
  // payload untagged (the daemon's historical behavior) — never break the
  // session.
  try {
    // Keyed on the CLI process (the launcher-derived CLAUDE_PID), not this Bun
    // shim's parent — see run-nonce.ts. A shim ppid key gave every hook its own
    // nonce, so the run this registers could never be matched by SessionEnd.
    if (payload.fleet_run == null) {
      const run = runNonce(HOME);
      if (run) payload.fleet_run = run;
    }
  } catch {
    /* untagged registration still registers */
  }
  const serverUp = await ensureServer();
  // 0.16.0: if THIS hook evicted an older daemon, say so on the registration —
  // the daemon answers with upgrade lines (which other sessions still run
  // pre-upgrade hooks) so the human hears it from the session that caused it.
  if (replacedVersion) payload.fleet_takeover = replacedVersion;
  // BUG-104 registered the entire session cwd as a dynamic watch. Do not emit
  // watchPaths at all: losing broad external-change telemetry is safer than
  // recursively traversing a large repository before the prompt is available.
  // PostToolUse remains the safe source of write/conflict telemetry.
  const context: string[] = [];
  if (serverUp) {
    // Cold boot: ensureServer() just minted HOME/token. Refresh before the first
    // authenticated registration instead of silently losing the birth event.
    TOKEN = readToken(HOME);
    const reg = await api<Registration>('/hook/SessionStart', {
      method: 'POST',
      body: payload,
      timeout: 1200,
    });
    // Upgrade/failure diagnostics belong in fleetdeck doctor and daemon logs,
    // never in model context. Only the normal roster brief is intentional.
    const brief = trustedRosterBrief(reg);
    if (brief) context.push(brief);
  }
  // A roster can be larger than a pipe's eager-write buffer. Wait for the
  // stream callback instead of force-exiting and risking a truncated brief;
  // the hook watchdog remains armed while it flushes.
  const rendered = context.join('');
  if (rendered) {
    await new Promise<void>((resolve) => {
      process.stdout.write(rendered, () => resolve());
    });
  }
} catch {
  /* no fleet, no drama */
}
clearTimeout(watchdog);
process.exitCode = 0;
