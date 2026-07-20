// tests/gateway-newwindow.test.mjs
//
// The tmux adapter's `-e` construction, tested directly.
//
// Why this file exists separately: every OTHER gateway test drives the daemon
// through the FLEETDECK_SPAWN_CMD seam, which replaces tmux wholesale — so
// newWindow() itself, the single mechanism the feature's central security claim
// rests on, was never executed by the suite at all. That claim is:
//
//   the gateway credential reaches the pane through tmux's own environment, so
//   it never enters the pane's command line and never appears in `ps` output
//
// If `-e` were ever dropped, or the credential were appended to `argv` instead,
// every seam-based test would still pass while a live credential became
// world-readable to every other OS user on the machine for the multi-hour life
// of the pane. So this asserts on the exact argv handed to tmux.
//
// The adapter runs `tmux` via execFile with no shell, so PATH is what decides
// which binary that is: these tests put a recording stub first on PATH rather
// than mocking the module, which keeps the real argv-array construction — the
// part that matters — under test.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, chmodSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const TOKEN = 'zzUNIQUE-credential-9871';

/** A fake `tmux` first on PATH that appends each invocation's argv as JSONL. */
function stubTmux(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-gwtmux-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const log = path.join(dir, 'argv.jsonl');
  const bin = path.join(dir, 'tmux');
  writeFileSync(bin, [
    '#!/usr/bin/env node',
    `require('node:fs').appendFileSync(${JSON.stringify(log)}, JSON.stringify(process.argv.slice(2)) + '\\n');`,
    // new-window -P -F '#{window_id}' expects a window id on stdout.
    "if (process.argv.includes('new-window')) process.stdout.write('@7\\n');",
    'process.exit(0);',
  ].join('\n'));
  chmodSync(bin, 0o755);
  const previous = process.env.PATH;
  process.env.PATH = `${dir}${path.delimiter}${previous}`;
  t.after(() => { process.env.PATH = previous; });
  return () => (existsSync(log) ? readFileSync(log, 'utf8').split('\n').filter(Boolean).map(JSON.parse) : []);
}

/** The argv of the first `new-window` invocation. */
function newWindowArgv(calls) {
  return calls.find(a => a.includes('new-window'));
}

test('newWindow: gateway env travels as tmux -e pairs, never in the pane command', async (t) => {
  const calls = stubTmux(t);
  const { newWindow } = await import('../scripts/fleetd/spawn.mjs');

  const argv = ['env', '-u', 'CLAUDECODE', 'FLEETDECK_PORT=4711', 'claude', '--session-id', 'sid'];
  await newWindow({
    port: 4711, callsign: 'heron-1', cwd: '/tmp', argv,
    env: { ANTHROPIC_BASE_URL: 'http://127.0.0.1:8317', ANTHROPIC_AUTH_TOKEN: TOKEN },
  });

  const nw = newWindowArgv(calls());
  assert.ok(nw, 'tmux new-window must have been invoked');

  // The credential rides -e …
  const eIdx = nw.indexOf('-e');
  assert.ok(eIdx >= 0, 'gateway env must be delivered with -e');
  const ePairs = nw.filter((a, i) => nw[i - 1] === '-e');
  assert.deepEqual(ePairs.sort(), [
    `ANTHROPIC_AUTH_TOKEN=${TOKEN}`,
    'ANTHROPIC_BASE_URL=http://127.0.0.1:8317',
  ], 'each variable is one -e NAME=value pair');

  // … and NOT the pane's command. THIS is the assertion the security claim
  // rests on: everything after `--` is what the pane actually execs, and it is
  // what `ps` shows for the life of that pane.
  const dashDash = nw.lastIndexOf('--');
  assert.ok(dashDash >= 0, 'the pane command must still be terminated by --');
  const paneCommand = nw.slice(dashDash + 1);
  assert.deepEqual(paneCommand, argv, 'the pane command is the caller argv, unchanged');
  assert.equal(paneCommand.some(a => a.includes(TOKEN)), false,
    'the credential must never appear in the pane command');

  // Every -e pair must sit BEFORE the -- terminator, or tmux would read it as
  // part of the command rather than as an option.
  assert.ok(eIdx < dashDash, '-e pairs must precede the -- terminator');
});

test('newWindow: no env argument means no -e flags at all', async (t) => {
  const calls = stubTmux(t);
  const { newWindow } = await import('../scripts/fleetd/spawn.mjs');

  await newWindow({ port: 4711, callsign: 'heron-2', cwd: '/tmp', argv: ['claude'] });

  const nw = newWindowArgv(calls());
  assert.equal(nw.includes('-e'), false,
    'a non-gateway spawn must produce a byte-identical command to the pre-0.15.0 adapter');
});

test('newWindow: a value containing shell metacharacters stays one argv element', async (t) => {
  const calls = stubTmux(t);
  const { newWindow } = await import('../scripts/fleetd/spawn.mjs');

  // execFile with no shell means these are literal bytes, but a future refactor
  // to a shell string would silently turn them into command substitution.
  const hostile = 'tok"; touch /tmp/fd-pwned; echo "$(id)';
  await newWindow({
    port: 4711, callsign: 'heron-3', cwd: '/tmp', argv: ['claude'],
    env: { ANTHROPIC_AUTH_TOKEN: hostile },
  });

  const nw = newWindowArgv(calls());
  const pair = nw[nw.indexOf('-e') + 1];
  assert.equal(pair, `ANTHROPIC_AUTH_TOKEN=${hostile}`,
    'the value arrives verbatim as a single element — no shell ever sees it');
  assert.equal(existsSync('/tmp/fd-pwned'), false, 'nothing was executed');
});
