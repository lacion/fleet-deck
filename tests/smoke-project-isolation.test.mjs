// tests/smoke-project-isolation.test.mjs
//
// BUG-016 regression: demo/run-smoke.sh used to run its workers directly in
// the tracked fixture at demo/project and arm an unconditional EXIT trap that
// copied .seed files over util.js/app.js and DELETED test.js and
// .claude/settings.json — even when the run aborted before mutating anything.
// A developer's uncommitted work in that directory was destroyed by a smoke
// run (or by an immediate abort of one).
//
// The fix runs the workers in a unique per-run copy of the fixture under the
// scratch home; the tracked checkout is read exactly once and never written.
// These tests stub `claude`/`tmux`/`curl` so no real session, token spend,
// or shared tmux server is involved, then assert byte-for-byte that the
// tracked fixture survives both a full run and an abort-before-setup path.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RUN_SMOKE = path.join(REPO_ROOT, 'demo', 'run-smoke.sh');
const FIXTURE_FILES = ['util.js', 'app.js', 'package.json', 'test.js', '.claude/settings.json'];

// Seed the files the script itself waits on: the bearer token (written by the
// elected daemon in a real run) and a strict pid record for THIS test process
// so cleanup's three-identity daemon check (pidfile, health pid, node+fleetd
// process shape) verifies successfully and reaps the scratch home. The pid
// record must name the smoke's port, so each sandbox gets its own
// FLEETDECK_SMOKE_PORT.
function seedDaemonProofs(t, sandbox, binDir, smokePort) {
  const scratchHome = mkdtempSync(path.join(sandbox, 'fleetdeck-smoke.'));
  writeFileSync(path.join(scratchHome, 'token'), 'test-token\n');
  writeFileSync(path.join(scratchHome, 'fleetd.pid'), JSON.stringify({ pid: process.pid, port: Number(smokePort) }));
  t.after(() => {
    try { process.kill(process.pid, 'SIGUSR1'); } catch { /* health server already replaced us */ }
  });

  // The daemon check calls `node -e` and fetch()es /health expecting this pid.
  // A shim named `node` re-exports itself as `fleetd.mjs` so the /proc cmdline
  // shape check passes, then execs the real node.
  const selfShim = `#!/bin/bash\nexec "${process.execPath}" "$@"\n`;
  writeFileSync(path.join(binDir, 'node'), selfShim);
  writeFileSync(path.join(binDir, 'fleetd.mjs'), selfShim);
  chmodSync(path.join(binDir, 'node'), 0o755);
  chmodSync(path.join(binDir, 'fleetd.mjs'), 0o755);

  // The first SIGUSR1 swaps this process for a tiny health server answering
  // with the recorded pid; the smoke's SIGTERM then closes it and the process
  // exits, satisfying the liveness poll.
  process.once('SIGUSR1', () => {
    const { createServer } = require('node:http');
    const server = createServer((req, res) => {
      if (req.url === '/health') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ pid: process.pid, port: Number(smokePort) }));
      } else {
        res.writeHead(404);
        res.end();
      }
    });
    server.listen(Number(smokePort), '127.0.0.1');
    const close = () => server.close(() => process.exit(0));
    process.once('SIGUSR1', close);
    process.once('SIGTERM', close);
    setTimeout(() => process.exit(0), 30_000).unref();
  });
}


function makeSandbox(t, smokePort) {
  const sandbox = mkdtempSync(path.join(tmpdir(), 'fd-smoke-iso-'));
  t.after(() => rmSync(sandbox, { recursive: true, force: true }));

  // Private copy of the whole demo/ tree — the real fixture must never be
  // touched by the test itself. run-smoke.sh resolves the repo layout from
  // its own path (scripts/fleet-*.mjs must exist one level up), so mirror it.
  const demoDir = path.join(sandbox, 'demo');
  cpSync(path.join(REPO_ROOT, 'demo'), demoDir, { recursive: true });
  const scriptsDir = path.join(sandbox, 'scripts');
  mkdirSync(scriptsDir, { recursive: true });
  for (const stub of ['fleet-sessionstart.mjs', 'fleet-hook.mjs']) {
    writeFileSync(path.join(scriptsDir, stub), '// stub for smoke-project-isolation test\n');
  }

  // Dirty the sandbox's fixture: uncommitted edits plus files a developer
  // would legitimately have and the old cleanup trap destroyed.
  const projectDir = path.join(demoDir, 'project');
  writeFileSync(path.join(projectDir, 'util.js'), '// MY UNCOMMITTED WORK util\n');
  writeFileSync(path.join(projectDir, 'app.js'), '// MY UNCOMMITTED WORK app\n');
  writeFileSync(path.join(projectDir, 'test.js'), '// MY LOCAL test.js\n');
  mkdirSync(path.join(projectDir, '.claude'), { recursive: true });
  writeFileSync(path.join(projectDir, '.claude', 'settings.json'), '{"my":"local settings"}\n');

  // Stub bin: a claude that behaves like a successful worker (writes its
  // edits + a successful JSON result), a tmux that absorbs kill-server, and a
  // curl whose /health probe always misses (the port is always "free") while
  // the /state capture returns the state the verify block wants.
  const binDir = path.join(sandbox, 'bin');
  mkdirSync(binDir, { recursive: true });
  const sidA = '11111111-1111-4111-8111-111111111111';
  const sidB = '22222222-2222-4222-8222-222222222222';
  writeFileSync(
    path.join(binDir, 'claude'),
    `#!/bin/bash
out=""
sid=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output-format) out="$2"; shift 2 ;;
    --session-id) sid="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ "$out" = "json" ]; then
  echo 'function slugify(s) { return s; }' >> util.js
  echo '// tests' >> test.js
  printf '{"is_error":false,"subtype":"success"}\\n'
fi
exit 0
`,
  );
  writeFileSync(
    path.join(binDir, 'tmux'),
    `#!/bin/bash
exit 0
`,
  );
  writeFileSync(
    path.join(binDir, 'curl'),
    `#!/bin/bash
url=""
for arg in "$@"; do case "$arg" in http*) url="$arg" ;; esac; done
case "$url" in
  *health*) exit 7 ;;
  */state)
    printf '{"sessions":[{"session_id":"${sidA}","callsign":"alpha","col":"offline","endedAt":1},{"session_id":"${sidB}","callsign":"bravo","col":"offline","endedAt":1}],"conflicts":[{"rel_path":"util.js"},{"rel_path":"test.js"}],"ticker":[{"msg":"alpha got fleet mail at the turn boundary"},{"msg":"bravo got fleet mail at the turn boundary"}]}\\n'
    ;;
  *) exit 0 ;;
esac
`,
  );
  for (const stub of ['claude', 'tmux', 'curl']) chmodSync(path.join(binDir, stub), 0o755);
  symlinkSync('/usr/bin/setsid', path.join(binDir, 'setsid'));
  symlinkSync('/usr/bin/timeout', path.join(binDir, 'timeout'));

  seedDaemonProofs(t, sandbox, binDir, smokePort);

  return { sandbox, demoDir, projectDir, binDir };
}

