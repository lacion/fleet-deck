import assert from 'node:assert/strict';
import type { ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'bun:test';
import { WebSocket } from 'ws';
import { DAEMON_SHUTDOWN_TIMEOUT_MS } from '../../src/daemon/app/root-program.ts';
import { openDb } from '../../src/daemon/db.ts';
import { decodeMessage } from '../../src/daemon/mdns.ts';
import { FLEETD_PATH, startDaemon, type DaemonHandle } from '../helpers/daemon.ts';
import { closeBoardClient, connectBoardClient, getJson, postHook } from '../helpers/http.ts';
import { getState } from '../helpers/state.ts';
import { scaleMs, waitUntil } from '../helpers/wait.ts';

const DAEMON_UNDER_TEST = process.env['FLEETDECK_TEST_DAEMON_SCRIPT'] ?? FLEETD_PATH;

interface QuestionState {
  readonly questions?: readonly {
    readonly session_id?: string;
    readonly kind?: string;
    readonly status?: string;
    readonly held?: boolean;
  }[];
}

interface MdnsRecord {
  readonly type?: string;
  readonly wire?: string;
}

interface ExitResult {
  readonly code: number | null;
  readonly signal: NodeJS.Signals | null;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} did not settle within ${effectiveTimeoutMs}ms`)),
          effectiveTimeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function childExit(child: ChildProcess): Promise<ExitResult> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

function pidIsLive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return !(
      typeof error === 'object' &&
      error !== null &&
      'code' in error &&
      error.code === 'ESRCH'
    );
  }
}

async function freePort(): Promise<number> {
  const server = http.createServer();
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
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

function isMdnsSendWithTtl(record: MdnsRecord, ttl: 0 | 'live'): boolean {
  if (record.type !== 'send' || !record.wire) return false;
  const packet = decodeMessage(Buffer.from(record.wire, 'base64'));
  if (!packet?.answers.length) return false;
  return ttl === 0
    ? packet.answers.every((answer) => answer.ttl === 0)
    : packet.answers.some((answer) => (answer.ttl ?? 0) > 0);
}

test('actual fleetd escalates repeated signals once without abandoning a held hook or ownership', async () => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p4-signal-escalation-'));
  const mdnsRecord = path.join(home, 'mdns.jsonl');
  const pidFile = path.join(home, 'fleetd.pid');
  const token = 'p4-signal-escalation-token-0123456789abcdef';
  const port = await freePort();
  let daemon: DaemonHandle | null = null;
  let board: WebSocket | null = null;

  try {
    daemon = await startDaemon({
      port,
      home,
      scriptPath: DAEMON_UNDER_TEST,
      env: {
        FLEETDECK_BIND: '0.0.0.0',
        FLEETDECK_HOLD_MS: '60000',
        FLEETDECK_HOLD_SCOPE: 'all',
        FLEETDECK_MDNS_RECORD: mdnsRecord,
        // The send is recorded synchronously, then its callback keeps the
        // withdrawing phase open long enough to deliver two distinct signals.
        FLEETDECK_MDNS_SEND_DELAY_MS: '200',
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
      { label: 'boot reconciliation to settle before signal escalation' },
    );
    await waitUntil(
      () => mdnsRecords(mdnsRecord).some((record) => isMdnsSendWithTtl(record, 'live')),
      { label: 'live mDNS announcement before signal escalation' },
    );

    board = await connectBoardClient(daemon.baseUrl, token);
    const boardClosed = new Promise<void>((resolve) => board?.once('close', () => resolve()));
    const sessionId = 'p4-signal-escalation-session';
    const started = await postHook(
      daemon.baseUrl,
      'SessionStart',
      { hook_event_name: 'SessionStart', session_id: sessionId, cwd: home },
      { token },
    );
    assert.equal(started.status, 200);

    let heldSettled = false;
    const held = postHook(
      daemon.baseUrl,
      'PermissionRequest',
      {
        hook_event_name: 'PermissionRequest',
        session_id: sessionId,
        cwd: home,
        tool_name: 'Bash',
        tool_input: { command: 'printf held-through-force' },
      },
      { token, timeout: 15_000 },
    ).finally(() => {
      heldSettled = true;
    });
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
      { label: 'PermissionRequest to be held before repeated signals' },
    );

    const pid = daemon.proc.pid;
    assert.ok(pid, 'spawned fleetd has a pid');
    const exited = childExit(daemon.proc);
    const shutdownStartedAt = performance.now();
    assert.equal(daemon.proc.kill('SIGTERM'), true, 'the first signal interrupts the root');

    const goodbye = await waitUntil(
      () => mdnsRecords(mdnsRecord).find((record) => isMdnsSendWithTtl(record, 0)) ?? null,
      {
        timeoutMs: 1_000,
        intervalMs: 5,
        label: 'TTL-0 withdrawal while the held route remains active',
      },
    );
    assert.equal(heldSettled, false, 'withdrawal precedes the held-route release phase');
    assert.equal(
      board.readyState,
      WebSocket.OPEN,
      'board client remains owned before close-clients',
    );
    assert.equal(daemon.proc.exitCode, null, 'the first signal is still running ordered cleanup');
    assert.equal(
      existsSync(pidFile),
      true,
      'pid ownership remains while earlier phases are active',
    );

    const secondSignalAt = performance.now();
    assert.equal(daemon.proc.kill('SIGTERM'), true, 'the second signal opens the force latch');
    await delay(10);
    assert.equal(daemon.proc.exitCode, null, 'the mDNS callback still owns the active phase');
    assert.equal(daemon.proc.kill('SIGINT'), true, 'a repeated signal is absorbed idempotently');

    const [response, result] = await Promise.all([
      within(held, 'held hook response after forced escalation'),
      within(exited, 'fleetd repeated-signal exit'),
      within(boardClosed, 'board websocket retirement'),
    ]).then(([hookResponse, exitResult]) => [hookResponse, exitResult] as const);
    const exitedAt = performance.now();
    const shutdownMs = exitedAt - shutdownStartedAt;
    const forcedMs = exitedAt - secondSignalAt;

    assert.equal(response.status, 200);
    assert.equal(response.text, '{}');
    assert.deepEqual(response.json, {});
    assert.deepEqual(result, { code: 0, signal: null }, daemon.stderr || daemon.stdout);
    assert.ok(
      shutdownMs < DAEMON_SHUTDOWN_TIMEOUT_MS,
      `repeated-signal cleanup (${shutdownMs.toFixed(1)}ms) must beat the ${DAEMON_SHUTDOWN_TIMEOUT_MS}ms root deadline`,
    );
    assert.ok(
      forcedMs < DAEMON_SHUTDOWN_TIMEOUT_MS - 500,
      `second-signal escalation (${forcedMs.toFixed(1)}ms) must finish before deadline-reserve escalation could substitute for it`,
    );

    const goodbyes = mdnsRecords(mdnsRecord).filter((record) => isMdnsSendWithTtl(record, 0));
    assert.equal(goodbyes.length, 1, 'repeated signals emit one observable mDNS finalizer');
    assert.equal(
      mdnsRecords(mdnsRecord).filter(
        (record) => record.type === 'callback' && record.wire === goodbye.wire,
      ).length,
      1,
      'the one goodbye send completes exactly once',
    );
    assert.equal(existsSync(pidFile), false, 'the exact owned pidfile is released at the end');
    assert.equal(pidIsLive(pid), false, 'the spawned fleetd child leaves no live process');

    await assert.rejects(
      fetch(daemon.baseUrl + '/health', { signal: AbortSignal.timeout(scaleMs(500)) }),
      'the listener no longer accepts connections',
    );
    await provePortCanRebind(port);

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
  } finally {
    if (board) await closeBoardClient(board);
    await daemon?.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}, 20_000);
