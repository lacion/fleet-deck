// tests/takeover.test.mjs
//
// Version takeover: the NEWEST installed plugin version must always end up
// owning the daemon on port 4711. The SessionStart hook detects a stale-version
// daemon via /health, SIGTERMs it (the daemon's tested graceful shutdown),
// waits for its death, and spawns its own newer build onto the freed port. The
// contract lives in scripts/fleetd/takeover.mjs and is wired into
// scripts/fleet-sessionstart.mjs (ensureServer).
//
// Two layers of coverage:
//   1. Pure units against takeover.mjs — the semver rule and the
//      verify-before-kill gate (no processes).
//   2. Integration cases spawning the REAL hook script as a child process
//      (modelled on tests/watch-rewake.test.mjs's fleet-watch spawns), each on
//      a per-test random port + fresh FLEETDECK_HOME. Every daemon/stub the
//      suite creates is reaped in t.after — this file must NEVER touch port
//      4711 or the real ~/.fleetdeck (see the daemon-leak scar in
//      tests/helpers/daemon.mjs).
//
// Determinism note: the hook's OWN version is read from the repo package.json
// (PKG_VERSION below). The DAEMON's version is pinned per-test via
// FLEETDECK_VERSION_OVERRIDE (a test-only env honoured by fleetd.mjs and the
// stub), so "older"/"newer"/"equal" are set explicitly rather than depending on
// the package.json value.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, randomPort, waitForHealth, REPO_ROOT } from './helpers/daemon.mjs';
import { getJson, postHook } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { waitUntil, scaleMs } from './helpers/wait.mjs';
import {
  parseSemver, compareSemver, shouldTakeOver, verifyDaemonPid,
} from '../scripts/fleetd/takeover.mjs';

const HOOK_SCRIPT = path.join(REPO_ROOT, 'scripts/fleet-sessionstart.mjs');
const FLEETD_SOURCE = path.join(REPO_ROOT, 'scripts/fleetd/fleetd.mjs');
const STUB = path.join(REPO_ROOT, 'tests/helpers/stub-immortal-daemon.mjs');
const PKG_VERSION = JSON.parse(readFileSync(path.join(REPO_ROOT, 'package.json'), 'utf8')).version;

