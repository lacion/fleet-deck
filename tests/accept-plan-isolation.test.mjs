// tests/accept-plan-isolation.test.mjs
//
// BUG-013: demo/run-accept-plan.sh is a BILLED acceptance gate that launches a
// scratch fleetd and then lets it spawn/control real Claude sessions. Its
// readiness probe used to accept whichever daemon answered /state first — a
// listener that survives the reset (or a supervisor restart) wins the port,
// the scratch child dies on EADDRINUSE, and the plan gates would then steer
// that foreign, possibly production, fleet.
//
// The fix binds readiness to the launched child: the port must be free before
// launch, /health.pid must equal DAEMON_PID, the scratch pidfile must record
// the same pid+port, and the child exiting before readiness aborts the run.
//
// The live test below reproduces the race for real: a stand-in "foreign"
// daemon appears on the port after the pre-launch check but before the
// scratch child binds (the stand-in child delays its bind), the child exits 3
// on EADDRINUSE, and the script must abort WITHOUT a single mutating API call
// (/api/spawn, /api/questions, /api/plans) reaching the foreign daemon.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SCRIPT = path.join(REPO_ROOT, 'demo', 'run-accept-plan.sh');

test('gate 1 readiness is bound to the launched child, not to any answering daemon', () => {
  const src = fs.readFileSync(SCRIPT, 'utf8');
  const gate1 = src.slice(src.indexOf('# --------------------------------------------------------------- gate 1'),
    src.indexOf('# --------------------------------------------------------------- gate 2'));

  // The readiness probe must interrogate /health (which carries the daemon's
  // pid), never a bare /state that any qualifying listener can answer.
  assert.ok(!gate1.includes('"$BASE/state"'), 'gate 1 must not accept readiness from a bare /state');
  assert.ok(gate1.includes('"$BASE/health"'), 'gate 1 must probe /health');
  assert.ok(gate1.includes('h.pid !== expectedPid'), 'readiness must require /health.pid === DAEMON_PID');
  assert.ok(gate1.includes('fleetd.pid'), 'readiness must cross-check the scratch pidfile');

  // The port must be proven free before the child is launched, and the child
  // dying before readiness (EADDRINUSE against a surviving listener) must be
  // terminal rather than falling through to the foreign daemon.
  assert.ok(src.includes('refusing to run the plan gate against an unowned listener'),
    'script must refuse to launch while the port still answers');
  assert.ok(src.includes('aborting to avoid steering a foreign fleet'),
    'script must abort when the scratch child exits before becoming ready');
});

