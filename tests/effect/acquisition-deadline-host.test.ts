import assert from 'node:assert/strict';
import { type ChildProcess, spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'bun:test';
import { DAEMON_SHUTDOWN_TIMEOUT_MS } from '../../src/daemon/app/root-program.ts';
import { scaleMs, waitUntil } from '../helpers/wait.ts';

const FIXTURE = fileURLToPath(
  new URL('./fixtures/p4-acquisition-deadline-host.ts', import.meta.url),
);

interface ReadyObservation {
  readonly event: 'ready';
  readonly pid: number;
  readonly port: number;
  readonly acquisitionShutdownTimeoutMs: number;
}

interface FixtureEvent {
  readonly event: string;
  readonly at: number;
  readonly tag?: string;
  readonly code?: number;
  readonly pidExists?: boolean;
  readonly listeners?: {
    readonly sigint: number;
    readonly sigterm: number;
    readonly unhandledRejection: number;
  };
}

interface ChildPids {
  readonly pid: number;
  readonly descendantPid: number;
}

interface ExitResult {
  readonly code: number | null;
  readonly signal: NodeJS.Signals | null;
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 4_000): Promise<T> {
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

function childExit(child: ChildProcess): Promise<ExitResult> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

function events(file: string): FixtureEvent[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line): FixtureEvent => JSON.parse(line) as FixtureEvent);
}

function terminateOwnedPid(pid: number | undefined): void {
  if (!pid || !pidIsLive(pid)) return;
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    // Exact test-owned cleanup only; ESRCH is already the desired state.
  }
}

test('signal during post-bind acquisition exits within 1750ms with no owned residue', async () => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p4-acquisition-deadline-'));
  const pidFile = path.join(home, 'fleetd.pid');
  const childPidFile = path.join(home, 'child-pids.json');
  const eventFile = path.join(home, 'events.jsonl');
  const databaseFile = path.join(home, 'fleetd.db');
  const tokenFile = path.join(home, 'token');
  const port = await freePort();
  const child = spawn(
    process.execPath,
    ['--no-env-file', FIXTURE, String(port), pidFile, childPidFile, eventFile],
    { stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let stdout = '';
  let stderr = '';
  let readySettled = false;
  let resolveReady: (observation: ReadyObservation) => void = () => undefined;
  let rejectReady: (cause: unknown) => void = () => undefined;
  const ready = new Promise<ReadyObservation>((resolve, reject) => {
    resolveReady = resolve;
    rejectReady = reject;
  });
  child.stdout?.on('data', (chunk: Buffer) => {
    stdout += chunk.toString();
    for (const line of stdout.split('\n')) {
      if (!line || readySettled) continue;
      try {
        const parsed = JSON.parse(line) as ReadyObservation;
        if (parsed.event !== 'ready') continue;
        readySettled = true;
        resolveReady(parsed);
      } catch {
        // Retain partial/non-JSON stdout for the eventual diagnostic.
      }
    }
  });
  child.stderr?.on('data', (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  const exited = childExit(child);
  void exited.then((result) => {
    if (readySettled) return;
    readySettled = true;
    rejectReady(
      new Error(
        `acquisition fixture exited before ready (${String(result.code)}/${String(result.signal)}): ${stderr || stdout}`,
      ),
    );
  });

  let ownedPids: ChildPids | null = null;
  try {
    const observation = await within(ready, 'post-bind acquisition readiness');
    assert.deepEqual(
      {
        pid: observation.pid,
        port: observation.port,
        acquisitionShutdownTimeoutMs: observation.acquisitionShutdownTimeoutMs,
      },
      {
        pid: child.pid,
        port,
        acquisitionShutdownTimeoutMs: DAEMON_SHUTDOWN_TIMEOUT_MS,
      },
    );
    assert.equal((await fetch(`http://127.0.0.1:${port}`)).status, 200);
    ownedPids = JSON.parse(readFileSync(childPidFile, 'utf8')) as ChildPids;
    assert.equal(pidIsLive(ownedPids.pid), true, 'the acquisition child is active before signal');
    assert.equal(
      pidIsLive(ownedPids.descendantPid),
      true,
      'the acquisition child process group is active before signal',
    );
    assert.equal(existsSync(pidFile), true, 'the host owns its pid marker before signal');
    assert.equal(existsSync(databaseFile), false);
    assert.equal(existsSync(tokenFile), false);

    const signalledAt = performance.now();
    assert.equal(child.kill('SIGTERM'), true);
    await waitUntil(() => events(eventFile).some((item) => item.event === 'acquisition-abort'), {
      timeoutMs: 500,
      intervalMs: 2,
      label: 'acquisition AbortSignal observation',
    });
    await Bun.sleep(25);
    assert.equal(child.exitCode, null, 'the bounded cleanup finalizer still owns the root');
    assert.equal(
      existsSync(pidFile),
      true,
      'pid ownership is retained until the actual host-exit boundary',
    );

    await waitUntil(
      () => !pidIsLive(ownedPids?.pid ?? -1) && !pidIsLive(ownedPids?.descendantPid ?? -1),
      {
        timeoutMs: 1_000,
        intervalMs: 5,
        label: 'forced acquisition child process-group reap',
      },
    );
    const result = await within(exited, 'post-bind acquisition shutdown', 2_500);
    const shutdownMs = performance.now() - signalledAt;

    assert.deepEqual(result, { code: 1, signal: null }, stderr || stdout);
    assert.ok(
      shutdownMs <= scaleMs(DAEMON_SHUTDOWN_TIMEOUT_MS),
      `post-bind acquisition shutdown took ${shutdownMs.toFixed(1)}ms`,
    );
    assert.equal(existsSync(pidFile), false, 'host exit removed the exact owned pid marker');
    assert.equal(pidIsLive(ownedPids.pid), false);
    assert.equal(pidIsLive(ownedPids.descendantPid), false);
    assert.equal(existsSync(databaseFile), false, 'no database state appeared after close');
    assert.equal(existsSync(tokenFile), false, 'no token state appeared after close');
    await provePortCanRebind(port);

    const recorded = events(eventFile);
    assert.deepEqual(
      recorded
        .filter((item) => item.event === 'acquisition-shutdown-failure')
        .map((item) => item.tag),
      ['TimedOut'],
    );
    assert.equal(recorded.filter((item) => item.event === 'host-process-release').length, 1);
    const teardownIndex = recorded.findIndex((item) => item.event === 'host-teardown');
    const releaseIndex = recorded.findIndex((item) => item.event === 'host-process-release');
    assert.ok(teardownIndex >= 0 && releaseIndex > teardownIndex);
    assert.equal(recorded[teardownIndex]?.code, 1);
    const exitObservation = recorded.find((item) => item.event === 'host-exit-observation');
    assert.equal(exitObservation?.pidExists, false);
    assert.deepEqual(exitObservation?.listeners, {
      sigint: 0,
      sigterm: 0,
      unhandledRejection: 0,
    });
    assert.doesNotMatch(`${stdout}\n${stderr}`, /unhandled|SQLITE_MISUSE|shutdown error/i);
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    await Promise.allSettled([exited]);
    terminateOwnedPid(ownedPids?.pid);
    terminateOwnedPid(ownedPids?.descendantPid);
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
});
