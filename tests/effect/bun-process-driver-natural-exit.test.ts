import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'bun:test';
import { waitUntil } from '../helpers/wait.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/bun-process-natural-close.ts', import.meta.url));

function readPid(file: string): number | null {
  try {
    const pid = Number(readFileSync(file, 'utf8').trim());
    return Number.isSafeInteger(pid) && pid > 1 ? pid : null;
  } catch {
    return null;
  }
}

function pidAlive(pid: number | null): boolean {
  if (pid === null) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function killPid(pid: number | null): void {
  if (pid === null) return;
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    // The fixture may already have completed its own cleanup.
  }
}

async function within<T>(promise: Promise<T>, label: string, timeoutMs: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out`)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

test('standalone driver close keeps cleanup alive until a TERM-immune group is reaped', async () => {
  if (process.platform === 'win32') return;

  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-bun-natural-close-'));
  const rootPidFile = path.join(scratch, 'root.pid');
  const descendantPidFile = path.join(scratch, 'descendant.pid');
  const closeMarker = path.join(scratch, 'closed');
  const owner = Bun.spawn(
    [process.execPath, FIXTURE, 'owner', rootPidFile, descendantPidFile, closeMarker],
    { stdin: 'ignore', stdout: 'pipe', stderr: 'pipe' },
  );

  let rootPid: number | null = null;
  let descendantPid: number | null = null;
  try {
    await waitUntil(
      () => {
        rootPid = readPid(rootPidFile);
        descendantPid = readPid(descendantPidFile);
        return rootPid !== null && descendantPid !== null;
      },
      {
        timeoutMs: 5_000,
        intervalMs: 5,
        label: 'standalone driver process-group identities',
      },
    );

    const exitCode = await within(owner.exited, 'standalone driver owner exit', 5_000);
    const [stdout, stderr] = await Promise.all([
      new Response(owner.stdout).text(),
      new Response(owner.stderr).text(),
    ]);
    assert.equal(exitCode, 0, stderr || stdout);
    assert.equal(
      existsSync(closeMarker),
      true,
      'the owner exited naturally before driver.close() resolved',
    );
    await waitUntil(() => !pidAlive(rootPid) && !pidAlive(descendantPid), {
      timeoutMs: 2_000,
      intervalMs: 10,
      label: 'standalone driver process group cleanup',
    });
  } finally {
    if (rootPid !== null) {
      try {
        process.kill(-rootPid, 'SIGKILL');
      } catch {
        // The process group is expected to be gone on the passing path.
      }
    }
    killPid(rootPid);
    killPid(descendantPid);
    if (owner.exitCode === null) owner.kill('SIGKILL');
    await within(owner.exited, 'standalone owner cleanup', 2_000).catch(() => undefined);
    await waitUntil(() => !pidAlive(rootPid) && !pidAlive(descendantPid), {
      timeoutMs: 2_000,
      intervalMs: 10,
      label: 'failed standalone fixture cleanup',
    }).catch(() => undefined);
    rmSync(scratch, { recursive: true, force: true });
  }
});