test('a foreign listener that wins the port is never sent plan-gate mutations', async (t) => {
  // Build a minimal stand-in repo: the acceptance script computes every path
  // from its own location, so a copy plus a stub fleetd is the whole fixture.
  const repo = mkdtempSync(path.join(tmpdir(), 'fleetdeck-accept-plan-'));
  t.after(() => rmSync(repo, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  fs.mkdirSync(path.join(repo, 'demo', 'project', '.seed'), { recursive: true });
  fs.mkdirSync(path.join(repo, 'scripts', 'fleetd'), { recursive: true });
  fs.copyFileSync(SCRIPT, path.join(repo, 'demo', 'run-accept-plan.sh'));
  fs.writeFileSync(path.join(repo, 'demo', 'project', '.seed', 'util.js'), 'module.exports = {};\n');

  // The scratch "fleetd": claims its pidfile, then waits for a test-created
  // go signal before binding so the test can deterministically slip a foreign
  // listener onto the port first — the exact BUG-013 race. (A fixed setTimeout
  // here races the node test runner's own event-loop stalls; the go file makes
  // the ordering explicit.)
  fs.writeFileSync(path.join(repo, 'scripts', 'fleetd', 'fleetd.mjs'), `
    import fs from 'node:fs';
    import http from 'node:http';
    const home = process.env.FLEETDECK_HOME;
    const port = Number(process.env.FLEETDECK_PORT);
    fs.writeFileSync(home + '/fleetd.pid', JSON.stringify({ pid: process.pid, port }));
    fs.writeFileSync(home + '/child-ready.txt', String(process.pid));
    const go = () => {
      const server = http.createServer((req, res) => res.end('{}'));
      server.on('error', (err) => {
        fs.writeFileSync(home + '/child-bind-error.txt', String(err?.code || err));
        process.exit(3);
      });
      server.listen(port, '127.0.0.1');
    };
    // watchFile fires once on registration for a missing file; only the
    // first change after that initial event is the test's go signal.
    let armed = false;
    fs.watchFile(home + '/go', { interval: 25 }, () => {
      if (!armed) { armed = true; return; }
      fs.unwatchFile(home + '/go');
      go();
    });
  `);

  // The script hardcodes FLEETDECK_PORT (the environment cannot override
  // it), so the fixture copy's port line is patched to a free port below;
  // the whole run — BASE, the scratch daemon env, the pid cross-check —
  // follows that one assignment.

  const scratch = path.join(repo, '.fleetdeck-test');
  // Patch only the port line: FLEETDECK_PORT feeds BASE, the scratch daemon's
  // env, and the pid cross-check, so the whole run follows it.
  const scriptCopy = path.join(repo, 'demo', 'run-accept-plan.sh');
  const original = fs.readFileSync(scriptCopy, 'utf8');
  const portLine = original.match(/^FLEETDECK_PORT=.+$/m);
  assert.ok(portLine, 'acceptance script must set FLEETDECK_PORT');
  const port = await new Promise((resolve, reject) => {
    const probe = http.createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const p = probe.address().port;
      probe.close(() => resolve(p));
    });
  });
  // BUG-097 made the shipped script mktemp its SCRATCH_HOME (unique per run);
  // this test needs a KNOWN home to poll the stub's child-ready.txt, so pin the
  // script COPY's SCRATCH_HOME to `scratch`. The shipped script is unchanged
  // (its mktemp behavior is covered by demo-accept-isolation).
  let patched = original.replace(portLine[0], `FLEETDECK_PORT=${port}`);
  patched = patched.replace(
    /SCRATCH_HOME="\$\(mktemp -d "\$\{TMPDIR:-\/tmp\}\/fleetdeck-plan\.XXXXXX"\)"/,
    `mkdir -p ${JSON.stringify(scratch)}; SCRATCH_HOME=${JSON.stringify(scratch)}`,
  );
  assert.ok(patched.includes(`SCRATCH_HOME=${JSON.stringify(scratch)}`), 'must pin the script copy SCRATCH_HOME');
  fs.writeFileSync(scriptCopy, patched);

  const child = spawn('bash', [scriptCopy], {
    cwd: repo,
    env: { ...process.env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  // The script's own cleanup stops the stub, but a failed assertion mid-test
  // must never strand a listener from this fixture.
  t.after(() => { try { child.kill('SIGKILL'); } catch { /* already gone */ } });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (d) => { stdout += d; });
  child.stderr.on('data', (d) => { stderr += d; });

  // Wait for the script to launch its child (the stub announces itself via
  // child-ready.txt), then install the foreign daemon on the same port: it
  // answers /health and /state with spawn.available=true and records every
  // request. SO_REUSEADDR is NOT set: this listener must hold the port
  // exclusively, exactly like a surviving production fleetd would. The
  // EADDRINUSE case below means the stub beat us to the bind — that cannot
  // happen with the go-signal held, so a bind failure here is a real defect.
  const childReady = path.join(scratch, 'child-ready.txt');
  await new Promise((resolve, reject) => {
    const deadline = Date.now() + 10000;
    const tick = () => {
      if (fs.existsSync(childReady)) return resolve();
      if (Date.now() > deadline) return reject(new Error('script never launched its scratch fleetd'));
      setTimeout(tick, 25);
    };
    tick();
  });

  const requestLog = path.join(repo, 'foreign-requests.jsonl');
  const foreign = http.createServer((req, res) => {
    fs.appendFileSync(requestLog, JSON.stringify({ method: req.method, url: req.url }) + '\n');
    req.resume();
    req.on('end', () => {
      if (req.url === '/health') {
        res.end(JSON.stringify({ ok: true, pid: process.pid, spawn: { available: true } }));
      } else if (req.url === '/state') {
        res.end(JSON.stringify({ spawn: { available: true }, sessions: [], questions: [], plans: [] }));
      } else if (req.method === 'POST') {
        res.statusCode = 500;
        res.end(JSON.stringify({ ok: false }));
      } else {
        res.end('{}');
      }
    });
  });
  foreign.exclusive = true;
  t.after(() => new Promise((resolve) => foreign.close(resolve)));
  await new Promise((resolve, reject) => {
    foreign.once('error', reject);
    foreign.listen({ port, host: '127.0.0.1', exclusive: true }, resolve);
  });

  // The foreign daemon now owns the port: release the scratch child so it
  // loses the bind and exits 3 (EADDRINUSE), exactly as fleetd would.
  fs.writeFileSync(path.join(scratch, 'go'), 'go');

  const exitCode = await new Promise((resolve) => child.on('close', resolve));

  // The race really happened: the scratch child lost the bind and exited.
  assert.match(fs.readFileSync(path.join(scratch, 'child-bind-error.txt'), 'utf8'), /EADDRINUSE/);

  // The gate must abort on its dead child instead of adopting the foreign
  // daemon, and the foreign daemon must see ZERO mutating calls.
  assert.equal(exitCode, 1, `script must abort; stdout:\n${stdout}`);
  assert.match(stderr, /aborting to avoid steering a foreign fleet/);
  const calls = fs.readFileSync(requestLog, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  assert.equal(calls.filter((c) => c.method === 'POST').length, 0,
    `foreign daemon must never receive plan-gate mutations, got: ${JSON.stringify(calls)}`);
});