function scratchDir(t) {
  const d = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(() => rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return d;
}

// Spawn the REAL SessionStart hook the way Claude Code does: a SessionStart
// payload on stdin, scratch-daemon env. FLEETDECK_TEST_DAEMON_SCRIPT pins the
// launcher to fleetd.mjs SOURCE (the committed bundle is deliberately stale
// mid-iteration); FLEETDECK_TMUX_SOCKET isolates any tmux server the spawned
// daemon might create; FLEETDECK_AGENTS_CMD=false keeps the poller off.
function runHook({ port, home, env = {}, payload }) {
  const childEnv = {
    ...process.env,
    FLEETDECK_PORT: String(port),
    FLEETDECK_HOME: home,
    FLEETDECK_AGENTS_CMD: 'false',
    FLEETDECK_TMUX_SOCKET: `fleetdeck-test-${port}`,
    FLEETDECK_TEST_DAEMON_SCRIPT: FLEETD_SOURCE,
    ...env,
  };
  // A hook run from inside the suite (itself a Claude session / tmux) must not
  // leak the outer tmux server into the daemon it launches.
  delete childEnv.TMUX;
  delete childEnv.TMUX_PANE;
  const child = spawn(process.execPath, [HOOK_SCRIPT], { env: childEnv, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', d => { stdout += d; });
  child.stderr.on('data', d => { stderr += d; });
  child.stdin.write(JSON.stringify(payload ?? { hook_event_name: 'SessionStart' }));
  child.stdin.end();
  const exited = new Promise(resolve => child.once('exit', code => resolve(code)));
  return {
    child,
    get stdout() { return stdout; },
    get stderr() { return stderr; },
    exitWithin(ms, label) {
      return Promise.race([
        exited,
        new Promise((_, reject) => setTimeout(
          () => reject(new Error(`hook did not exit within ${ms}ms (${label})`)), scaleMs(ms)).unref?.()),
      ]);
    },
  };
}

// Reap a daemon the HOOK spawned (detached — nothing else owns its lifetime).
// Find its pid via /health, else the pidfile; SIGTERM, wait, SIGKILL backstop;
// then reap the isolated tmux server for the port. Leaking a daemon here would
// reopen the exact class of bug this repo just cleaned up.
async function killDaemonAt(port, home) {
  let pid = null;
  try { pid = (await getJson(`http://127.0.0.1:${port}/health`, { timeout: 500 })).json?.pid ?? null; }
  catch { /* fall through to the pidfile */ }
  if (pid == null) {
    try { pid = JSON.parse(readFileSync(path.join(home, 'fleetd.pid'), 'utf8'))?.pid ?? null; }
    catch { /* nothing to reap */ }
  }
  if (pid != null) {
    try { process.kill(pid, 'SIGTERM'); } catch { /* already gone */ }
    for (let i = 0; i < 20; i++) {
      await new Promise(r => setTimeout(r, 100));
      try { process.kill(pid, 0); } catch { pid = null; break; }
    }
    if (pid != null) { try { process.kill(pid, 'SIGKILL'); } catch { /* gone */ } }
  }
  try { spawnSync('tmux', ['-L', `fleetdeck-test-${port}`, 'kill-server'], { stdio: 'ignore', timeout: 3000 }); }
  catch { /* the common case is no server on the socket */ }
}

// ---------------------------------------------------------------------------
// 1. Units — the semver rule and the verify-before-kill gate.
// ---------------------------------------------------------------------------

test('semver: parse, numeric compare, and the strictly-newer + 0.0.0/unparseable refusal rules', () => {
  // parseSemver: three all-digit segments; leading v and -/+ suffix tolerated.
  assert.deepEqual(parseSemver('0.6.0'), [0, 6, 0]);
  assert.deepEqual(parseSemver('v1.2.3'), [1, 2, 3]);
  assert.deepEqual(parseSemver('0.6.10-rc.1'), [0, 6, 10]);
  assert.deepEqual(parseSemver('1.0.0+build.9'), [1, 0, 0]);
  assert.equal(parseSemver('1.2'), null, 'fewer than three segments is unorderable');
  assert.equal(parseSemver('1.2.x'), null, 'a non-numeric segment is unorderable');
  assert.equal(parseSemver('latest'), null);
  assert.equal(parseSemver(''), null);
  assert.equal(parseSemver(null), null);
  assert.equal(parseSemver(undefined), null);

  // compareSemver is numeric, never lexicographic (0.6.10 > 0.6.2).
  assert.equal(compareSemver([0, 6, 10], [0, 6, 2]), 1);
  assert.equal(compareSemver([0, 6, 2], [0, 6, 10]), -1);
  assert.equal(compareSemver([1, 0, 0], [0, 9, 9]), 1);
  assert.equal(compareSemver([0, 6, 0], [0, 6, 0]), 0);

  // shouldTakeOver: strictly newer, both parse, neither is the 0.0.0 sentinel.
  assert.equal(shouldTakeOver('0.7.0', '0.6.0'), true);
  assert.equal(shouldTakeOver('0.6.10', '0.6.2'), true, 'numeric, not lexicographic');
  assert.equal(shouldTakeOver('1.0.0', '0.9.9'), true);
  assert.equal(shouldTakeOver('0.6.0', '0.7.0'), false, 'older never evicts');
  assert.equal(shouldTakeOver('0.6.0', '0.6.0'), false, 'equal never evicts');
  // 0.0.0 loop guard — either side.
  assert.equal(shouldTakeOver('0.7.0', '0.0.0'), false, 'a 0.0.0 daemon is never evicted (respawn loop guard)');
  assert.equal(shouldTakeOver('0.0.1', '0.0.0'), false);
  assert.equal(shouldTakeOver('0.0.0', '0.0.0'), false);
  // Unparseable on either side.
  assert.equal(shouldTakeOver('0.7.0', 'garbage'), false);
  assert.equal(shouldTakeOver('garbage', '0.6.0'), false);
  assert.equal(shouldTakeOver(null, '0.6.0'), false);
});

test('verifyDaemonPid refuses a non-fleetd-shaped live pid, a pidfile mismatch, and a missing pidfile', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-verify-'));
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  // A live but non-fleetd node process: its /proc cmdline carries no
  // fleetd*.mjs arg, so livePidLooksLikeFleetd (Linux) must reject it even
  // though we hand it a perfectly matching pidfile.
  const sleeper = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1e9)'], { stdio: 'ignore' });
  t.after(() => { try { sleeper.kill('SIGKILL'); } catch { /* gone */ } });
  await waitUntil(() => sleeper.pid != null, { label: 'sleeper pid' });
  writeFileSync(path.join(home, 'fleetd.pid'), JSON.stringify({ pid: sleeper.pid, port: 40000 }));

  if (process.platform === 'linux') {
    assert.equal(verifyDaemonPid(sleeper.pid, home), false,
      'a live but non-fleetd-shaped pid must be refused even when the pidfile matches');
  }

  // Pidfile pid mismatch is refused on every platform (checked before /proc).
  writeFileSync(path.join(home, 'fleetd.pid'), JSON.stringify({ pid: sleeper.pid + 100000, port: 40000 }));
  assert.equal(verifyDaemonPid(sleeper.pid, home), false, 'a pidfile pid mismatch must be refused');

  // A missing pidfile is refused.
  const emptyHome = mkdtempSync(path.join(tmpdir(), 'fleetdeck-verify-empty-'));
  t.after(() => rmSync(emptyHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  assert.equal(verifyDaemonPid(sleeper.pid, emptyHome), false, 'a missing pidfile must be refused');

  // A bad pid argument is refused.
  assert.equal(verifyDaemonPid(0, home), false);
  assert.equal(verifyDaemonPid(-1, home), false);
});

test('verifyDaemonPid accepts a genuine running daemon (pidfile match + fleetd /proc shape)', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });
  const health = (await getJson(`${daemon.baseUrl}/health`)).json;
  assert.ok(health?.pid, 'health should report a pid');
  assert.equal(verifyDaemonPid(health.pid, daemon.home), true,
    'a real fleetd must verify (its pidfile matches and its /proc shape is node fleetd.mjs)');
});

