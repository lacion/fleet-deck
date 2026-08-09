#!/usr/bin/env node
// fleet-sessionstart.mjs — the ONLY command hook. Election + spawn + brief +
// the dynamic FileChanged watch list (BUG-104).
//
// Reads the SessionStart hook payload on stdin, makes sure fleetd is up
// (health check → spawn detached → poll ~3 s), POSTs /hook/SessionStart and
// prints the daemon-composed roster brief to stdout (SessionStart stdout is
// added to the session context).
//
// It also owns watch registration: hooks.json's FileChanged matcher can only
// watch literal top-level filenames, so coverage of the files a session will
// actually touch has to come from hookSpecificOutput.watchPaths. stdout is
// therefore a single JSON object: the roster brief rides additionalContext and
// watchPaths registers the session's cwd — the one absolute path that covers
// every file the session may touch, including files created after startup.
// fleet-hook.mjs refreshes the same list from CwdChanged, and every delivered
// FileChanged event re-pins it.
//
// Design rule #1: this script must NEVER break the session. EVERY failure
// path is a silent exit 0, and a watchdog guarantees we are gone in ~4 s.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { CLAUDE_ENV_MARKERS, GATEWAY_ENV_VARS, SPAWN_ENV_VARS } from './fleetd/env-scrub.ts';
import { runNonce } from './fleetd/run-nonce.ts';
// Version-takeover contract, imported as SOURCE from the sibling fleetd/ dir
// (same unbundled pattern as env-scrub.mjs above) so this hook can evict a
// strictly-older daemon and let the newest installed build own the port.
import { shouldTakeOver, verifyDaemonPid, terminateDaemon, replacementMatches } from './fleetd/takeover.ts';
import { resolveHome, resolvePort, resolveBase } from './fleetd/config.ts';

const PORT = resolvePort();
const BASE = resolveBase(PORT);
const HERE = path.dirname(fileURLToPath(import.meta.url));
// Prefer the committed bundle (self-contained — git-distributed installs have
// no node_modules); fall back to source for dev checkouts mid-iteration.
const FLEETD_BUNDLE = path.join(HERE, 'fleetd', 'fleetd.bundle.mjs');
// FLEETDECK_TEST_DAEMON_SCRIPT is a test-only seam (mirrors the same env in
// tests/helpers/daemon.mjs): it pins the launcher to a specific daemon build so
// the takeover suite can boot the daemon FROM SOURCE while the committed bundle
// is deliberately left stale mid-iteration. Unset in production, which always
// prefers the bundle and falls back to source.
const FLEETD = process.env.FLEETDECK_TEST_DAEMON_SCRIPT
  || (fs.existsSync(FLEETD_BUNDLE) ? FLEETD_BUNDLE : path.join(HERE, 'fleetd', 'fleetd.mjs'));
const HOME = resolveHome();

// Every /hook/* route requires the bearer, including plain loopback. A warm
// hook reads the persisted token here; a cold hook reads null, boots fleetd,
// then MUST reread after health succeeds because that boot minted the file.
function readToken() {
  try { return fs.readFileSync(path.join(HOME, 'token'), 'utf8').trim() || null; }
  catch { return null; }
}
let TOKEN = readToken();

// Hard deadline: whatever happens, exit 0 well inside the hook's 15s timeout.
// The takeover path can outlast the default 3.8s budget (waiting for an old
// daemon to die + polling the replacement), so the watchdog is re-armable to a
// larger total via rearmWatchdog() — always measured from the hook's start and
// always well inside hooks.json's 15s ceiling.
const HOOK_START = Date.now();
let watchdog = setTimeout(() => process.exit(0), 3800);
function rearmWatchdog(totalMs) {
  clearTimeout(watchdog);
  const remaining = Math.max(0, totalMs - (Date.now() - HOOK_START));
  watchdog = setTimeout(() => process.exit(0), remaining);
}
// Set only on a takeover spawn (see ensureServer): the displaced version, which
// bootEnv folds into FLEETDECK_REPLACED for the replacement daemon's banner.
let replacedVersion = null;
// Set when a service-managed daemon is running a different version than this
// plugin. We never evict it (see ensureServer), but silent drift is how someone
// spends an afternoon debugging a fix that is installed and not running.
let managedVersionDrift = null;

async function readStdin() {
  let data = '';
  try {
    for await (const chunk of process.stdin) data += chunk;
    return JSON.parse(data || '{}');
  } catch { return {}; }
}

async function api(pathname, { method = 'GET', body, timeout = 400 } = {}) {
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), timeout);
  try {
    const headers = {};
    if (body) headers['content-type'] = 'application/json';
    if (TOKEN) headers.authorization = `Bearer ${TOKEN}`;
    const res = await fetch(BASE + pathname, {
      method,
      headers: Object.keys(headers).length ? headers : undefined,
      body: body ? JSON.stringify(body) : undefined,
      signal: ctl.signal,
    });
    return await res.json();
  } catch { return null; }
  finally { clearTimeout(t); }
}

