// tests/spawn-unsupervised.test.ts
//
// v1.3 — unsupervised spawns + plan library (unsupervised spawns section).
// Additive to tests/spawn.test.ts (v1.2 dynamic
// fleet) — kept in its own file so it can land/iterate independently of the
// v1.2 suite. Same harness as tests/spawn.test.ts: every test uses the
// FLEETDECK_SPAWN_CMD test override (tests/helpers/spawn-cmd-fixture.ts)
// instead of real tmux, so we can inspect the full env-wrapped claude argv the daemon
// would have executed.
//
// Contract (v1.3 §A, unsupervised spawns):
//   POST /api/spawn gains `dangerously_skip_permissions: bool` ->
//   appends `--dangerously-skip-permissions` to the claude argv;
//   `permission_mode` additionally accepts "bypassPermissions" (plain
//   passthrough). Both allowed together (CLI semantics decide). `spawns`
//   table gains `skip_permissions INTEGER DEFAULT 0` (set when EITHER form
//   of bypass was requested); the per-card /state `spawn` object gains
//   `skip_permissions: bool`.
//
// Coverage map (task brief bullet 6 -> tests below):
//   dangerously_skip_permissions:true -> argv has --dangerously-skip-permissions
//     AND row/state spawn.skip_permissions true
//        -> "unsupervised spawn: dangerously_skip_permissions:true ..."
//   permission_mode 'bypassPermissions' -> argv has --permission-mode
//     bypassPermissions (two elements) and skip flag also set
//        -> "unsupervised spawn: permission_mode 'bypassPermissions' ..."
//   neither -> flag false, no such argv
//        -> "unsupervised spawn: neither flag requested ..."
//   (bonus, contract-legal) both together
//        -> "unsupervised spawn: both flags together ..."

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startDaemon, randomPort, type DaemonHandle } from './helpers/daemon.ts';
import { postJson, getJson } from './helpers/http.ts';
import { waitForSpecRecords, makeSpecRecordFile } from './helpers/wait.ts';
import type { SessionEntry, StateResponse } from '../contracts/state.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.ts');
try {
  chmodSync(SPAWN_CMD_FIXTURE, 0o755);
} catch {
  /* best-effort, see tests/spawn.test.ts */
}

// The /api/spawn ack and the arm-unsupervised ack: only the fields these tests
// read as ordered/matched values are pinned; everything else stays off the type.
interface SpawnResponse {
  session_id: string;
}
interface ArmResponse {
  arm_token: string;
}
// One recorded spec line: the fixture wraps the daemon-computed spec under
// `.parsed`; its interior is not pinned by the contract, so it stays unknown.
interface SpecRecord {
  parsed: unknown;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function scratchDir(prefix = 'fleetdeck-spawn-unsup-') {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

function findSession(state: StateResponse, sid: string): SessionEntry | undefined {
  return state.sessions.find((s) => s.session_id === sid);
}

function spawnCmdEnv({
  recordFile,
  postUrl,
}: {
  recordFile: string;
  postUrl?: string;
}): Record<string, string> {
  const env: Record<string, string> = {
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: recordFile,
  };
  if (postUrl) env['FLEETDECK_TEST_SPAWN_POST_URL'] = postUrl;
  return env;
}

/** Recursively find an array anywhere in `spec` matching `pred` (mirrors
 * tests/spawn.test.ts's extractArgv — the exact top-level shape of `spec`
 * is not pinned by the contract beyond "argv [FLEETDECK_SPAWN_CMD,
 * JSON.stringify(spec)]"). */
function findArray(
  value: unknown,
  pred: (arr: unknown[]) => boolean,
  seen = new Set<object>(),
): unknown[] | null {
  if (Array.isArray(value)) {
    const arr = value as unknown[];
    if (pred(arr)) return arr;
  }
  if (isRecord(value)) {
    if (seen.has(value)) return null;
    seen.add(value);
    for (const v of Object.values(value)) {
      const found = findArray(v, pred, seen);
      if (found) return found;
    }
  }
  return null;
}

function extractArgv(spec: unknown): unknown[] | null {
  if (isRecord(spec)) {
    const argv = spec['argv'];
    if (Array.isArray(argv)) return argv as unknown[];
  }
  return findArray(spec, (arr) => arr.includes('claude'));
}

// 0.16.0: unsupervised spawns (dangerously_skip_permissions:true or
// permission_mode:'bypassPermissions') require a fresh single-use arm_token
// from POST /api/spawn/arm-unsupervised (itself bearer-gated).
async function armUnsupervised(daemon: DaemonHandle): Promise<string> {
  const res = await postJson(
    `${daemon.baseUrl}/api/spawn/arm-unsupervised`,
    {},
    { token: daemon.token },
  );
  assert.equal(
    res.status,
    200,
    `arm-unsupervised should 200 (got ${res.status}: ${JSON.stringify(res.json)})`,
  );
  return (res.json as ArmResponse).arm_token;
}

// ---------------------------------------------------------------------------

test('unsupervised spawn: dangerously_skip_permissions:true adds --dangerously-skip-permissions to argv and sets spawn.skip_permissions true', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = makeSpecRecordFile(t);
  const cwd = scratchDir();
  const daemon = await startDaemon({
    port,
    env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }),
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    dangerously_skip_permissions: true,
    arm_token: await armUnsupervised(daemon),
  });
  assert.equal(
    res.status,
    200,
    `spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`,
  );
  const sid = (res.json as SpawnResponse).session_id;

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = findSession(state, sid);
  assert.ok(
    card?.spawn,
    'the spawned card should carry a spawn{} descriptor immediately (the row is inserted before the HTTP response returns)',
  );
  assert.equal(
    card.spawn.skip_permissions,
    true,
    'spawn.skip_permissions must be true when dangerously_skip_permissions:true was requested',
  );

