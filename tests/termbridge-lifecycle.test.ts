import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createTermBridge, type TermFrame } from '../src/daemon/termbridge.ts';
import type { SpawnRow } from '../src/daemon/statements.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(HERE, 'helpers/termbridge-lifecycle-fixture.ts');
try {
  chmodSync(FIXTURE, 0o755);
} catch {
  /* best effort; git also preserves the executable bit */
}

interface FixtureRecord {
  pid: number;
  type: string;
  line?: string;
  signal?: string;
}

function records(file: string): FixtureRecord[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line) as FixtureRecord);
}

async function waitUntil<T>(
  read: () => T | null | undefined | false,
  label: string,
  timeoutMs = 5_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = read();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`timed out waiting for ${label}`);
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function fixtureEnvironment(
  t: TestContext,
  record: string,
  extra: Record<string, string> = {},
): void {
  const next = {
    FLEETDECK_TERM_CMD: FIXTURE,
    FLEETDECK_TEST_TERM_LIFECYCLE_RECORD: record,
    ...extra,
  };
  const previous = new Map<string, string | undefined>();
  for (const [key, value] of Object.entries(next)) {
    previous.set(key, process.env[key]);
    process.env[key] = value;
  }
  t.after(() => {
    for (const [key, value] of previous) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });
}

function liveRow(port: number): SpawnRow {
  return {
    status: 'live',
    tmux_session: `fleetdeck-${port}`,
    tmux_window: `fd${port}-lifecycle`,
  } as unknown as SpawnRow;
}

function isClosedError(error: unknown): boolean {
  return (
    error instanceof Error &&
    'reason' in error &&
    (error as Error & { reason?: unknown }).reason === 'terminal bridge is closed'
  );
}

test('termbridge close is one idempotent Promise and permanently rejects new viewers', async () => {
  let resolveCalls = 0;
  const bridge = createTermBridge({
    port: 22001,
    resolveSpawn: () => {
      resolveCalls += 1;
      return liveRow(22001);
    },
  });

  const first = bridge.close();
  const second = bridge.close();
  assert.strictEqual(second, first, 'double close must share the exact release Promise');
  await first;
  await bridge.close();

  await assert.rejects(
    bridge.openViewer({ spawn_id: 'after-close', cols: 80, rows: 24, send: () => {} }),
    isClosedError,
  );
  assert.equal(resolveCalls, 0, 'a post-quiesce viewer must be refused before DB/spawn lookup');
});

test('termbridge close closes active viewers, TERM-to-KILL reaps the child, and detaches callbacks', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-termbridge-close-'));
  const record = path.join(scratch, 'fixture.jsonl');
  fixtureEnvironment(t, record, {
    FLEETDECK_TEST_TERM_LIFECYCLE_IGNORE_SIGTERM: '1',
    FLEETDECK_TEST_TERM_LIFECYCLE_OUTPUT_MS: '5',
  });
  const frames: TermFrame[] = [];
  const closes: string[] = [];
  const logs: string[] = [];
  let reentrantClose: Promise<void> | null = null;
  const bridge = createTermBridge({
    port: 22002,
    resolveSpawn: () => liveRow(22002),
    log: (line) => logs.push(line),
    closeGraceMs: 50,
  });
  t.after(() => bridge.close());
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  const handle = await bridge.openViewer({
    spawn_id: 'active',
    cols: 80,
    rows: 24,
    send: (frame) => frames.push(frame),
    onClose: (reason) => {
      closes.push(reason);
      reentrantClose = bridge.close();
    },
  });
  await waitUntil(() => frames.find((frame) => frame.t === 'out'), 'fixture output');
  const pid = await waitUntil(
    () => records(record).find((entry) => entry.type === 'start')?.pid,
    'fixture pid',
  );

  const startedAt = Date.now();
  const first = bridge.close();
  const second = bridge.close();
  assert.strictEqual(second, first);
  await first;
  const elapsedMs = Date.now() - startedAt;

  assert.strictEqual(
    reentrantClose,
    first,
    'an onClose callback that re-enters close must observe the published release Promise',
  );
  assert.deepEqual(closes, ['terminal bridge is closed']);
  assert.ok(
    records(record).some((entry) => entry.type === 'signal' && entry.signal === 'SIGTERM'),
    'close must attempt SIGTERM before the backstop',
  );
  assert.equal(alive(pid), false, 'the SIGTERM-immune control child must be reaped');
  assert.ok(elapsedMs >= 35, `SIGKILL backstop fired suspiciously early (${elapsedMs}ms)`);
  assert.ok(elapsedMs < 1_000, `bounded close took too long (${elapsedMs}ms)`);

  const callbacksAtClose = { frames: frames.length, closes: closes.length, logs: logs.length };
  handle.input('ignored');
  handle.paste('ignored');
  handle.resize(100, 40);
  handle.close();
  await new Promise((resolve) => setTimeout(resolve, 75));
  assert.deepEqual(
    { frames: frames.length, closes: closes.length, logs: logs.length },
    callbacksAtClose,
    'no output, close, or log callback may fire after bridge close settles',
  );
});

