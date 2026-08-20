import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'bun:test';
import { scaleMs } from '../helpers/wait.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/p4-hard-deadline-host.ts', import.meta.url));

interface FixtureEvent {
  readonly event: string;
}

interface Observation {
  readonly event: string;
  readonly pid?: number;
  readonly port?: number;
  readonly timeoutMs?: number;
  readonly deadlineExpired?: boolean;
  readonly elapsedMs?: number;
  readonly phases?: readonly { readonly phase: string; readonly tag: string }[];
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 3_000): Promise<T> {
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

function observations(stdout: string): Observation[] {
  return stdout
    .split('\n')
    .filter(Boolean)
    .map((line): Observation => JSON.parse(line) as Observation);
}

function events(file: string): FixtureEvent[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line): FixtureEvent => JSON.parse(line) as FixtureEvent);
}

test('hard deadline exits one BunRuntime root nonzero and releases process ownership at teardown', async () => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p4-hard-deadline-'));
  const pidFile = path.join(home, 'fleetd.pid');
  const eventFile = path.join(home, 'events.jsonl');
  const port = await freePort();
  const child = Bun.spawn(
    [process.execPath, '--no-env-file', FIXTURE, String(port), pidFile, eventFile],
    {
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    },
  );
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();

  try {
    const exitCode = await within(child.exited, 'hard-deadline fixture exit');
    const [out, err] = await within(
      Promise.all([stdout, stderr]),
      'hard-deadline fixture pipe drain',
    );
    const items = observations(out);
    const ready = items.find((item) => item.event === 'ready');
    const outcome = items.find((item) => item.event === 'outcome');

    assert.equal(exitCode, 1, err || out);
    assert.deepEqual(
      { pid: ready?.pid, port: ready?.port },
      { pid: child.pid, port },
      'fixture acquired the exact process and listener under test',
    );
    assert.equal(outcome?.deadlineExpired, true);
    assert.ok(
      (outcome?.elapsedMs ?? Number.POSITIVE_INFINITY) >= (ready?.timeoutMs ?? 0),
      'the absolute deadline, rather than an early force notification, discharges the stuck join',
    );
    assert.ok(
      (outcome?.elapsedMs ?? Number.POSITIVE_INFINITY) < 1_000,
      `the ${String(ready?.timeoutMs)}ms deadline remained bounded (${String(outcome?.elapsedMs)}ms)`,
    );
    assert.deepEqual(outcome?.phases?.slice(-3), [
      { phase: 'closing-http', tag: 'TimedOut' },
      { phase: 'closing-store', tag: 'Skipped' },
      { phase: 'releasing-process', tag: 'TimedOut' },
    ]);

    const recorded = events(eventFile).map((item) => item.event);
    assert.equal(recorded.filter((event) => event === 'force-stop-start').length, 1);
    assert.equal(recorded.includes('unexpected-store-close'), false);
    assert.equal(recorded.includes('unexpected-http-close'), false);
    assert.equal(
      recorded.filter((event) => event === 'host-process-release').length,
      1,
      'the shared release fallback runs exactly once',
    );
    assert.equal(
      recorded.at(-1),
      'host-process-release',
      'pid ownership is the final observable release at host teardown',
    );
    assert.equal(existsSync(pidFile), false, 'the teardown fallback removes the owned marker');
    await provePortCanRebind(port);
    assert.equal(err, '');
  } finally {
    if (child.exitCode === null) child.kill('SIGKILL');
    await Promise.allSettled([child.exited, stdout, stderr]);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
});