  const specs = await waitForSpecRecords(rec, 1);
  const argv = extractArgv((specs[specs.length - 1] as SpecRecord).parsed);
  assert.ok(argv, 'expected an argv-shaped array in the recorded spec');
  assert.ok(
    argv.includes('--dangerously-skip-permissions'),
    `argv must include --dangerously-skip-permissions when requested (got: ${JSON.stringify(argv)})`,
  );
});

test("unsupervised spawn: permission_mode 'bypassPermissions' adds --permission-mode bypassPermissions (two argv elements) and also sets spawn.skip_permissions true", async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = makeSpecRecordFile(t);
  const cwd = scratchDir();
  const daemon = await startDaemon({
    port,
    env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }),
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    permission_mode: 'bypassPermissions',
    arm_token: await armUnsupervised(daemon),
  });
  assert.equal(
    res.status,
    200,
    `spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`,
  );
  const sid = (res.json as SpawnResponse).session_id;

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = findSession(state, sid);
  assert.ok(card?.spawn, 'the spawned card should carry a spawn{} descriptor');
  assert.equal(
    card.spawn.skip_permissions,
    true,
    'permission_mode:"bypassPermissions" is itself a form of bypass — spawn.skip_permissions must be true',
  );

  const specs = await waitForSpecRecords(rec, 1);
  const argv = extractArgv((specs[specs.length - 1] as SpecRecord).parsed);
  assert.ok(argv, 'expected an argv-shaped array in the recorded spec');
  const pmIdx = argv.indexOf('--permission-mode');
  assert.ok(pmIdx >= 0, 'argv must include --permission-mode');
  assert.equal(
    argv[pmIdx + 1],
    'bypassPermissions',
    '--permission-mode must be followed by bypassPermissions as a SEPARATE argv element (two elements, no shell-joining)',
  );
  assert.ok(
    !argv.includes('--dangerously-skip-permissions'),
    'dangerously_skip_permissions was NOT requested here, so its dedicated flag must not appear (only the DB-level skip_permissions bit is shared between the two bypass forms)',
  );
});

test('unsupervised spawn: neither dangerously_skip_permissions nor permission_mode bypassPermissions requested -> skip_permissions false, no --dangerously-skip-permissions in argv', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = makeSpecRecordFile(t);
  const cwd = scratchDir();
  const daemon = await startDaemon({
    port,
    env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }),
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(
    res.status,
    200,
    `spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`,
  );
  const sid = (res.json as SpawnResponse).session_id;

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = findSession(state, sid);
  assert.ok(card?.spawn, 'the spawned card should carry a spawn{} descriptor');
  assert.equal(
    card.spawn.skip_permissions,
    false,
    'spawn.skip_permissions must be false when neither bypass form was requested',
  );

  const specs = await waitForSpecRecords(rec, 1);
  const argv = extractArgv((specs[specs.length - 1] as SpecRecord).parsed);
  assert.ok(argv, 'expected an argv-shaped array in the recorded spec');
  assert.ok(
    !argv.includes('--dangerously-skip-permissions'),
    'argv must not include --dangerously-skip-permissions when it was never requested',
  );
  assert.ok(
    !argv.includes('--permission-mode'),
    'argv must not include --permission-mode when none was requested',
  );
});

test('unsupervised spawn: both dangerously_skip_permissions:true AND permission_mode bypassPermissions together (contract-legal) produce both argv effects', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = makeSpecRecordFile(t);
  const cwd = scratchDir();
  const daemon = await startDaemon({
    port,
    env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }),
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    dangerously_skip_permissions: true,
    permission_mode: 'bypassPermissions',
    arm_token: await armUnsupervised(daemon),
  });
  assert.equal(
    res.status,
    200,
    `spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`,
  );
  const sid = (res.json as SpawnResponse).session_id;

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = findSession(state, sid);
  assert.equal(card?.spawn?.skip_permissions, true);

  const specs = await waitForSpecRecords(rec, 1);
  const argv = extractArgv((specs[specs.length - 1] as SpecRecord).parsed);
  assert.ok(argv, 'expected an argv-shaped array in the recorded spec');
  assert.ok(
    argv.includes('--dangerously-skip-permissions'),
    'both requested together: --dangerously-skip-permissions must still appear',
  );
  const pmIdx = argv.indexOf('--permission-mode');
  assert.ok(
    pmIdx >= 0 && argv[pmIdx + 1] === 'bypassPermissions',
    'both requested together: --permission-mode bypassPermissions must still appear',
  );
});