// The daemon's env seeds any tmux SERVER it creates: tmux bakes the FIRST
// client's environment into the server's global env, which every later pane
// inherits (the 2026-07-11 ghost-daemon scar — a test-run daemon poisoned the
// default server with a test FLEETDECK_PORT/HOME). This hook runs INSIDE a
// Claude session, so scrub the session markers before boot. Deliberately does
// NOT scrub FLEETDECK_* tuning knobs — tests/demos pass those through here on
// purpose. The shared Claude/agent marker list lives in fleetd/env-scrub.mjs
// (imported by the spawn() scrub in fleetd/helpers.mjs too, so the two can
// never drift); TMUX/TMUX_PANE are this hook's own context-specific additions.
function bootEnv() {
  const env = { ...process.env, FLEETDECK_PORT: String(PORT), FLEETDECK_HOME: HOME };
  for (const k of [
    ...CLAUDE_ENV_MARKERS, 'TMUX', 'TMUX_PANE',
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
    'FLEETDECK_TEST_DAEMON_SCRIPT', 'FLEETDECK_VERSION_OVERRIDE',
    // The managed bit belongs to `fleetdeck serve` ALONE: a hook that happens
    // to run inside a session whose daemon IS managed would otherwise stamp
    // its unmanaged replacement as a service, and the NEXT hook would then
    // refuse to evict it (the managed no-evict guard) — a daemon nothing
    // supervises, immune to takeover forever.
    'FLEETDECK_MANAGED',
  ]) delete env[k];
  // Upgrade takeover: ONLY the spawn that just evicted an older daemon carries
  // the displaced version, so the replacement logs the handoff and emits the
  // "replaced" ticker line exactly once. A cold first boot (no daemon was
  // running) leaves it unset; the explicit delete also stops an inherited
  // FLEETDECK_REPLACED from leaking a bogus banner onto an unrelated cold boot.
  if (replacedVersion) env.FLEETDECK_REPLACED = replacedVersion;
  else delete env.FLEETDECK_REPLACED;
  return env;
}

