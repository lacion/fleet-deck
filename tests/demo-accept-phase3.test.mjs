// tests/demo-accept-phase3.test.mjs
//
// Regression coverage for BUG-094: demo/run-accept-phase3.sh fired several
// curl requests (permission-answer POST, freeform state read, freeform answer
// POST, final evidence state read) with NO connect or total timeout. A
// listener that accepts the loopback connection but never completes the
// response made curl wait indefinitely — outside the 240s Claude watchdogs —
// hanging a billed live gate without ever reaching cleanup or a verdict.
//
// These tests check two things:
//   1. Every curl invocation in the script carries a total-time bound
//      (-m / --max-time) and POSTs also carry --connect-timeout.
//   2. The script installs an overall deadline that fires even when a curl
//      hangs: run against a black-hole server that accepts and never
//      responds, the script must exit on its own before the deadline elapses.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const SCRIPT = path.join(REPO_ROOT, 'demo/run-accept-phase3.sh');

const src = readFileSync(SCRIPT, 'utf8');

// Extract curl invocations: `curl` up to the end of its (possibly
// backslash-continued) command line.
function curlInvocations(source) {
  const invocations = [];
  for (const match of source.matchAll(/^[^\n#]*\bcurl\b[^\n]*(?:\\\n[^\n]*)*/gm)) {
    invocations.push(match[0]);
  }
  return invocations;
}

test('every curl in run-accept-phase3.sh has a total-time bound', () => {
  const invocations = curlInvocations(src);
  assert.ok(invocations.length > 0, 'expected curl invocations in the script');
  for (const inv of invocations) {
    assert.match(
      inv,
      /(^|\s)(-m|--max-time)(\s|=)/,
      `curl without -m/--max-time — can hang forever on a silent listener:\n${inv}`,
    );
  }
});

test('every state/answer curl also bounds connection establishment', () => {
  const invocations = curlInvocations(src).filter(
    (inv) => inv.includes('/state') || inv.includes('/answer'),
  );
  assert.ok(invocations.length > 0, 'expected state/answer curls in the script');
  for (const inv of invocations) {
    assert.match(
      inv,
      /--connect-timeout(\s|=)/,
      `state/answer curl without --connect-timeout:\n${inv}`,
    );
  }
});

// The gate must not block forever even if some request still wedges: the
// script's overall deadline must fire and terminate it. We point the script
// at a black-hole TCP server (accepts, never speaks a byte) and give it a
// 3-second deadline via the override; a script without a deadline would hang
// here until the test's own hard kill — well past 3s.
test('script terminates on its overall deadline when the fleet server hangs', { timeout: 60000 }, async () => {
  const blackhole = createServer((sock) => {
    sock.on('error', () => {});
    // Never write, never close: curl's connection "succeeds", response never comes.
  });
  await new Promise((resolve) => blackhole.listen(0, '127.0.0.1', resolve));
  const port = blackhole.address().port;

  const started = Date.now();
  let timedOut = false;
  const result = await new Promise((resolve) => {
    const child = spawn('bash', [SCRIPT], {
      env: {
        ...process.env,
        FLEETDECK_PORT: String(port),
        FLEETDECK_ACCEPT_DEADLINE_S: '3',
        // Deflect any scratch state away from real fleet dirs.
        FLEETDECK_HOME_OVERRIDE: '/tmp/fd-bug094-scratch',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => { out += d; });
    child.stderr.on('data', (d) => { out += d; });
    // Hard-kill backstop far beyond the 3s deadline: if the script has no
    // deadline of its own it would hang here (pre-fix behavior).
    const killer = setTimeout(() => {
      timedOut = true;
      child.kill('SIGKILL');
    }, 20000);
    child.on('exit', (code, signal) => {
      clearTimeout(killer);
      resolve({ code, signal, out });
    });
  });
  blackhole.close();

  const elapsedMs = Date.now() - started;
  assert.equal(timedOut, false, `script hung past the 20s backstop (no overall deadline):\n${result.out}`);
  assert.ok(
    elapsedMs < 15000,
    `script took ${elapsedMs}ms — deadline did not fire promptly:\n${result.out}`,
  );
  assert.match(result.out, /overall deadline/, `expected deadline abort message in output:\n${result.out}`);
});
