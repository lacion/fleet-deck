// tests/audit-hardening.test.mjs — regression coverage for the audit's
// local diagnostic/launcher resource and permission boundaries.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createPayloadCapture } from '../scripts/fleetd/payload-capture.mjs';
import { randomPort } from './helpers/daemon.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const WATCH = path.join(REPO_ROOT, 'scripts/fleet-watch.mjs');
const SESSIONSTART = path.join(REPO_ROOT, 'scripts/fleet-sessionstart.mjs');

function scratch(t, prefix = 'fleetdeck-audit-') {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function exitOf(child, timeoutMs = 6000) {
  return Promise.race([
    new Promise(resolve => child.once('exit', (code, signal) => resolve({ code, signal }))),
    new Promise((_, reject) => setTimeout(() => reject(new Error('child did not exit in time')), timeoutMs)),
  ]);
}

test('payload capture is off by default and enabled only by the explicit on flag', (t) => {
  const home = scratch(t);
  const file = path.join(home, 'hook-payloads.jsonl');
  const previous = process.env.FLEETDECK_CAPTURE_PAYLOADS;
  t.after(() => {
    if (previous === undefined) delete process.env.FLEETDECK_CAPTURE_PAYLOADS;
    else process.env.FLEETDECK_CAPTURE_PAYLOADS = previous;
  });

  delete process.env.FLEETDECK_CAPTURE_PAYLOADS;
  createPayloadCapture(home)('Stop', { prompt: 'must not persist' });
  assert.equal(existsSync(file), false, 'default capture must not even create the file');

  process.env.FLEETDECK_CAPTURE_PAYLOADS = 'on';
  createPayloadCapture(home)('Stop', { prompt: 'diagnostic' });
  assert.equal(JSON.parse(readFileSync(file, 'utf8')).payload.prompt, 'diagnostic');
  assert.equal(statSync(file).mode & 0o777, 0o600, 'new capture files are owner-only');
});

test('payload capture repairs/creates mode 0600, bounds huge values, and keeps first-three behavior', (t) => {
  const home = scratch(t);
  const file = path.join(home, 'hook-payloads.jsonl');
  writeFileSync(file, '');
  chmodSync(file, 0o644);

  const capture = createPayloadCapture(home, { enabled: true, maxPayloadBytes: 256 });
  for (let i = 0; i < 5; i++) capture('PostToolUse', { index: i, contents: 's'.repeat(2_000_000) });

  assert.equal(statSync(file).mode & 0o777, 0o600);
  const lines = readFileSync(file, 'utf8').trim().split('\n');
  assert.equal(lines.length, 3, 'only the first three records for an event are retained');
  assert.ok(Buffer.byteLength(lines[0]) < 2_000, 'the giant value was projected before line serialization');
  assert.match(JSON.parse(lines[0]).payload.contents, /\[truncated\]$/);
});

test('fleet-watch stops at its stdin byte ceiling and removes every stream listener', async (t) => {
  const home = scratch(t);
  const marker = path.join(home, 'stdin-cleanup.json');
  const preload = path.join(home, 'observe-stdin.cjs');
  writeFileSync(preload, `
    const fs = require('node:fs');
    const input = process.stdin;
    const removed = [];
    const remove = input.removeListener.bind(input);
    const pause = input.pause.bind(input);
    input.removeListener = (name, fn) => { removed.push(name); return remove(name, fn); };
    input.pause = () => {
      fs.writeFileSync(${JSON.stringify(marker)}, JSON.stringify(removed));
      return pause();
    };
  `);

  const child = spawn(process.execPath, [WATCH], {
    env: {
      ...process.env,
      NODE_OPTIONS: `--require=${preload}`,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  child.stdin.on('error', () => {}); // expected EPIPE when the capped reader exits
  child.stdin.write(`{"session_id":"sid","padding":"${'x'.repeat(70_000)}`);

  const result = await exitOf(child, 2500);
  assert.deepEqual(result, { code: 0, signal: null });
  assert.deepEqual(readFileSync(marker, 'utf8') && JSON.parse(readFileSync(marker, 'utf8')).sort(),
    ['data', 'end', 'error']);
});

test('fleet-watch timeout uses the same listener cleanup and pauses stdin', async (t) => {
  const home = scratch(t);
  const marker = path.join(home, 'stdin-timeout-cleanup.json');
  const preload = path.join(home, 'observe-timeout.cjs');
  writeFileSync(preload, `
    const fs = require('node:fs');
    const input = process.stdin;
    const removed = [];
    const on = input.on.bind(input);
    const remove = input.removeListener.bind(input);
    const pause = input.pause.bind(input);
    // This sandbox can error/EOF a child pipe immediately. Swallow that
    // platform event and withhold the watcher's end/error callbacks so the
    // production five-second timer is the path under test.
    on('error', () => {});
    input.on = (name, fn) => (name === 'end' || name === 'error') ? input : on(name, fn);
    input.removeListener = (name, fn) => { removed.push(name); return remove(name, fn); };
    input.pause = () => {
      fs.writeFileSync(${JSON.stringify(marker)}, JSON.stringify({ removed, paused: true }));
      return pause();
    };
  `);

  const child = spawn(process.execPath, [WATCH], {
    env: {
      ...process.env,
      NODE_OPTIONS: `--require=${preload}`,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
      FLEETDECK_WATCH_POLL_MS: '50',
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  child.stdin.on('error', () => {});
  child.stdin.write('{"session_id":"timeout-sid"}'); // deliberately no EOF

  assert.deepEqual(await exitOf(child, 8000), { code: 0, signal: null });
  const observed = JSON.parse(readFileSync(marker, 'utf8'));
  assert.equal(observed.paused, true);
  assert.deepEqual(observed.removed.sort(), ['data', 'end', 'error']);
});

test('SessionStart launcher repairs fleetd.log to 0600', async (t) => {
  const home = scratch(t);
  const log = path.join(home, 'fleetd.log');
  writeFileSync(log, 'legacy log\n');
  chmodSync(log, 0o644);
  const port = randomPort();

  const child = spawn(process.execPath, [SESSIONSTART], {
    env: {
      ...process.env,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(port),
      FLEETDECK_AGENTS_CMD: 'false',
      FLEETDECK_MDNS: 'off',
      FLEETDECK_TMUX_SOCKET: `fleetdeck-audit-${port}`,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  child.stdin.end(JSON.stringify({ session_id: `audit-${port}`, cwd: home }));
  assert.deepEqual(await exitOf(child), { code: 0, signal: null });
  assert.equal(statSync(log).mode & 0o777, 0o600);

  // The launcher intentionally detaches fleetd; own and retire the test copy.
  const pidFile = path.join(home, 'fleetd.pid');
  if (existsSync(pidFile)) {
    try {
      const rawPid = readFileSync(pidFile, 'utf8').trim();
      let pid = Number(rawPid); // legacy pidfile
      try { pid = JSON.parse(rawPid).pid; } catch { /* legacy format */ }
      process.kill(pid, 'SIGTERM');
    } catch { /* already gone */ }
  }
});

test('SessionStart silently absorbs an asynchronous spawn error', async (t) => {
  const home = scratch(t);
  const preload = path.join(home, 'fail-spawn.cjs');
  writeFileSync(preload, `
    const childProcess = require('node:child_process');
    const { EventEmitter } = require('node:events');
    childProcess.spawn = () => {
      const child = new EventEmitter();
      child.unref = () => {};
      process.nextTick(() => child.emit('error', Object.assign(new Error('synthetic EAGAIN'), { code: 'EAGAIN' })));
      return child;
    };
    require('node:module').syncBuiltinESMExports();
  `);

  const child = spawn(process.execPath, [SESSIONSTART], {
    env: {
      ...process.env,
      NODE_OPTIONS: `--require=${preload}`,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  child.stdin.end('{}');

  assert.deepEqual(await exitOf(child), { code: 0, signal: null });
  assert.equal(stderr, '', 'the command hook keeps its silent-failure contract');
});