function runSmoke(sandbox, demoDir, binDir, smokePort, scriptArgs = [], extraEnv = {}) {
  return spawnSync('bash', [path.join(demoDir, 'run-smoke.sh'), ...scriptArgs], {
    cwd: sandbox,
    encoding: 'utf8',
    env: {
      ...process.env,
      PATH: `${binDir}:/usr/bin:/bin`,
      HOME: sandbox,
      TMPDIR: sandbox,
      FLEETDECK_SMOKE_PORT: smokePort,
      ...extraEnv,
    },
  });
}

function assertFixturePreserved(t, projectDir, label) {
  for (const rel of FIXTURE_FILES) {
    const p = path.join(projectDir, rel);
    assert.ok(existsSync(p), `${label}: ${rel} must still exist after the run`);
  }
  assert.equal(readFileSync(path.join(projectDir, 'util.js'), 'utf8'), '// MY UNCOMMITTED WORK util\n', `${label}: util.js`);
  assert.equal(readFileSync(path.join(projectDir, 'app.js'), 'utf8'), '// MY UNCOMMITTED WORK app\n', `${label}: app.js`);
  assert.equal(readFileSync(path.join(projectDir, 'test.js'), 'utf8'), '// MY LOCAL test.js\n', `${label}: test.js`);
  assert.equal(
    readFileSync(path.join(projectDir, '.claude', 'settings.json'), 'utf8'),
    '{"my":"local settings"}\n',
    `${label}: .claude/settings.json`,
  );
}

// A smoke run that reaches the worker phase and then aborts must leave the
// tracked fixture byte-for-byte intact, including files that did not come
// from the seed (test.js, local settings). The old script's EXIT trap
// restored util.js/app.js from .seed and DELETED test.js and
// .claude/settings.json from the checkout at exactly this point; the fixed
// script's trap only reaps the run-unique scratch home. A stub `node` exits
// the script right after the workers are spawned so no daemon is needed.
test('run-smoke.sh aborting after the workers leaves the fixture untouched', { timeout: 60_000 }, async (t) => {
  const smokePort = '28971';
  const { sandbox, demoDir, projectDir, binDir } = makeSandbox(t, smokePort);
  // Two UUIDs for SA/SB, then force the "no token minted" abort. Keeping the
  // default node (the health-server shim) would leave a daemon running, which
  // is out of scope for this fixture-preservation test.
  writeFileSync(path.join(binDir, 'node'), '#!/bin/bash\necho 00000000-0000-4000-8000-000000000000\nexit 0\n');
  chmodSync(path.join(binDir, 'node'), 0o755);
  const result = runSmoke(sandbox, demoDir, binDir, smokePort);
  assert.notEqual(result.status, 0, 'post-worker abort run must fail');
  assert.match(result.stdout + result.stderr, /smoke daemon did not mint its bearer token/);
  assert.match(result.stdout, /T\+15 session B launched/, 'run must have reached the worker phase');
  assertFixturePreserved(t, projectDir, 'post-worker abort run');
});

// The historical trap was armed before the dependency preflight, so even an
// immediate abort reset/deleted the fixture. The abort path must be equally
// read-only against the checkout. A stub `timeout` exits 127 IMMEDIATELY, so
// both workers die at spawn and the token check fails fast — no 300s kill,
// no daemon, and the same fixture-preservation assertion runs against the
// trap. (The preflight itself is unreachable without regressing the sandbox:
// PATH must keep /usr/bin for dirname/mktemp/mkdir, which provides timeout.)
test('run-smoke.sh aborting with dead workers leaves the fixture untouched', { timeout: 30_000 }, async (t) => {
  const smokePort = '28972';
  const { sandbox, demoDir, projectDir, binDir } = makeSandbox(t, smokePort);
  rmSync(path.join(binDir, 'timeout')); // replace the /usr/bin/timeout symlink
  writeFileSync(path.join(binDir, 'timeout'), '#!/bin/bash\nexit 127\n');
  chmodSync(path.join(binDir, 'timeout'), 0o755);
  const result = runSmoke(sandbox, demoDir, binDir, smokePort);
  assert.notEqual(result.status, 0, 'abort run must fail');
  assert.match(result.stdout + result.stderr, /smoke daemon did not mint its bearer token/);
  assertFixturePreserved(t, projectDir, 'dead-worker abort run');
});
