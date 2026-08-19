import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createFiles, runBounded } from '../src/daemon/files.ts';
import { makePlainDir, makeRepoWithWorktree } from './helpers/gitrepo.ts';
import test from './helpers/harness-test.ts';
import { scaleMs, waitUntil } from './helpers/wait.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/files-run-bounded-child.mjs', import.meta.url));

function fixtureArgs(mode: string, ...args: string[]): string[] {
  return [FIXTURE, mode, ...args];
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
    // Already gone.
  }
}

function readPid(file: string): number | null {
  try {
    const pid = Number(readFileSync(file, 'utf8'));
    return Number.isSafeInteger(pid) && pid > 1 ? pid : null;
  } catch {
    return null;
  }
}

function filesAt(root: string): ReturnType<typeof createFiles> {
  const ctx = {
    q: {
      getSession: { get: () => null },
      spawnBySession: { get: () => null },
    },
    browseRootChoice: () => ({ source: 'override' as const, resolved: root }),
  };
  return createFiles(ctx as unknown as Parameters<typeof createFiles>[0]);
}

test('runBounded shares a configurable exact byte cap across stdout and stderr', async () => {
  const maxBytes = 64;
  const stdoutBytes = 24;
  const stderrBytes = maxBytes - stdoutBytes;

  const exact = await within(
    runBounded(process.execPath, fixtureArgs('bytes', String(stdoutBytes), String(stderrBytes)), {
      timeoutMs: 2_000,
      maxBytes,
    }),
    'exact-cap command',
  );
  assert.deepEqual(exact, {
    code: 0,
    stdout: Buffer.alloc(stdoutBytes, 0x61),
    stderr: 'b'.repeat(stderrBytes),
    truncated: false,
    timedOut: false,
  });

  const over = await within(
    runBounded(
      process.execPath,
      fixtureArgs('bytes', String(stdoutBytes), String(stderrBytes + 1), 'hold'),
      { timeoutMs: 2_000, maxBytes },
    ),
    'over-cap command',
  );
  assert.deepEqual(over, {
    code: null,
    stdout: Buffer.alloc(stdoutBytes, 0x61),
    stderr: 'b'.repeat(stderrBytes),
    truncated: true,
    timedOut: false,
  });
  assert.equal(over.stdout.length + Buffer.byteLength(over.stderr), maxBytes);
});

test('runBounded maps synchronous validation and asynchronous missing-spawn failures', async () => {
  const missingPath = path.join(
    tmpdir(),
    `fleetdeck-files-missing-${String(process.pid)}-${String(Date.now())}`,
  );
  assert.equal(existsSync(missingPath), false);

  const missing = await runBounded(missingPath, [], { timeoutMs: 1_000, maxBytes: 64 });
  assert.equal(missing.code, process.platform === 'win32' ? -4058 : -2);
  assert.deepEqual(missing.stdout, Buffer.alloc(0));
  assert.match(missing.stderr, /ENOENT|no such file/i);
  assert.equal(missing.truncated, false);
  assert.equal(missing.timedOut, false);

  const invalid = await runBounded('invalid\0command', [], { timeoutMs: 1_000, maxBytes: 64 });
  assert.equal(invalid.code, null);
  assert.deepEqual(invalid.stdout, Buffer.alloc(0));
  assert.match(invalid.stderr, /null bytes?/i);
  assert.equal(invalid.truncated, false);
  assert.equal(invalid.timedOut, false);
});

test('runBounded writes finite stdin and retains raw output on nonzero exit', async () => {
  const input = Buffer.from([0, 1, 0xfe, 0xff]);
  assert.deepEqual(
    await runBounded(process.execPath, fixtureArgs('stdin'), {
      timeoutMs: 2_000,
      maxBytes: 128,
      input,
    }),
    {
      code: 0,
      stdout: Buffer.from('0001feff'),
      stderr: '',
      truncated: false,
      timedOut: false,
    },
  );

  assert.deepEqual(
    await runBounded(process.execPath, fixtureArgs('nonzero'), {
      timeoutMs: 2_000,
      maxBytes: 128,
    }),
    {
      code: 7,
      stdout: Buffer.from('kept stdout'),
      stderr: '  fatal: bounded fixture  \n',
      truncated: false,
      timedOut: false,
    },
  );
});