test('termbridge bounds its post-KILL join when a descendant inherits the control pipes', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-termbridge-inherited-pipe-'));
  const record = path.join(scratch, 'fixture.jsonl');
  fixtureEnvironment(t, record, {
    FLEETDECK_TEST_TERM_LIFECYCLE_IGNORE_SIGTERM: '1',
    FLEETDECK_TEST_TERM_LIFECYCLE_INHERIT_PIPES: '1',
  });
  const bridge = createTermBridge({
    port: 22005,
    resolveSpawn: () => liveRow(22005),
    closeGraceMs: 50,
  });
  t.after(() => bridge.close());
  t.after(() => {
    for (const entry of records(record)) {
      if (entry.type !== 'pipe-holder' || !alive(entry.pid)) continue;
      try {
        process.kill(entry.pid, 'SIGKILL');
      } catch {
        /* already gone */
      }
    }
  });
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  await bridge.openViewer({ spawn_id: 'inherited-pipe', cols: 80, rows: 24, send: () => {} });
  const directPid = await waitUntil(
    () => records(record).find((entry) => entry.type === 'start')?.pid,
    'direct fixture pid',
  );
  const pipeHolderPid = await waitUntil(
    () => records(record).find((entry) => entry.type === 'pipe-holder')?.pid,
    'inherited-pipe holder pid',
  );

  const startedAt = Date.now();
  let deadline: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      bridge.close(),
      new Promise<never>((_, reject) => {
        deadline = setTimeout(
          () => reject(new Error('termbridge close stayed joined to inherited control pipes')),
          1_000,
        );
        deadline.unref();
      }),
    ]);
  } finally {
    if (deadline) clearTimeout(deadline);
  }

  assert.equal(alive(directPid), false, 'the direct control child must be gone when close settles');
  assert.equal(
    alive(pipeHolderPid),
    true,
    'the pipe-holding descendant proves close did not wait for ChildProcess close',
  );
  assert.ok(Date.now() - startedAt < 1_000, 'post-KILL join must remain bounded');
});

test('termbridge close rejects a wedged command and waits for its input chain to settle', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-termbridge-input-'));
  const record = path.join(scratch, 'fixture.jsonl');
  fixtureEnvironment(t, record, {
    FLEETDECK_TEST_TERM_LIFECYCLE_HANG_SEND: '1',
  });
  const closes: string[] = [];
  const bridge = createTermBridge({
    port: 22003,
    resolveSpawn: () => liveRow(22003),
    closeGraceMs: 50,
  });
  t.after(() => bridge.close());
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  const handle = await bridge.openViewer({
    spawn_id: 'input',
    cols: 80,
    rows: 24,
    send: () => {},
    onClose: (reason) => closes.push(reason),
  });
  handle.input('queued input');
  await waitUntil(
    () => records(record).some((entry) => entry.type === 'send-hung'),
    'wedged send-keys waiter',
  );

  await bridge.close();
  assert.deepEqual(closes, ['terminal bridge is closed']);
  const pid = records(record).find((entry) => entry.type === 'start')?.pid;
  assert.ok(pid);
  assert.equal(alive(pid), false, 'close must join the child after rejecting the input waiter');
});

test('termbridge quiesce cancels a scheduled window-close recheck', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-termbridge-timer-'));
  const record = path.join(scratch, 'fixture.jsonl');
  fixtureEnvironment(t, record, {
    FLEETDECK_TEST_TERM_LIFECYCLE_WINDOW_CLOSE: '1',
  });
  const bridge = createTermBridge({
    port: 22004,
    resolveSpawn: () => liveRow(22004),
    closeGraceMs: 50,
  });
  t.after(() => bridge.close());
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  await bridge.openViewer({ spawn_id: 'timer', cols: 80, rows: 24, send: () => {} });
  await waitUntil(
    () => records(record).some((entry) => entry.type === 'close-probe-error'),
    'failed close probe that schedules a recheck',
  );
  await bridge.close();
  const commandsAtClose = records(record).filter((entry) => entry.type === 'command').length;
  await new Promise((resolve) => setTimeout(resolve, 1_100));
  assert.equal(
    records(record).filter((entry) => entry.type === 'command').length,
    commandsAtClose,
    'the one-second recheck timer must not issue a post-quiesce command',
  );
});