// Our OWN plugin version, read from the package.json one level up from this
// hook (scripts/../package.json). CLAUDE_PLUGIN_ROOT always points Claude at
// the NEWEST installed plugin version's cache dir, so this is the version that
// should own the daemon. null on ANY failure: with no version of our own we
// cannot claim to be newer than anyone, so takeover is skipped and the hook
// behaves exactly as before (fail open onto whatever daemon is running).
function ownVersion() {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(HERE, '..', 'package.json'), 'utf8'));
    const v = typeof pkg?.version === 'string' ? pkg.version.trim() : '';
    return v || null;
  } catch { return null; }
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
async function ensureServer(round = 0) {
  const health = await api('/health', { timeout: 250 });
  if (health) {
    // A daemon is already up. ownVersion() is read only on this branch — the
    // cold-boot path below never compares versions, so it skips the
    // package.json read entirely. String-equality shortcut before ANY semver
    // work: identical versions can never be a takeover candidate, and
    // own==null (our package.json was unreadable) means we can't claim to be
    // newer — both keep the overwhelmingly-common path a single /health
    // round-trip, as before.
    const own = ownVersion();
    if (own == null || health.version === own) return true;
    // MANAGED DAEMON: started by `fleetdeck serve` under a supervisor, so it is
    // not ours to kill. Evicting it would start a fight we cannot win — we
    // SIGTERM it and spawn a replacement while the supervisor ALSO restarts it,
    // and whichever loses the port bind exits 3. The service owns the port; a
    // version mismatch here is an operator's upgrade to make, so we fail open
    // onto it and say so rather than papering over the drift.
    if (health.managed) {
      managedVersionDrift = health.version;
      return true;
    }
    // Version differs. Take over ONLY when strictly newer AND we can positively
    // identify the /health pid as our daemon (pidfile match + /proc shape). Any
    // doubt on either check → keep using the running daemon.
    if (!shouldTakeOver(own, health.version) || !verifyDaemonPid(health.pid, HOME)) return true;
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
  let out = null;
  try {
    fs.mkdirSync(HOME, { recursive: true });
    const logFile = path.join(HOME, 'fleetd.log');
    // WHY mode is not enough: open(append) preserves an existing file's old
    // permissions. chmod repairs logs created by older versions before this
    // hook gives a daemon another chance to write credentials into them.
    out = fs.openSync(logFile, 'a', 0o600);
    fs.chmodSync(logFile, 0o600);
    const child = spawn(process.execPath, ['--no-warnings=ExperimentalWarning', FLEETD], {
      detached: true,
      stdio: ['ignore', out, out],
      env: bootEnv(),
    });
    // spawn() reports resource exhaustion and similar launch failures on the
    // next turn. Without a listener that 'error' would violate this hook's
    // foundational promise to fail silently instead of breaking SessionStart.
    child.once('error', () => {});
    child.unref();
  } catch { return false; }
  finally {
    // The detached child owns duplicated descriptors after a successful
    // spawn; the launcher must release its copy on every success/failure path.
    if (out !== null) try { fs.closeSync(out); } catch { /* silent hook */ }
  }
  for (let i = 0; i < 12; i++) {
    await new Promise(r => setTimeout(r, 250));
    const spawned = await api('/health', { timeout: 250 });
    if (spawned) {
      // A daemon answers, but whose? Only a hook that just evicted an older
      // build can be RACING another candidate doing the same — a cold-boot
      // competitor simply fails to bind and this poll never sees it (its
      // winner, whoever spawned it, reports this hook's own version anyway).
      // If the winner is not our build, do NOT accept its code as the result
      // of our upgrade: fail open onto it for this session and re-arbitrate.
      // Round 2's /health then applies the normal strictly-newer rule — we
      // evict it when we are newer, keep it when we are not — so a rapid
      // multi-version upgrade converges on the NEWEST build instead of
      // whichever candidate happened to claim the port first. Skipping the
      // check on the cold-boot path also keeps ownVersion() (a package.json
      // read) off that path, as documented in ensureServer.
      if (replacedVersion && !replacementMatches(ownVersion(), spawned.version)) {
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
  payload.hook_event_name = payload.hook_event_name || 'SessionStart';
  // Run generation (BUG-025): mint/read THIS CLI process's run nonce and stamp
  // it on the registration. SessionStart is usually the process's first hook,
  // so this hook owns the mint; fleet-hook.mjs (same run-<ppid> dotfile, mint
  // only when absent) tags the process's later events — SessionEnd included —
  // with the same nonce. The daemon refuses to tombstone a card on a
  // SessionEnd whose nonce is not the active run, which is what stops a
  // delayed async SessionEnd from the PREVIOUS `claude --resume` process
  // (same session id) from killing the live one. Every failure path leaves the
  // payload untagged (the daemon's historical behavior) — never break the
  // session.
  try {
    // Keyed on the CLI process (CLAUDE_PID), not this shim's parent — see
    // run-nonce.mjs. A ppid key gave every hook its own nonce, so the run this
    // registers could never be matched by the SessionEnd that followed.
    if (payload.fleet_run == null) {
      const run = runNonce(HOME);
      if (run) payload.fleet_run = run;
    }
  } catch { /* untagged registration still registers */ }
  const serverUp = await ensureServer();
  // 0.16.0: if THIS hook evicted an older daemon, say so on the registration —
  // the daemon answers with upgrade lines (which other sessions still run
  // pre-upgrade hooks) so the human hears it from the session that caused it.
  if (replacedVersion) payload.fleet_takeover = replacedVersion;
  // BUG-104: the FileChanged hook is dead weight without a watch list — a
  // matcher-less FileChanged registers nothing, and matchers can only name
  // literal top-level files. SessionStart's hookSpecificOutput.watchPaths is
  // the dynamic registration channel: hand the harness the session's cwd (the
  // one absolute path covering everything the session may touch, including
  // files created after startup). The brief and the warnings move into
  // additionalContext — SessionStart accepts BOTH plain stdout and JSON, but
  // only the JSON form carries watchPaths.
  const watchPaths = [typeof payload.cwd === 'string' && payload.cwd ? payload.cwd : process.cwd()];
  const context = [];
  if (serverUp) {
    // Cold boot: ensureServer() just minted HOME/token. Refresh before the first
    // authenticated registration instead of silently losing the birth event.
    TOKEN = readToken();
    const reg = await api('/hook/SessionStart', { method: 'POST', body: payload, timeout: 1200 });
    if (managedVersionDrift) {
      context.push(
        `[FLEETDECK] The fleet daemon is a managed service running v${managedVersionDrift}, `
        + `but this plugin is v${ownVersion()}. The service owns the port and was left running. `
        + `Restart it to pick up the new version.\n`,
      );
    }
    if (Array.isArray(reg?.upgrade_lines)) {
      for (const line of reg.upgrade_lines) context.push(`${line}\n`);
    }
    if (reg?.brief) context.push(reg.brief);
  }
  const hookSpecificOutput = { hookEventName: 'SessionStart', watchPaths };
  if (context.length) hookSpecificOutput.additionalContext = context.join('');
  process.stdout.write(JSON.stringify({ hookSpecificOutput }));
} catch { /* no fleet, no drama */ }
clearTimeout(watchdog);
process.exit(0);
