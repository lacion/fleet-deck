// tests/audit-hardening.test.ts — regression coverage for the audit's
// local diagnostic/launcher resource and permission boundaries.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';
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
import { createPayloadCapture } from '../scripts/fleetd/payload-capture.ts';
import { randomPort } from './helpers/daemon.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const WATCH = path.join(REPO_ROOT, 'scripts/fleet-watch.mjs');
const SESSIONSTART = path.join(REPO_ROOT, 'scripts/fleet-sessionstart.mjs');

interface ExitResult {
  code: number | null;
  signal: NodeJS.Signals | null;
}
interface CapturePayload {
  prompt?: string;
  contents?: string;
  index?: number;
}
interface CaptureRecord {
  payload: CapturePayload;
}
interface WatchObservation {
  paused?: boolean;
  removed: string[];
}

// process._getActiveHandles is an undocumented internal, absent from @types/node.
function activeHandleCount(): number {
  return (process as unknown as { _getActiveHandles: () => unknown[] })._getActiveHandles().length;
}

function scratch(t: TestContext, prefix = 'fleetdeck-audit-'): string {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

function exitOf(child: EventEmitter, timeoutMs = 6000): Promise<ExitResult> {
  return new Promise<ExitResult>((resolve, reject) => {
    const onExit = (code: number | null, signal: NodeJS.Signals | null): void => {
      clearTimeout(timer);
      resolve({ code, signal });
    };
    const timer = setTimeout(() => {
      child.removeListener('exit', onExit);
      reject(new Error('child did not exit in time'));
    }, timeoutMs);
    child.once('exit', onExit);
  });
}

test('exitOf clears its timeout when the child exits first and drops the listener when the timeout wins', async () => {
  const handlesBefore = activeHandleCount();

  // Exit path: a settled child must not leave its timeout timer referenced.
  const fast = new EventEmitter();
  const fastWait = exitOf(fast, 60_000);
  fast.emit('exit', 0, null);
  assert.deepEqual(await fastWait, { code: 0, signal: null });
  assert.ok(
    activeHandleCount() <= handlesBefore,
    'a prompt child exit must not leave the timeout timer referenced',
  );

  // Timeout path: the loser must drop its exit listener so the child can be GC'd.
  const slow = new EventEmitter();
  await assert.rejects(exitOf(slow, 25), /did not exit in time/);
  assert.equal(slow.listenerCount('exit'), 0, 'the timed-out wait must remove its exit listener');
});

test('payload capture is off by default and enabled only by the explicit on flag', (t) => {
  const home = scratch(t);
  const file = path.join(home, 'hook-payloads.jsonl');
  const previous = process.env['FLEETDECK_CAPTURE_PAYLOADS'];
  t.after(() => {
    if (previous === undefined) delete process.env['FLEETDECK_CAPTURE_PAYLOADS'];
    else process.env['FLEETDECK_CAPTURE_PAYLOADS'] = previous;
  });

  delete process.env['FLEETDECK_CAPTURE_PAYLOADS'];
  createPayloadCapture(home)('Stop', { prompt: 'must not persist' });
  assert.equal(existsSync(file), false, 'default capture must not even create the file');

  process.env['FLEETDECK_CAPTURE_PAYLOADS'] = 'on';
  createPayloadCapture(home)('Stop', { prompt: 'diagnostic' });
  assert.equal(
    (JSON.parse(readFileSync(file, 'utf8')) as CaptureRecord).payload.prompt,
    'diagnostic',
  );
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
  const [first] = lines;
  assert.ok(first !== undefined, 'the first retained record must exist');
  assert.ok(
    Buffer.byteLength(first) < 2_000,
    'the giant value was projected before line serialization',
  );
  assert.match((JSON.parse(first) as CaptureRecord).payload.contents ?? '', /\[truncated\]$/);
});

test('fleet-watch stops at its stdin byte ceiling and removes every stream listener', async (t) => {
  const home = scratch(t);
  const marker = path.join(home, 'stdin-cleanup.json');
  const preload = path.join(home, 'observe-stdin.cjs');
  writeFileSync(
    preload,
    `
    const fs = require('node:fs');
    const input = process.stdin;
    const removed = [];
    const remove = input.removeListener.bind(input);
    const pause = input.pause.bind(input);
    input.removeListener = (name, fn) => {
      // Record only the named listeners fleet-watch manages. Under bun, stdin
      // fires an internal once-listener whose onceWrapper calls removeListener
      // with an undefined event name (node does not); filtering to strings
      // ignores that runtime housekeeping without masking any real removal.
      if (typeof name === 'string') removed.push(name);
      return remove(name, fn);
    };
    input.pause = () => {
      fs.writeFileSync(${JSON.stringify(marker)}, JSON.stringify(removed));
      return pause();
    };
  `,
  );

  // fleet-watch is a plain .mjs bundle; inject the stdin-observing preload as a
  // --require CLI arg, not via NODE_OPTIONS. Both node and bun honor --require on
  // the command line (bun ignores NODE_OPTIONS=--require entirely), and both
  // expose the same process.stdin singleton the preload patches.
  const child = spawn(process.execPath, ['--require', preload, WATCH], {
    env: {
      ...process.env,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const stdin = child.stdin;
  assert.ok(stdin, 'child stdin must be piped');
  stdin.on('error', () => {
    /* expected EPIPE when the capped reader exits */
  });
  stdin.write(`{"session_id":"sid","padding":"${'x'.repeat(70_000)}`);

  const result = await exitOf(child, 2500);
  assert.deepEqual(result, { code: 0, signal: null });
  const raw = readFileSync(marker, 'utf8');
  assert.deepEqual(raw && (JSON.parse(raw) as string[]).sort(), ['data', 'end', 'error']);
});

test('fleet-watch timeout uses the same listener cleanup and pauses stdin', async (t) => {
  const home = scratch(t);
  const marker = path.join(home, 'stdin-timeout-cleanup.json');
  const preload = path.join(home, 'observe-timeout.cjs');
  writeFileSync(
    preload,
    `
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
    // Filter to string names: bun's stdin housekeeping calls removeListener with
    // an undefined event name (see the byte-ceiling test for the full rationale).
    input.removeListener = (name, fn) => {
      if (typeof name === 'string') removed.push(name);
      return remove(name, fn);
    };
    input.pause = () => {
      fs.writeFileSync(${JSON.stringify(marker)}, JSON.stringify({ removed, paused: true }));
      return pause();
    };
  `,
  );

  // --require CLI arg (not NODE_OPTIONS) so the preload loads under bun too.
  const child = spawn(process.execPath, ['--require', preload, WATCH], {
    env: {
      ...process.env,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
      FLEETDECK_WATCH_POLL_MS: '50',
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const stdin = child.stdin;
  assert.ok(stdin, 'child stdin must be piped');
  stdin.on('error', () => {
    /* sandbox may EPIPE the child pipe immediately */
  });
  stdin.write('{"session_id":"timeout-sid"}'); // deliberately no EOF

  assert.deepEqual(await exitOf(child, 8000), { code: 0, signal: null });
  const observed = JSON.parse(readFileSync(marker, 'utf8')) as WatchObservation;
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
  const stdin = child.stdin;
  assert.ok(stdin, 'child stdin must be piped');
  stdin.end(JSON.stringify({ session_id: `audit-${port}`, cwd: home }));
  assert.deepEqual(await exitOf(child), { code: 0, signal: null });
  assert.equal(statSync(log).mode & 0o777, 0o600);

  // The launcher intentionally detaches fleetd; own and retire the test copy.
  const pidFile = path.join(home, 'fleetd.pid');
  if (existsSync(pidFile)) {
    try {
      const rawPid = readFileSync(pidFile, 'utf8').trim();
      let pid = Number(rawPid); // legacy pidfile
      try {
        pid = (JSON.parse(rawPid) as { pid: number }).pid;
      } catch {
        /* legacy format */
      }
      process.kill(pid, 'SIGTERM');
    } catch {
      /* already gone */
    }
  }
});

test('SessionStart silently absorbs an asynchronous spawn error', async (t) => {
  const home = scratch(t);
  const preload = path.join(home, 'fail-spawn.cjs');
  writeFileSync(
    preload,
    `
    const childProcess = require('node:child_process');
    const { EventEmitter } = require('node:events');
    childProcess.spawn = () => {
      const child = new EventEmitter();
      child.unref = () => {};
      process.nextTick(() => child.emit('error', Object.assign(new Error('synthetic EAGAIN'), { code: 'EAGAIN' })));
      return child;
    };
    require('node:module').syncBuiltinESMExports();
  `,
  );

  // --require CLI arg (not NODE_OPTIONS): under bun a NODE_OPTIONS=--require
  // preload is silently ignored, so spawn would NOT be stubbed and the launcher
  // would spawn a real detached fleetd — leaking a daemon and defeating the
  // test's whole point. On the command line both runtimes load the stub.
  const child = spawn(process.execPath, ['--require', preload, SESSIONSTART], {
    env: {
      ...process.env,
      FLEETDECK_HOME: home,
      FLEETDECK_PORT: String(randomPort()),
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stderr = '';
  const childStderr = child.stderr;
  assert.ok(childStderr, 'child stderr must be piped');
  childStderr.on('data', (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  const stdin = child.stdin;
  assert.ok(stdin, 'child stdin must be piped');
  stdin.end('{}');

  assert.deepEqual(await exitOf(child), { code: 0, signal: null });
  assert.equal(stderr, '', 'the command hook keeps its silent-failure contract');
});
