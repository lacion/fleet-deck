import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'bun:test';
import { scaleMs, waitUntil } from '../helpers/wait.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/process-force-root.ts', import.meta.url));
const ROOT_TIMEOUT_MS = 1_750;

interface Observation {
  readonly event: string;
  readonly descendantPid?: number;
  readonly timeoutMs?: number;
  readonly deadlineExpired?: boolean;
  readonly elapsedMs?: number;
  readonly driverSettledAt?: number;
  readonly postDeadlineJoinMs?: number;
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs = 4_000): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${String(scaleMs(timeoutMs))}ms`)),
          scaleMs(timeoutMs),
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function forceKill(pid: number | null): void {
  if (pid === null) return;
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    // Already reaped.
  }
}

function observations(stdout: string): Observation[] {
  return stdout
    .split('\n')
    .filter(Boolean)
    .map((line): Observation => JSON.parse(line) as Observation);
}

test('root force reserve SIGKILLs and joins a TERM-immune process group before 1750ms', async () => {
  if (process.platform === 'win32') return;
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-process-force-root-'));
  const pidFile = path.join(scratch, 'descendant.pid');
  const child = Bun.spawn([process.execPath, '--no-env-file', FIXTURE, pidFile], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();
  let descendantPid: number | null = null;

  try {
    const [exitCode, out, err] = await within(
      Promise.all([child.exited, stdout, stderr]),
      'process-force root fixture',
    );
    const items = observations(out);
    const ready = items.find((item) => item.event === 'ready');
    const settled = items.find((item) => item.event === 'driver-settled');
    const outcome = items.find((item) => item.event === 'outcome');
    const finalized = items.find((item) => item.event === 'root-finalized');
    descendantPid = ready?.descendantPid ?? null;

    assert.equal(exitCode, 0, err || out);
    assert.equal(ready?.timeoutMs, ROOT_TIMEOUT_MS);
    assert.equal(outcome?.deadlineExpired, true);
    assert.ok(
      (settled?.elapsedMs ?? Number.POSITIVE_INFINITY) < ROOT_TIMEOUT_MS,
      `driver ownership must join before the root deadline (${String(settled?.elapsedMs)}ms)`,
    );
    assert.equal(outcome?.driverSettledAt, settled?.elapsedMs);
    assert.ok(
      (outcome?.postDeadlineJoinMs ?? Number.POSITIVE_INFINITY) < scaleMs(50),
      `Layer fallback must see the same settled close (${String(outcome?.postDeadlineJoinMs)}ms)`,
    );
    assert.ok(
      (finalized?.elapsedMs ?? Number.POSITIVE_INFINITY) < scaleMs(ROOT_TIMEOUT_MS + 250),
      `root Layer finalization exceeded its bounded handoff (${String(finalized?.elapsedMs)}ms)`,
    );
    assert.ok(descendantPid !== null, 'fixture published the TERM-immune descendant pid');
    await waitUntil(() => !pidAlive(descendantPid as number), {
      timeoutMs: 500,
      intervalMs: 10,
      label: 'forced process-group descendant reaped',
    });
    assert.equal(err, '');
  } finally {
    if (child.exitCode === null) child.kill('SIGKILL');
    await Promise.allSettled([child.exited, stdout, stderr]);
    forceKill(descendantPid);
    try {
      forceKill(Number(readFileSync(pidFile, 'utf8')));
    } catch {
      // The descendant may not have reached its pid-file publication point.
    }
    rmSync(scratch, { recursive: true, force: true });
  }
});
