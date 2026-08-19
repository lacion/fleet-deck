import assert from 'node:assert/strict';
import type { ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../src/daemon/db.ts';
import { startDaemon, type DaemonHandle } from './helpers/daemon.ts';
import test from './helpers/harness-test.ts';
import { scaleMs, waitUntil } from './helpers/wait.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const POLL_FIXTURE = path.join(HERE, 'helpers/agents-poll-shutdown-fixture.ts');
const SESSION_ID = 'p1-in-flight-agents-poll';

interface ExitResult {
  code: number | null;
  signal: NodeJS.Signals | null;
}

interface SessionRow {
  col: string;
  note: string | null;
  ended_at: number | null;
  end_reason: string | null;
}

function childExit(child: ChildProcess): Promise<ExitResult> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

function within<T>(promise: Promise<T>, label: string, timeoutMs = 8_000): Promise<T> {
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

function readFixturePid(file: string): number | null {
  if (!existsSync(file)) return null;
  const pid = Number(readFileSync(file, 'utf8').trim());
  return Number.isSafeInteger(pid) && pid > 0 ? pid : null;
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

test('P3 real SIGTERM cancels and joins an in-flight agents poll without delivering its result', {
  timeout: 20_000,
}, async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p1-agents-shutdown-'));
  const startedFile = path.join(home, 'agents-poll-started');
  const releaseFile = path.join(home, 'agents-poll-release');
  const completedFile = path.join(home, 'agents-poll-completed');
  let daemon: DaemonHandle | null = null;
  let fixturePid: number | null = null;

  t.after(async () => {
    await daemon?.stop({ keepHome: true });
    fixturePid ??= readFixturePid(startedFile);
    const pid = fixturePid;
    if (pid !== null && pidAlive(pid)) {
      try {
        process.kill(pid, 'SIGKILL');
      } catch {
        /* already gone */
      }
      await waitUntil(() => !pidAlive(pid), {
        timeoutMs: 1_000,
        intervalMs: 10,
        label: 'failed-test agents poll fixture cleanup',
      });
    }
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // A successfully delivered [] poll would absence-tombstone this live
  // agents-cli card. Leaving it untouched after shutdown proves the stopped
  // poll discarded its completed subprocess result before ingestAgentsPoll.
  const seeded = openDb(path.join(home, 'fleetd.db'));
  const now = Date.now();
  seeded
    .prepare(
      `INSERT INTO sessions
          (session_id, callsign, col, note, started_at, last_seen, source)
         VALUES (?, ?, 'working', 'awaiting in-flight poll', ?, ?, 'agents-cli')`,
    )
    .run(SESSION_ID, 'otter-p1poll', now, now);
  seeded.close();

  daemon = await startDaemon({
    home,
    env: {
      FLEETDECK_MDNS: 'off',
      FLEETDECK_AGENTS_CMD: `${process.execPath} ${POLL_FIXTURE}`,
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_AGENTS_IDLE_POLL_MS: '100',
      FLEETDECK_TEST_AGENTS_POLL_STARTED: startedFile,
      FLEETDECK_TEST_AGENTS_POLL_RELEASE: releaseFile,
      FLEETDECK_TEST_AGENTS_POLL_COMPLETED: completedFile,
    },
  });

  fixturePid = await waitUntil(() => readFixturePid(startedFile), {
    timeoutMs: 3_000,
    intervalMs: 10,
    label: 'agents poll fixture child to start',
  });
  assert.equal(pidAlive(fixturePid), true, 'poll child is live before SIGTERM');

  // Publish the gate immediately before SIGTERM. The fixture would now remain
  // live for 2 s, below execFileP's 5 s timeout, unless P3 interruption owns it.
  writeFileSync(releaseFile, 'complete after shutdown begins\n');
  const exited = childExit(daemon.proc);
  const signaledAt = Date.now();
  assert.equal(daemon.proc.kill('SIGTERM'), true);
  const result = await within(exited, 'fleetd joining its in-flight agents poll');

  assert.deepEqual(result, { code: 0, signal: null }, daemon.stderr || daemon.stdout);
  assert.equal(
    existsSync(completedFile),
    false,
    'the interrupted poll command must not reach its natural completion marker',
  );
  assert.equal(pidAlive(fixturePid), false, 'daemon exit follows poll child cleanup/reap');
  assert.ok(Date.now() - signaledAt < scaleMs(2_000), 'shutdown interrupts the 2 s fixture sleep');

  const verified = openDb(path.join(home, 'fleetd.db'));
  try {
    const row = verified
      .prepare<SessionRow>(
        'SELECT col, note, ended_at, end_reason FROM sessions WHERE session_id = ?',
      )
      .get(SESSION_ID);
    assert.deepEqual(row, {
      col: 'working',
      note: 'awaiting in-flight poll',
      ended_at: null,
      end_reason: null,
    });
  } finally {
    verified.close();
  }

  const output = `${daemon.stdout}\n${daemon.stderr}`;
  assert.doesNotMatch(output, /shutdown error|database (?:is )?closed|SQLITE_MISUSE|unhandled/i);
});
