// tests/election.test.mjs
//
// Daemon election rules. Two distinct collisions:
//   1. Cross-HOME, same port: plain "first bind wins" — the loser exits 3 on
//      EADDRINUSE and the launcher proceeds as a client. Unchanged.
//   2. Same-HOME (the takeover-replacement race, BUG-156): a second fleetd
//      booting against a live incumbent's pidfile is usually a REPLACEMENT
//      two concurrent upgrade hooks spawned. Version arbitration applies: a
//      strictly newer, unmanaged boot supersedes a strictly older, unmanaged
//      incumbent (SIGTERM + wait-for-death); every other pairing keeps the
//      historical claimHome refusal (exit 1, incumbent untouched).

import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, spawnRaw, randomPort, waitForHealth } from './helpers/daemon.mjs';

test('a second daemon on the same port loses the election and exits with code 3', async (t) => {
  const port = randomPort();
  const homeA = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-a-'));
  const homeB = mkdtempSync(path.join(tmpdir(), 'fleetdeck-home-b-'));
  t.after(() => {
    rmSync(homeA, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(homeB, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const winner = await startDaemon({ port, home: homeA });
  t.after(async () => { await winner.stop(); });

  const loser = spawnRaw({ port, home: homeB });
  t.after(async () => { await loser.kill(); });

  const code = await loser.waitForExit(10000);
  assert.equal(code, 3, `loser should exit 3 on EADDRINUSE, got ${code}. stderr: ${loser.stderr}`);

  // The winner should be completely unaffected.
  const health = await fetch(`${winner.baseUrl}/health`);
  assert.equal(health.status, 200, 'the winning daemon should remain healthy after the collision');
});

<<<<<<< /tmp/mf-ours
// BUG-121 regression: a malformed FLEETDECK_PORT made server.listen throw
// synchronously AFTER the pid guard claimed HOME — the async server 'error'
// handler never ran, so the stale pidfile (recording a garbage port) wedged
// every supervised restart until it was removed by hand. The daemon must
// refuse a bad port BEFORE touching HOME: exit 1, a clear reason on stderr,
// and no pidfile written.
test('a malformed FLEETDECK_PORT refuses to start before claiming HOME (no stale pidfile)', async (t) => {
  for (const bad of ['banana', '4711.5', '-1', '70000']) {
    const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-badport-'));
    t.after(() => { rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

    const refused = spawnRaw({ port: bad, home });
    t.after(async () => { await refused.kill(); });

    const code = await refused.waitForExit(10000);
    assert.equal(code, 1, `FLEETDECK_PORT='${bad}' should exit 1, got ${code}. stderr: ${refused.stderr}`);
    assert.match(refused.stderr, /FLEETDECK_PORT/, `stderr should name the offending variable. stderr: ${refused.stderr}`);
    assert.equal(existsSync(path.join(home, 'fleetd.pid')), false, `FLEETDECK_PORT='${bad}' must not leave a pidfile behind`);
  }
=======
// BUG-156 regression: a second daemon on the same HOME is usually a takeover
// REPLACEMENT (two concurrent newer hooks both spawn after evicting the stale
// daemon). Plain "first claim wins" has no notion of version, so the OLDER
// candidate's build could keep HOME+port and the newest build would die at
// claimHome — settling the upgrade on superseded code. The same-HOME
// arbitration lets a strictly newer, unmanaged boot supersede a strictly
// older, unmanaged incumbent (SIGTERM + wait-for-death, the takeover
// contract's own graceful handoff), and keeps the historical refusal for
// every other pairing.
test('a strictly newer same-HOME daemon supersedes the older incumbent instead of dying at claimHome', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-reelect-home-'));
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  // The incumbent reports 0.0.1; the challenger reports 99.0.0 (both via the
  // test-only override, so the ordering does not depend on package.json).
  const incumbent = await startDaemon({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1' } });
  const incumbentPid = incumbent.proc.pid;

  const challenger = spawnRaw({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '99.0.0' } });
  t.after(async () => { await challenger.kill(); });

  // The incumbent is gracefully superseded (exit 0 via SIGTERM shutdown, not
  // a hard kill), and the challenger takes over the SAME port + home.
  await new Promise((resolve) => {
    if (incumbent.proc.exitCode !== null) return resolve();
    incumbent.proc.once('exit', resolve);
    setTimeout(resolve, 10000);
  });
  assert.equal(incumbent.proc.exitCode, 0,
    `the older incumbent must exit 0 via its graceful shutdown (got ${incumbent.proc.exitCode}; challenger stderr: ${challenger.stderr})`);

  const health = await waitForHealth(`http://127.0.0.1:${port}`, 8000);
  assert.equal(health.version, '99.0.0', 'the strictly newer challenger owns the port');
  assert.notEqual(health.pid, incumbentPid, 'the survivor is the challenger, not the incumbent');
  assert.equal(challenger.proc.exitCode, null, 'the challenger must still be running (claimHome did not refuse it)');
});

test('an older or equal same-HOME challenger is refused and leaves the incumbent serving', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-reelect-older-home-'));
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const incumbent = await startDaemon({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '99.0.0' } });
  t.after(async () => { await incumbent.stop(); });
  const before = incumbent.proc.pid;

  // OLDER challenger: must never evict — and never boot. claimHome refuses
  // with exit 1 (startupFatal), as it always has for a live same-HOME lock.
  const older = spawnRaw({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1' } });
  t.after(async () => { await older.kill(); });
  assert.equal(await older.waitForExit(10000), 1,
    `an older challenger must be refused at claimHome (stderr: ${older.stderr})`);
  assert.match(older.stderr, /already used by live fleetd pid/);

  // EQUAL challenger: equal never evicts (the takeover contract's own rule).
  const equal = spawnRaw({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '99.0.0' } });
  t.after(async () => { await equal.kill(); });
  assert.equal(await equal.waitForExit(10000), 1,
    `an equal-version challenger must be refused at claimHome (stderr: ${equal.stderr})`);

  const health = await (await fetch(`http://127.0.0.1:${port}/health`)).json();
  assert.equal(health.pid, before, 'the incumbent is untouched by older/equal challengers');
  assert.equal(health.version, '99.0.0');
});

test('a newer challenger never supersedes a MANAGED incumbent (the service owns HOME+port)', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-reelect-managed-home-'));
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  // A managed daemon pinned OLDER — exactly the build the takeover contract
  // exists to evict, except a supervisor owns it and will restart whatever is
  // killed: evicting it restarts the very race FLEETDECK_MANAGED prevents.
  const svc = await startDaemon({
    port, home,
    env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1', FLEETDECK_MANAGED: '1' },
  });
  t.after(async () => { await svc.stop(); });
  const svcPid = svc.proc.pid;

  const challenger = spawnRaw({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '99.0.0' } });
  t.after(async () => { await challenger.kill(); });
  assert.equal(await challenger.waitForExit(10000), 1,
    `a challenger against a managed incumbent must be refused (stderr: ${challenger.stderr})`);

  const health = await (await fetch(`http://127.0.0.1:${port}/health`)).json();
  assert.equal(health.pid, svcPid, 'the managed incumbent must be the SAME process — never superseded');
  assert.equal(health.version, '0.0.1');
  assert.equal(svc.proc.exitCode, null, 'the managed daemon was never signalled');
});

test('a MANAGED challenger never fights for HOME, even against an older unmanaged incumbent', async (t) => {
  const port = randomPort();
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-reelect-managed-challenger-home-'));
  t.after(() => rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const incumbent = await startDaemon({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '0.0.1' } });
  t.after(async () => { await incumbent.stop(); });
  const incumbentPid = incumbent.proc.pid;

  // The managed build is strictly NEWER — but it is supervised, so boot-time
  // eviction would put it in the hook/supervisor race. It must be refused and
  // let the supervisor's own restart cadence converge instead.
  const managed = spawnRaw({ port, home, env: { FLEETDECK_VERSION_OVERRIDE: '99.0.0', FLEETDECK_MANAGED: '1' } });
  t.after(async () => { await managed.kill(); });
  assert.equal(await managed.waitForExit(10000), 1,
    `a managed challenger must be refused rather than fight for HOME (stderr: ${managed.stderr})`);

  const health = await (await fetch(`http://127.0.0.1:${port}/health`)).json();
  assert.equal(health.pid, incumbentPid, 'the unmanaged incumbent keeps serving');
  assert.equal(incumbent.proc.exitCode, null, 'the incumbent was never signalled');
>>>>>>> /tmp/mf-theirs
});