// ---------------------------------------------------------------------------
// 2. Integration — the real hook against real (and immortal) daemons.
// ---------------------------------------------------------------------------

test('a cold-boot SessionStart hook rereads the minted token and registers the first session', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cold-hook-home-'));
  const cwd = scratchDir(t);
  const sid = randomUUID();
  t.after(async () => {
    await killDaemonAt(port, home);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const hook = runHook({
    port, home,
    payload: loadFixture('session-start', { session_id: sid, cwd }),
  });
  assert.equal(await hook.exitWithin(10000, 'cold-boot SessionStart'), 0,
    `the hook must fail open with exit 0 (stderr: ${hook.stderr})`);
  await waitForHealth(`http://127.0.0.1:${port}`, 8000);

  const state = (await getJson(`http://127.0.0.1:${port}/state`)).json;
  assert.ok(state.sessions?.some(session => session.session_id === sid),
    'the birth SessionStart must authenticate after cold boot and create the first card');
});

test('a newer hook replaces an older daemon: old exits 0, new owns the same port+HOME, a pre-seeded session survives, ticker says "replaced"', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-takeover-home-'));
  const cwd = scratchDir(t);
  t.after(async () => {
    await killDaemonAt(port, home);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // OLD daemon from SOURCE, pinned to 0.0.1 (strictly older than PKG_VERSION).
  const old = await startDaemon({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1' } });
  const oldPid = (await getJson(`${old.baseUrl}/health`)).json.pid;

  // Pre-seed a session so we can prove state survives the SQLite handoff.
  const seededSid = randomUUID();
  await postHook(old.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: seededSid, cwd }), { token: old });
  assert.ok(
    (await getJson(`${old.baseUrl}/state`)).json.sessions.find(s => s.session_id === seededSid),
    'sanity: the seed session is present before the takeover');

  // Run the real hook. own = PKG_VERSION > 0.0.1 → commit to the takeover.
  const hook = runHook({ port, home, payload: loadFixture('session-start', { session_id: randomUUID(), cwd }) });
  const code = await hook.exitWithin(14000, 'newer-hook takeover');
  assert.equal(code, 0, `the hook must always exit 0 (stderr: ${hook.stderr})`);

  // The displaced daemon exits 0 (graceful shutdown, not the hard-exit watchdog).
  await waitUntil(() => old.proc.exitCode !== null, { timeoutMs: 5000, label: 'old daemon exit' });
  assert.equal(old.proc.exitCode, 0, 'the displaced daemon must exit 0 via its graceful SIGTERM shutdown');

  // The replacement is healthy on the SAME port, reports the NEWER version, is
  // a DIFFERENT process.
  const health = await waitForHealth(`http://127.0.0.1:${port}`, 8000);
  assert.equal(health.version, PKG_VERSION, 'the replacement reports the newer (package.json) version');
  assert.notEqual(health.pid, oldPid, 'the replacement is a different process');

  // State survived the handoff (same FLEETDECK_HOME, SQLite/WAL).
  const state = (await getJson(`http://127.0.0.1:${port}/state`)).json;
  assert.ok(state.sessions.find(s => s.session_id === seededSid),
    'the pre-seeded session survived the version takeover');

  // The board ticker announces the takeover.
  const ticker = (state.ticker || []).map(r => r.msg).join('\n');
  assert.match(ticker, /replaced/, `the ticker must carry the "replaced" handoff line (got: ${ticker.slice(0, 200)})`);
  assert.match(ticker, new RegExp(`v${PKG_VERSION.replace(/\./g, '\\.')} replaced v0\\.0\\.1`),
    'the ticker line names both the new and the displaced version');
});

test('equal versions: the hook keeps using the running daemon — no takeover (same /health pid)', async (t) => {
  const daemon = await startDaemon(); // no override → reports PKG_VERSION
  t.after(async () => { await daemon.stop(); });
  const before = (await getJson(`${daemon.baseUrl}/health`)).json;
  assert.equal(before.version, PKG_VERSION, 'sanity: an un-overridden daemon reports the package.json version');

  const hook = runHook({
    port: daemon.port, home: daemon.home,
    payload: loadFixture('session-start', { session_id: randomUUID(), cwd: scratchDir(t) }),
  });
  assert.equal(await hook.exitWithin(10000, 'equal-version hook'), 0);

  const after = (await getJson(`${daemon.baseUrl}/health`)).json;
  assert.equal(after.pid, before.pid, 'an equal-version daemon must NOT be replaced (same pid)');
  assert.equal(after.version, PKG_VERSION);
});

test('an older hook never downgrades a newer daemon (daemon pinned to 99.0.0)', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_VERSION_OVERRIDE: '99.0.0' } });
  t.after(async () => { await daemon.stop(); });
  const before = (await getJson(`${daemon.baseUrl}/health`)).json;
  assert.equal(before.version, '99.0.0');

  const hook = runHook({
    port: daemon.port, home: daemon.home,
    payload: loadFixture('session-start', { session_id: randomUUID(), cwd: scratchDir(t) }),
  });
  assert.equal(await hook.exitWithin(10000, 'older hook'), 0);

  const after = (await getJson(`${daemon.baseUrl}/health`)).json;
  assert.equal(after.pid, before.pid, 'a newer daemon must never be downgraded by an older hook');
  assert.equal(after.version, '99.0.0', 'the newer daemon keeps serving unchanged');
});