test('runBounded timeout means truncation, uses immediate SIGKILL, and settles after reap', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-files-timeout-'));
  const pidFile = path.join(scratch, 'child.pid');
  const termMarker = path.join(scratch, 'term.marker');
  let pid: number | null = null;
  t.after(() => {
    forceKill(pid);
    rmSync(scratch, { recursive: true, force: true });
  });

  const started = Date.now();
  const pending = runBounded(process.execPath, fixtureArgs('timeout', pidFile, termMarker), {
    timeoutMs: 500,
    maxBytes: 128,
  });
  pid = await waitUntil(() => readPid(pidFile), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'runBounded timeout child pid',
  });
  const result = await within(pending, 'runBounded timeout cleanup', 2_000);
  assert.deepEqual(result, {
    code: null,
    stdout: Buffer.from('partial stdout'),
    stderr: 'partial stderr',
    truncated: true,
    timedOut: true,
  });
  assert.ok(Date.now() - started < scaleMs(1_500), 'timeout must not add a TERM grace period');
  assert.equal(existsSync(termMarker), false, 'the timeout path must send SIGKILL directly');
  assert.equal(pidAlive(pid), false, 'the result resolves only after the child close/reap event');
});

test('files callers feed check-ignore stdin and treat git grep exit 1 as a normal empty search', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-files-bounded-callers' });
  t.after(() => repo.cleanup());
  writeFileSync(path.join(repo.worktree, '.gitignore'), '*.ignored\n');
  writeFileSync(path.join(repo.worktree, 'fixture.ignored'), 'not searchable\n');
  const files = filesAt(repo.worktree);

  const listed = await files.fsListHome('');
  assert.equal(listed.status, 200);
  const entries = listed.body['entries'];
  assert.ok(Array.isArray(entries));
  assert.equal(
    entries.some(
      (entry) =>
        typeof entry === 'object' &&
        entry !== null &&
        'name' in entry &&
        entry.name === 'fixture.ignored' &&
        'ignored' in entry &&
        entry.ignored === true,
    ),
    true,
    'git check-ignore consumes the NUL-delimited stdin supplied by ignoredPaths',
  );

  const searched = await files.fsSearchHome('definitely-no-such-needle', { mode: 'content' });
  assert.equal(searched.status, 200);
  const { elapsed_ms: elapsedMs, ...body } = searched.body;
  assert.equal(typeof elapsedMs, 'number');
  assert.deepEqual(body, {
    ok: true,
    mode: 'content',
    q: 'definitely-no-such-needle',
    backend: 'git',
    hits: [],
    truncated: false,
  });
});

test('files admits exactly two searches, rejects the third, and releases both slots', async (t) => {
  const plain = makePlainDir();
  t.after(() => plain.cleanup());
  for (let index = 0; index < 520; index += 1) {
    writeFileSync(path.join(plain.dir, `file-${String(index).padStart(4, '0')}.txt`), '');
  }
  const files = filesAt(plain.dir);

  const first = files.fsSearchHome('zz', { mode: 'name' });
  const second = files.fsSearchHome('zz', { mode: 'name' });
  const third = await files.fsSearchHome('zz', { mode: 'name' });
  assert.deepEqual(third, {
    status: 429,
    body: { ok: false, reason: 'search busy — try again' },
  });

  const admitted = await within(Promise.all([first, second]), 'two admitted searches');
  assert.deepEqual(
    admitted.map((result) => result.status),
    [200, 200],
  );
  const afterRelease = await files.fsSearchHome('zz', { mode: 'name' });
  assert.equal(afterRelease.status, 200, 'settled searches release both global admission slots');
});
