import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { ChildProcess } from 'node:child_process';
import { openDb } from '../src/daemon/db.ts';
import { decodeMessage } from '../src/daemon/mdns.ts';
import { startDaemon, type DaemonHandle } from './helpers/daemon.ts';
import test from './helpers/harness-test.ts';
import { getJson, postHook } from './helpers/http.ts';
import { getState } from './helpers/state.ts';
import { scaleMs, waitUntil } from './helpers/wait.ts';

interface QuestionState {
  questions?: Array<{
    session_id?: string;
    kind?: string;
    status?: string;
    held?: boolean;
  }>;
}

interface MdnsRecord {
  type?: string;
  wire?: string;
}

interface ExitResult {
  code: number | null;
  signal: NodeJS.Signals | null;
}

function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`${label} did not settle within ${scaleMs(timeoutMs)}ms`)),
      scaleMs(timeoutMs),
    );
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function childExit(child: ChildProcess): Promise<ExitResult> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

async function freePort(): Promise<number> {
  const server = http.createServer();
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '0.0.0.0', resolve);
  });
  const port = (server.address() as AddressInfo).port;
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
  return port;
}

async function provePortCanRebind(port: number): Promise<void> {
  const server = http.createServer((_request, response) => response.end('ok'));
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '0.0.0.0', resolve);
  });
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
}

function mdnsRecords(file: string): MdnsRecord[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line): MdnsRecord => JSON.parse(line) as MdnsRecord);
}

function hasMdnsPacket(file: string, ttl: 0 | 'live'): boolean {
  for (const item of mdnsRecords(file)) {
    if (item.type !== 'send' || !item.wire) continue;
    const packet = decodeMessage(Buffer.from(item.wire, 'base64'));
    if (!packet?.answers.length) continue;
    if (ttl === 0 && packet.answers.every((answer) => answer.ttl === 0)) return true;
    if (ttl === 'live' && packet.answers.some((answer) => (answer.ttl ?? 0) > 0)) return true;
  }
  return false;
}

test('actual fleetd SIGTERM drains a held hook, closes storage/listener, and releases ownership', {
  timeout: 20_000,
}, async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-daemon-lifecycle-'));
  const mdnsRecord = path.join(home, 'mdns.jsonl');
  const pidFile = path.join(home, 'fleetd.pid');
  const token = 'daemon-lifecycle-token-0123456789abcdef';
  const port = await freePort();
  let daemon: DaemonHandle | null = null;

  t.after(async () => {
    await daemon?.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  daemon = await startDaemon({
    port,
    home,
    env: {
      FLEETDECK_BIND: '0.0.0.0',
      FLEETDECK_HOLD_MS: '60000',
      FLEETDECK_HOLD_SCOPE: 'all',
      FLEETDECK_MDNS_RECORD: mdnsRecord,
      FLEETDECK_MDNS_SEND_DELAY_MS: '150',
      FLEETDECK_TEST_NET_MOCK: '1',
      FLEETDECK_TOKEN: token,
    },
  });

  await waitUntil(
    async () => {
      const health = await getJson(`${daemon?.baseUrl}/health`, { token });
      const body = health.json as { startup?: string } | null;
      return health.status === 200 && body?.startup === 'settled';
    },
    { label: 'boot reconciliation to settle before shutdown' },
  );
  await waitUntil(() => hasMdnsPacket(mdnsRecord, 'live'), {
    label: 'live mDNS announcement before shutdown',
  });

  const sessionId = 'daemon-lifecycle-session';
  const started = await postHook(
    daemon.baseUrl,
    'SessionStart',
    { hook_event_name: 'SessionStart', session_id: sessionId, cwd: home },
    { token },
  );
  assert.equal(started.status, 200);

  const held = postHook(
    daemon.baseUrl,
    'PermissionRequest',
    {
      hook_event_name: 'PermissionRequest',
      session_id: sessionId,
      cwd: home,
      tool_name: 'Bash',
      tool_input: { command: 'printf held' },
    },
    { token, boardClient: true, timeout: 15_000 },
  );
  await waitUntil(
    async () => {
      const state = await getState<QuestionState>(daemon?.baseUrl ?? '', { token });
      return state.questions?.some(
        (question) =>
          question.session_id === sessionId &&
          question.kind === 'permission' &&
          question.status === 'pending' &&
          question.held === true,
      );
    },
    { label: 'PermissionRequest to be held by the real daemon' },
  );

  const exited = childExit(daemon.proc);
  const shutdownStartedAt = performance.now();
  assert.equal(daemon.proc.kill('SIGTERM'), true);
  const [response, result] = await Promise.all([
    within(held, 'held hook release'),
    within(exited, 'fleetd SIGTERM exit'),
  ]);
  const shutdownMs = performance.now() - shutdownStartedAt;

  assert.equal(response.status, 200);
  assert.equal(response.text, '{}');
  assert.deepEqual(response.json, {});
  assert.deepEqual(result, { code: 0, signal: null }, daemon.stderr || daemon.stdout);
  assert.ok(
    shutdownMs < 1_000,
    `clean shutdown (${shutdownMs.toFixed(1)}ms) must beat the existing 1s watchdog`,
  );
  assert.equal(existsSync(pidFile), false, 'the exact owned pidfile is removed last');
  assert.equal(hasMdnsPacket(mdnsRecord, 0), true, 'mDNS emitted its TTL-0 goodbye');

  await assert.rejects(
    fetch(`${daemon.baseUrl}/health`, { signal: AbortSignal.timeout(scaleMs(500)) }),
    'the HTTP listener no longer accepts connections',
  );
  await provePortCanRebind(port);

  // releaseAll persists retirement synchronously while SQLite is still open;
  // reopening after process exit proves that write preceded store disposal.
  const db = openDb(path.join(home, 'fleetd.db'));
  try {
    const row = db
      .prepare('SELECT status FROM questions WHERE session_id = ? AND kind = ?')
      .get(sessionId, 'permission') as { status?: string } | null;
    assert.equal(row?.status, 'expired');
  } finally {
    db.close();
  }

  const output = `${daemon.stdout}\n${daemon.stderr}`;
  assert.doesNotMatch(output, /shutdown error|database (?:is )?closed|SQLITE_MISUSE|unhandled/i);
  assert.doesNotMatch(output, /shutdown timed out waiting for discovery/);
});