test('a SIGTERM-immune stale daemon: the hook fails open and the stub keeps serving (no SIGKILL escalation)', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-immortal-home-'));
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  // The disguise argv 'fleetd.mjs' makes livePidLooksLikeFleetd (and thus
  // verifyDaemonPid) accept the stub, so the hook genuinely reaches the SIGTERM
  // — which the stub ignores.
  const stub = spawn(process.execPath, [STUB, 'fleetd.mjs'], {
    env: { ...process.env, FLEETDECK_PORT: String(port), FLEETDECK_HOME: home, FLEETDECK_VERSION_OVERRIDE: '0.0.1' },
    stdio: ['ignore', 'ignore', 'ignore'],
  });
  t.after(() => { try { stub.kill('SIGKILL'); } catch { /* gone */ } });
  await waitForHealth(`http://127.0.0.1:${port}`, 8000);
  const stubPid = (await getJson(`http://127.0.0.1:${port}/health`)).json.pid;
  assert.equal(stubPid, stub.pid, 'sanity: the stub reports its own pid');

  const hook = runHook({
    port, home,
    payload: loadFixture('session-start', { session_id: randomUUID(), cwd: scratchDir(t) }),
  });
  // terminateDaemon's default 2s wait-for-death runs before the hook fails open.
  assert.equal(await hook.exitWithin(12000, 'immortal-daemon fail-open'), 0,
    'the hook must still exit 0 after failing to evict a wedged daemon');

  assert.equal(stub.exitCode, null, 'the immortal stub must survive the SIGTERM (no SIGKILL escalation)');
  const after = (await getJson(`http://127.0.0.1:${port}/health`)).json;
  assert.equal(after.pid, stubPid, 'the stale stub keeps serving; nothing replaced it (fail open)');
});

