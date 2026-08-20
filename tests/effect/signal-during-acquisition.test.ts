import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'bun:test';
import { FLEETD_PATH, spawnRaw, type RawDaemon } from '../helpers/daemon.ts';
import { scaleMs, waitUntil } from '../helpers/wait.ts';

const INCUMBENT = fileURLToPath(
  new URL('./fixtures/acquisition-incumbent/fleetd.ts', import.meta.url),
);
const DAEMON_UNDER_TEST = process.env['FLEETDECK_TEST_DAEMON_SCRIPT'] ?? FLEETD_PATH;

interface FixtureEvent {
  readonly event: string;
}

function events(file: string): FixtureEvent[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line): FixtureEvent => JSON.parse(line) as FixtureEvent);
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

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${effectiveTimeoutMs}ms`)),
          effectiveTimeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
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
    server.listen(port, '127.0.0.1', resolve);
  });
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
}

test('signal during real fleetd Layer acquisition exits cleanly without claiming partial ownership', async () => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p4-acquisition-signal-'));
  const incumbentEvents = path.join(home, 'incumbent.jsonl');
  const pidFile = path.join(home, 'fleetd.pid');
  const incumbentPort = await freePort();
  const challengerPort = await freePort();
  const incumbent = Bun.spawn(
    [process.execPath, '--no-env-file', INCUMBENT, String(incumbentPort), incumbentEvents],
    { stdin: 'ignore', stdout: 'pipe', stderr: 'pipe' },
  );
  const incumbentStdout = new Response(incumbent.stdout).text();
  const incumbentStderr = new Response(incumbent.stderr).text();
  let challenger: RawDaemon | null = null;

  try {
    await waitUntil(() => events(incumbentEvents).some((item) => item.event === 'ready'), {
      label: 'fleetd-shaped incumbent readiness',
    });
    assert.ok(incumbent.pid, 'incumbent has a pid');
    const incumbentRecord = JSON.stringify({ pid: incumbent.pid, port: incumbentPort });
    writeFileSync(pidFile, incumbentRecord);

    challenger = spawnRaw({ port: challengerPort, home, scriptPath: DAEMON_UNDER_TEST });
    assert.ok(challenger.proc.pid, 'challenger has a pid');
    await waitUntil(() => events(incumbentEvents).some((item) => item.event === 'health-request'), {
      timeoutMs: 3_000,
      intervalMs: 10,
      label: 'challenger takeover probe inside Layer acquisition',
    });
    assert.equal(challenger.proc.exitCode, null);
    assert.equal(readFileSync(pidFile, 'utf8'), incumbentRecord);

    const signalledAt = performance.now();
    assert.equal(challenger.proc.kill('SIGTERM'), true);
    const exitCode = await within(
      challenger.waitForExit(scaleMs(4_000)),
      'signalled acquisition exit',
    );
    const shutdownMs = performance.now() - signalledAt;

    assert.equal(exitCode, 0, challenger.stderr || challenger.stdout);
    assert.ok(
      shutdownMs < scaleMs(2_500),
      `the bounded incumbent probe and acquisition canceler took ${shutdownMs.toFixed(1)}ms`,
    );
    assert.equal(pidIsLive(challenger.proc.pid ?? -1), false);
    assert.equal(
      pidIsLive(incumbent.pid),
      true,
      'interrupted acquisition never evicts the incumbent',
    );
    assert.equal(
      readFileSync(pidFile, 'utf8'),
      incumbentRecord,
      'interrupted acquisition never removes ownership it did not acquire',
    );
    assert.equal(existsSync(path.join(home, 'fleetd.db')), false);
    assert.equal(existsSync(path.join(home, 'token')), false);
    await provePortCanRebind(challengerPort);
    assert.doesNotMatch(
      `${challenger.stdout}\n${challenger.stderr}`,
      /shutdown error|unhandled|SQLITE_MISUSE/i,
    );
  } finally {
    await challenger?.kill();
    if (incumbent.exitCode === null) incumbent.kill('SIGTERM');
    await Promise.allSettled([
      within(incumbent.exited, 'incumbent cleanup'),
      incumbentStdout,
      incumbentStderr,
    ]);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
});