test('stretch: two racing newer hooks converge on exactly one replacement daemon', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-race-home-'));
  const cwd = scratchDir(t);
  t.after(async () => {
    await killDaemonAt(port, home);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const old = await startDaemon({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1' } });
  const oldPid = (await getJson(`${old.baseUrl}/health`)).json.pid;

  // Two hooks fire simultaneously. Both SIGTERM the old daemon (the second gets
  // ESRCH), both spawn a replacement; the port-bind election (loser exits 3)
  // resolves it to exactly one survivor.
  const h1 = runHook({ port, home, payload: loadFixture('session-start', { session_id: randomUUID(), cwd }) });
  const h2 = runHook({ port, home, payload: loadFixture('session-start', { session_id: randomUUID(), cwd }) });
  const [c1, c2] = await Promise.all([
    h1.exitWithin(14000, 'race hook #1'),
    h2.exitWithin(14000, 'race hook #2'),
  ]);
  assert.equal(c1, 0, `race hook #1 must exit 0 (stderr: ${h1.stderr})`);
  assert.equal(c2, 0, `race hook #2 must exit 0 (stderr: ${h2.stderr})`);

  await waitUntil(() => old.proc.exitCode !== null, { timeoutMs: 5000, label: 'old daemon exit' });

  // Exactly one healthy, stable daemon owns the port, on the new version.
  const health = await waitForHealth(`http://127.0.0.1:${port}`, 8000);
  assert.notEqual(health.pid, oldPid, 'the survivor is a replacement, not the old daemon');
  assert.equal(health.version, PKG_VERSION);
  const again = await waitForHealth(`http://127.0.0.1:${port}`, 3000);
  assert.equal(again.pid, health.pid, 'the port is owned by a single, stable daemon (no flapping)');
});

// ---------------------------------------------------------------------------
// MANAGED DAEMONS (standalone mode). A daemon started by `fleetdeck serve` is
// owned by a supervisor — systemd, or the wrapper in a Coder workspace. The
// takeover contract above must NOT apply to it, and this is not a stylistic
// preference: if a newer plugin SIGTERMs a supervised daemon, the supervisor
// restarts it at the very moment the hook spawns its own replacement, and the
// two race for the port. Whichever loses exits 3. The service owns the port; a
// version mismatch is an operator's upgrade to make, not a hook's.

test('a MANAGED daemon is never evicted, even by a strictly newer hook', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-managed-home-'));
  const cwd = scratchDir(t);
  t.after(async () => {
    await killDaemonAt(port, home);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // A managed daemon pinned OLDER than us — precisely the case the takeover
  // contract exists to evict. FLEETDECK_MANAGED=1 is what `fleetdeck serve` sets.
  const svc = await startDaemon({
    port, home,
    env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1', FLEETDECK_MANAGED: '1' },
  });
  const before = (await getJson(`${svc.baseUrl}/health`)).json;
  assert.equal(before.managed, true, 'sanity: `fleetdeck serve` marks the daemon managed on /health');
  assert.equal(before.version, '0.0.1', 'sanity: it is strictly older than this package');

  const hook = runHook({ port, home, payload: loadFixture('session-start', { session_id: randomUUID(), cwd }) });
  assert.equal(
    await hook.exitWithin(14000, 'managed-daemon hook'), 0,
    `the hook must still exit 0 (stderr: ${hook.stderr})`,
  );

  // The service is untouched: same process, same version, still managed.
  assert.equal(svc.proc.exitCode, null, 'the managed daemon must NOT have been terminated');
  const after = (await getJson(`http://127.0.0.1:${port}/health`)).json;
  assert.equal(after.pid, before.pid, 'the managed daemon must be the SAME process — no takeover, no respawn');
  assert.equal(after.version, '0.0.1', 'the older managed daemon still owns the port');

  // ...and the drift is REPORTED, not silently swallowed. Discovering that a fix
  // you installed is not the code that is running should not cost an afternoon.
  assert.match(hook.stdout, /managed service running v0\.0\.1/,
    `the SessionStart brief must name the running service version (got: ${hook.stdout.slice(0, 300)})`);
  assert.match(hook.stdout, new RegExp(`plugin is v${PKG_VERSION.replace(/\./g, '\\.')}`),
    'the SessionStart brief must name the plugin version it could not install');
});

test('an UNMANAGED daemon of the same age is still evicted (the guard is the flag, not the version)', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-unmanaged-home-'));
  const cwd = scratchDir(t);
  t.after(async () => {
    await killDaemonAt(port, home);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // Identical to the test above in every respect EXCEPT FLEETDECK_MANAGED. If
  // this one did not evict, the test above would prove nothing.
  const old = await startDaemon({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1' } });
  const oldPid = (await getJson(`${old.baseUrl}/health`)).json.pid;

  const hook = runHook({ port, home, payload: loadFixture('session-start', { session_id: randomUUID(), cwd }) });
  assert.equal(await hook.exitWithin(14000, 'unmanaged-daemon hook'), 0);

  const health = await waitForHealth(`http://127.0.0.1:${port}`, 8000);
  assert.notEqual(health.pid, oldPid, 'an unmanaged older daemon must still be replaced');
  assert.equal(health.version, PKG_VERSION);
  assert.equal(health.managed, false, 'the hook-spawned replacement is not a managed service');
});
