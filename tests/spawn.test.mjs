// tests/spawn.test.mjs
//
// v1.2 — dynamic fleet (board-spawned sessions).
// Spawn = explicit human click; the daemon owns the pane; all tmux command
// construction is argv arrays, never a shell string containing user text.
// This file never launches a real tmux pane or a real billed `claude`
// session — every test uses the FLEETDECK_SPAWN_CMD override (see
// tests/helpers/spawn-cmd-fixture.mjs), which stands in for tmux and lets us
// inspect exactly what the daemon would have executed, and (when told via
// env) plays the spawned pane's first hook event back at the daemon.
//
// Coverage map (task brief bullets -> tests below):
//   1. Capability flag
//        -> "capability: FLEETDECK_SPAWN_CMD reports available:true reason:test-override..."
//        -> "capability: FLEETDECK_SPAWN=off reports available:false..."
//   2. Spawn happy path (pre-created card, response shape, source/status
//      flip, argv construction incl. a shell-metachar-hostile prompt)
//        -> "spawn happy path: POST /api/spawn pre-creates the card..."
//        -> "spawn happy path: the fixture's SessionStart flips source..."
//        -> "argv construction: prompt/model/permission-mode survive intact..."
//   3. Fail-loud (missing cwd, cap, worktree on non-git cwd)
//        -> "fail-loud: missing cwd -> 4xx with reason"
//        -> "fail-loud: FLEETDECK_MAX_SPAWNED cap -> 4xx on the (cap+1)th spawn"
//        -> "fail-loud: worktree:true on a non-git cwd -> 409"
//   4. Worktree path
//        -> "worktree path: worktree:true creates a sibling --fd-<callsign> checkout..."
//   5. Kill semantics
//        -> "kill: unknown spawn id -> 404"
//        -> "kill: a live (non-offline) card is refused without force -> 409"
//        -> "kill: an offline card's kill is accepted, or 410 if the window is already gone"
//   6. Stale flag
//        -> "stale flag: a working card with no events for FLEETDECK_STALE_MS..."
//   7. Unrouted text
//        -> "unrouted assign-auto carries the task text verbatim (v1.2 CTA support)"
//   8. Reconciliation (best-effort; see the test's own comments for what
//      stays unverifiable without real tmux)
//        -> "reconciliation: a spawn row whose window was never actually..."
//   9. Regression: the full existing suite staying green is verified by
//      running `node --test --test-concurrency=1` across the whole tests/
//      directory, not by anything inside this file.
//
// Spec-shape caveat: the contract pins the spawns DB row's column names
// and the /api/spawn request/response shapes exactly, but does NOT pin the
// exact JSON shape of `spec` handed to FLEETDECK_SPAWN_CMD beyond "argv
// [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)]" and the full env-wrapped argv
// it must produce (`env ... claude --session-id <uuid> [--model m]
// [--permission-mode pm] [prompt]`). Assertions on `spec` below search
// recursively for reasonably-named fields (session_id/sessionId, cwd, an
// argv-shaped array) rather than assuming one exact top-level layout, and
// prefer a field literally named `argv` first since that is the most direct
// reading of the contract text. Any mismatch here is a deviation to report,
// not a bug in this file's expectations.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, readFileSync, existsSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startDaemon, randomPort } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { makeRepoWithWorktree, makePlainDir } from './helpers/gitrepo.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
// Defensive re-assert of the executable bit: git does not always preserve
// it across checkouts/CI runners, and the contract's "argv [cmd, ...]"
// wording requires this file to be directly executable (see the fixture's
// own header comment for why this differs from FLEETDECK_AGENTS_CMD).
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best-effort */ }

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function scratchDir(prefix = 'fleetdeck-spawn-') {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

const WAIT_SCALE = Number(process.env.FLEETDECK_TEST_WAIT_SCALE) || 1;

async function waitUntil(fn, { timeoutMs = 8000, intervalMs = 100, label = 'condition' } = {}) {
  const effectiveTimeoutMs = timeoutMs * WAIT_SCALE;
  const deadline = Date.now() + effectiveTimeoutMs;
  for (;;) {
    const result = await fn();
    if (result) return result;
    if (Date.now() >= deadline) throw new Error(`waitUntil: ${label} not met within ${effectiveTimeoutMs}ms`);
    await new Promise(r => setTimeout(r, intervalMs));
  }
}

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

/** Read+parse every JSONL line recorded so far by the spawn-cmd fixture. */
function readSpecRecords(file) {
  if (!existsSync(file)) return [];
  const text = readFileSync(file, 'utf8');
  return text.split('\n').filter(Boolean).map(line => {
    try { return JSON.parse(line); } catch { return null; }
  }).filter(Boolean);
}

async function waitForSpecRecords(file, minCount, opts) {
  return waitUntil(() => {
    const recs = readSpecRecords(file);
    return recs.length >= minCount ? recs : null;
  }, { label: `>= ${minCount} recorded spec(s) in ${file}`, ...opts });
}

/** Recursively find an array anywhere in `spec` matching `pred`. */
function findArray(value, pred, seen = new Set()) {
  if (Array.isArray(value)) { if (pred(value)) return value; }
  if (value && typeof value === 'object') {
    if (seen.has(value)) return null;
    seen.add(value);
    for (const v of Object.values(value)) {
      const found = findArray(v, pred, seen);
      if (found) return found;
    }
  }
  return null;
}

/** Best-effort extraction of the claude argv array from a captured spec. */
function extractArgv(spec) {
  if (Array.isArray(spec?.argv)) return spec.argv;
  return findArray(spec, arr => arr.includes('claude'));
}

function findByKeys(value, keys, seen = new Set()) {
  if (value == null || typeof value !== 'object') return null;
  if (seen.has(value)) return null;
  seen.add(value);
  for (const k of keys) {
    if (typeof value[k] === 'string' && value[k]) return value[k];
  }
  for (const v of Object.values(value)) {
    if (v && typeof v === 'object') {
      const found = findByKeys(v, keys, seen);
      if (found) return found;
    }
  }
  return null;
}

function specCwd(spec) {
  return findByKeys(spec, ['cwd', 'worktree_path', 'worktreePath']);
}

/** Recursively search for `target` as an exact string value/array element. */
function containsExactString(value, target, seen = new Set()) {
  if (value === target) return true;
  if (value && typeof value === 'object') {
    if (seen.has(value)) return false;
    seen.add(value);
    for (const v of Object.values(value)) {
      if (containsExactString(v, target, seen)) return true;
    }
  }
  return false;
}

function spawnCmdEnv({ recordFile, postUrl, staleMs } = {}) {
  const env = {
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: recordFile,
  };
  if (postUrl) env.FLEETDECK_TEST_SPAWN_POST_URL = postUrl;
  if (staleMs !== undefined) env.FLEETDECK_STALE_MS = String(staleMs);
  return env;
}

// ---------------------------------------------------------------------------
// 1. Capability flag
// ---------------------------------------------------------------------------

test('capability: FLEETDECK_SPAWN_CMD reports available:true reason:test-override on /health and /state', async (t) => {
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const daemon = await startDaemon({ env: spawnCmdEnv({ recordFile: rec }) });
  t.after(async () => { await daemon.stop(); });

  const health = await getJson(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200);
  assert.ok(health.json?.spawn, '/health should carry a top-level spawn capability object');
  assert.equal(health.json.spawn.available, true, 'FLEETDECK_SPAWN_CMD should make the daemon report spawn availability');
  assert.equal(health.json.spawn.reason, 'test-override', 'the override reason must be exactly "test-override"');
  assert.equal(typeof health.json.spawn.active, 'number', 'spawn.active should be a number');
  assert.equal(health.json.spawn.active, 0, 'no spawns have happened yet');
  assert.equal(health.json.spawn.max, undefined, 'there is no fleet cap — spawn.max must not come back');

  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.equal(state.status, 200);
  assert.ok(state.json?.spawn, '/state should carry the same top-level spawn capability object as /health');
  assert.equal(state.json.spawn.available, true);
  assert.equal(state.json.spawn.reason, 'test-override');
});

test('capability: FLEETDECK_SPAWN=off reports available:false on /health and /state, and POST /api/spawn is refused 4xx', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN: 'off' } });
  const cwd = scratchDir();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const health = await getJson(`${daemon.baseUrl}/health`);
  assert.ok(health.json?.spawn, '/health should carry a spawn capability object even when disabled');
  assert.equal(health.json.spawn.available, false, 'FLEETDECK_SPAWN=off must report available:false');

  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.ok(state.json?.spawn);
  assert.equal(state.json.spawn.available, false);

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.ok(res.status >= 400 && res.status < 500, `POST /api/spawn with spawn disabled must 4xx (got ${res.status})`);
  assert.equal(res.json?.ok, false, 'a refused spawn must carry ok:false');
});

// ---------------------------------------------------------------------------
// 2. Spawn happy path
// ---------------------------------------------------------------------------

test('spawn happy path: POST /api/spawn pre-creates the card immediately (source spawned, col queued, note mentions spawning) before any hook fires', async (t) => {
  // Deliberately NO FLEETDECK_TEST_SPAWN_POST_URL here: the fixture never
  // POSTs a SessionStart back, so this session's card can only ever be in
  // its pre-created state -- proving bullet 1 of the v1.2 spawn flow without
  // racing our own assertions against the fixture's async POST.
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const cwd = scratchDir();
  const daemon = await startDaemon({ env: spawnCmdEnv({ recordFile: rec }) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(res.status, 200, `spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  assert.equal(res.json.ok, true);
  assert.ok(res.json.spawn_id !== undefined && res.json.spawn_id !== null, 'response should carry a spawn_id');
  assert.match(res.json.session_id, UUID_RE, 'response should carry a UUID session_id');
  assert.ok(res.json.callsign, 'response should carry a callsign');
  assert.ok(res.json.tmux, 'response should carry a tmux descriptor');
  assert.equal(res.json.tmux.session, `fleetdeck-${daemon.port}`, 'tmux session name should be scoped fleetdeck-<port>');
  assert.equal(res.json.tmux.window, `fd${daemon.port}-${res.json.callsign}`, 'tmux window name should be scoped fd<port>-<callsign>');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, res.json.session_id);
  assert.ok(card, 'the pre-created card should exist in /state immediately, before any hook event');
  assert.equal(card.source, 'spawned', 'a spawned-but-not-yet-hooked card should carry source:spawned');
  assert.equal(card.col, 'queued', 'the pre-created card should be col:queued');
  assert.match(card.note ?? '', /spawn/i, `the pre-created card's note should mention spawning (got: ${JSON.stringify(card.note)})`);
});

test("spawn happy path: the fixture's SessionStart flips source to hooks and the spawns row to live, and the card gains a spawn{} descriptor", async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const cwd = scratchDir();
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(res.status, 200);
  const { session_id: sid, spawn_id: spawnId } = res.json;

  const card = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const c = findSession(state, sid);
    return c && c.source === 'hooks' ? c : null;
  }, { label: 'source flips to hooks after the fixture posts SessionStart' });

  assert.equal(card.col, 'queued', 'SessionStart should still derive col:queued as normal, now via the hooks path');
  assert.ok(card.spawn, 'a hooked spawned card should carry a spawn{} descriptor');
  assert.equal(String(card.spawn.spawn_id), String(spawnId), 'card.spawn.spawn_id should match the /api/spawn response');
  assert.equal(card.spawn.tmux_window, res.json.tmux.window, 'card.spawn.tmux_window should match the tmux window name');
  assert.equal(card.spawn.status, 'live', "the spawns row should flip to status 'live' on the session's first hook event");
});

test('argv construction: prompt/model/permission-mode survive intact through the FLEETDECK_SPAWN_CMD override, including a shell-metachar-hostile prompt', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const cwd = scratchDir();
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  // The rm target is a guaranteed-nonexistent scratch path so that even in
  // the worst case -- an implementation that (in violation of the contract's
  // explicit "argv arrays, never a shell string" rule) shell-interpolates
  // this text -- the "rm -rf" would be a harmless no-op, not real damage.
  const rmTarget = path.join(tmpdir(), `fleetdeck-spawn-should-never-exist-${randomUUID()}`);
  const dangerousPrompt = `wrap up the task"; rm -rf ${rmTarget} #`;

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    prompt: dangerousPrompt,
    model: 'claude-test-model',
    permission_mode: 'acceptEdits',
  });
  assert.equal(res.status, 200, `spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  const sid = res.json.session_id;

  const specs = await waitForSpecRecords(rec, 1);
  const record = specs[specs.length - 1];
  assert.equal(record.parseError, null, 'the JSON.stringify(spec) argv element must parse back as valid JSON');
  assert.ok(record.parsed, 'sanity: a parsed spec must be present');

  // The dangerous prompt must appear as ONE exact, intact value somewhere in
  // the spec -- proof it crossed the process boundary as a single argv
  // element, not shell-tokenized/split/mangled.
  assert.ok(containsExactString(record.parsed, dangerousPrompt),
    `the exact dangerous prompt string must appear intact in the recorded spec (got: ${JSON.stringify(record.parsed)})`);
  assert.ok(!existsSync(rmTarget), 'sanity: the (harmless, nonexistent) rm target must still not exist');

  const argv = extractArgv(record.parsed);
  assert.ok(argv, `expected an argv-shaped array in the spec per the contract ("claude --session-id <uuid> ..."); spec keys were: ${JSON.stringify(Object.keys(record.parsed || {}))}`);
  // Contract-pinned copy of the spawn() scrub list (fleetd/derive.mjs) — an
  // intentional duplicate: a var silently dropped from the wrapper fails HERE.
  const scrub = [
    'CLAUDECODE', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CODE_CHILD_SESSION',
    'CLAUDE_CODE_BRIDGE_SESSION_ID', 'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_EXECPATH',
    'CLAUDE_ENV_FILE', 'CLAUDE_PROJECT_DIR', 'CLAUDE_PLUGIN_ROOT', 'CLAUDE_PLUGIN_DATA',
    'CLAUDE_EFFORT', 'AI_AGENT', 'CODEX_COMPANION_TRANSCRIPT_PATH',
    'CODEX_COMPANION_SESSION_ID', 'FLEETDECK_AGENTS_CMD', 'FLEETDECK_SPAWN_CMD',
    'TMUX', 'TMUX_PANE', 'FLEETDECK_TMUX_SOCKET',
    'FLEETDECK_AGENTS_POLL_MS', 'FLEETDECK_HOLD_MS', 'FLEETDECK_STALE_MS',
    'FLEETDECK_NUDGE_MS', 'FLEETDECK_WATCH_MAX_MS',
    'FLEETDECK_WATCH_POLL_MS', 'FLEETDECK_SPAWN_REGISTER_MS',
    'FLEETDECK_PANE_MAIL_GRACE_MS', 'FLEETDECK_PRESUME_DEAD_MS',
    'FLEETDECK_RETAIN_OFFLINE_MS',
    'FLEETDECK_RC_HARVEST_MS',
  ];
  const expectedPrefix = [
    'env', ...scrub.flatMap(name => ['-u', name]),
    `FLEETDECK_PORT=${daemon.port}`, `FLEETDECK_HOME=${daemon.home}`,
  ];
  assert.deepEqual(argv.slice(0, expectedPrefix.length), expectedPrefix,
    'argv must scrub inherited agent/fleet variables and pin this daemon port/home in the specified order');
  const claudeIdx = expectedPrefix.length;
  assert.equal(argv[claudeIdx], 'claude', 'claude must immediately follow the deterministic env prefix');

  const sidIdx = argv.indexOf('--session-id');
  assert.ok(sidIdx >= 0, 'argv must include --session-id');
  assert.equal(argv[sidIdx + 1], sid, '--session-id must be followed by the pre-generated session UUID');

  const modelIdx = argv.indexOf('--model');
  assert.ok(modelIdx >= 0, 'argv must include --model when a model was requested');
  assert.equal(argv[modelIdx + 1], 'claude-test-model');

  const pmIdx = argv.indexOf('--permission-mode');
  assert.ok(pmIdx >= 0, 'argv must include --permission-mode when one was requested');
  assert.equal(argv[pmIdx + 1], 'acceptEdits');

  assert.deepEqual(argv.slice(claudeIdx), [
    'claude', '--session-id', sid,
    '--model', 'claude-test-model',
    '--permission-mode', 'acceptEdits',
    dangerousPrompt,
  ], 'claude flag ordering must remain contract-pinned after the env wrapper');

  assert.ok(argv.includes(dangerousPrompt), 'the dangerous prompt must appear as ONE whole argv element, not split into shell words');
  // No fragment of the dangerous prompt should appear as a SEPARATE element
  // (that would indicate shell word-splitting happened before argv construction).
  const fragments = dangerousPrompt.split(' ');
  const suspiciousSplit = argv.some((el, i) => el === fragments[0] && argv[i + 1] === fragments[1]);
  assert.ok(!suspiciousSplit, 'the prompt must not appear word-split across separate argv elements');
});

// ---------------------------------------------------------------------------
// 3. Fail-loud
// ---------------------------------------------------------------------------

test('fail-loud: missing cwd -> 4xx with a reason', async (t) => {
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const daemon = await startDaemon({ env: spawnCmdEnv({ recordFile: rec }) });
  t.after(async () => { await daemon.stop(); });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {});
  assert.ok(res.status >= 400 && res.status < 500, `missing cwd should 4xx (got ${res.status})`);
  assert.equal(res.json?.ok, false);
  assert.equal(typeof res.json?.reason, 'string', 'a fail-loud rejection must carry a string reason');
  assert.ok(res.json.reason.length > 0);
});

test('no fleet cap: the 8th concurrent spawn is as welcome as the 1st', async (t) => {
  // There used to be a hard FLEETDECK_MAX_SPAWNED=5 refusal here. It is gone:
  // the fleet is as big as the human says it is. 8 is deliberately past the old
  // default, so this test fails loudly if a cap ever creeps back in.
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  const dirs = Array.from({ length: 8 }, () => scratchDir());
  t.after(async () => { await daemon.stop(); for (const d of dirs) rmSync(d, { recursive: true, force: true }); });

  for (const [i, dir] of dirs.entries()) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: dir });
    assert.equal(res.status, 200, `spawn #${i + 1} of ${dirs.length} should 200 — there is no cap`);
    await waitUntil(async () => {
      const state = (await getJson(`${daemon.baseUrl}/state`)).json;
      return findSession(state, res.json.session_id)?.spawn?.status === 'live' || null;
    }, { label: `spawn #${i + 1} reaches status live` });
  }

  // And the capability object reports the fleet size as a plain count, with no
  // budget to run out of.
  const health = (await getJson(`${daemon.baseUrl}/health`)).json;
  assert.equal(health.spawn.active, 8, 'all 8 should be counted live');
  assert.equal(health.spawn.max, undefined, 'no cap should be advertised');
  assert.equal(health.spawn.available, true, '8 live agents must not make spawning "unavailable"');
});

test('fail-loud: worktree:true on a non-git cwd -> 409', async (t) => {
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const daemon = await startDaemon({ env: spawnCmdEnv({ recordFile: rec }) });
  const plain = makePlainDir();
  t.after(async () => { await daemon.stop(); plain.cleanup(); });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: plain.dir, worktree: true });
  assert.equal(res.status, 409, `worktree:true on a non-git cwd should 409 (got ${res.status}: ${JSON.stringify(res.json)})`);
  assert.equal(res.json?.ok, false);
});

// ---------------------------------------------------------------------------
// 4. Worktree path
// ---------------------------------------------------------------------------

test('worktree path: worktree:true creates a sibling --fd-<callsign> checkout on branch fd/<callsign>, and the card collapses to the main tree\'s repo_id', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-spawn-worktree-test' });
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: repo.root, worktree: true });
  assert.equal(res.status, 200, `worktree spawn on a real git repo should 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  const { callsign, session_id: sid } = res.json;

  const expectedWorktreePath = path.join(path.dirname(repo.root), `${path.basename(repo.root)}--fd-${callsign}`);
  await waitUntil(() => existsSync(expectedWorktreePath) || null,
    { label: `sibling worktree directory ${expectedWorktreePath} appears` });

  const listing = execFileSync('git', ['-C', repo.root, 'worktree', 'list', '--porcelain'], { encoding: 'utf8' });
  assert.match(listing, new RegExp(`worktree ${expectedWorktreePath.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`),
    `git worktree list should show the new sibling worktree (listing: ${listing})`);
  assert.match(listing, new RegExp(`branch refs/heads/fd/${callsign.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`),
    `the new worktree's branch should be fd/${callsign} (listing: ${listing})`);

  // Spec cwd (what the daemon handed to FLEETDECK_SPAWN_CMD) should be the
  // NEW worktree path, not the original repo.root.
  const specs = await waitForSpecRecords(rec, 1);
  const cwdSeen = specCwd(specs[specs.length - 1].parsed);
  if (cwdSeen) {
    assert.equal(path.resolve(cwdSeen), path.resolve(expectedWorktreePath),
      'the spec handed to the spawn backend should carry the NEW worktree path as cwd');
  } else {
    t.diagnostic('could not locate a cwd-shaped field in the recorded spec to cross-check against the worktree path (see spec-shape caveat in this file\'s header)');
  }

  // The pre-created card's repo identity must collapse to the SAME repo_id
  // as the main tree (F1: git rev-parse --git-common-dir collapses
  // worktrees), regardless of exactly which cwd the card was created with.
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  assert.ok(card, 'the spawned card should be present in /state');
  assert.equal(card.repo_id, repo.gitCommonDir, "the worktree-spawned card's repo_id must collapse to the main tree's git-common-dir");
});

test('fan-out: 4 agents spawned into ONE repo each get their own worktree and branch', async (t) => {
  // The whole point of a batch: N agents on one repo, none of them standing on
  // another's edits. The board forces worktree:true for a batch; this pins that
  // the daemon can actually deliver N isolated checkouts from one cwd.
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-fanout-test' });
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const prompts = ['fix the flaky test', 'update the README', 'audit the spawn path', 'fix the flaky test'];
  const agents = [];
  for (const prompt of prompts) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: repo.root, worktree: true, prompt });
    assert.equal(res.status, 200, `spawning "${prompt}" into ${repo.root} should 200`);
    agents.push(res.json);
  }

  // Four distinct callsigns → four distinct worktrees, four distinct branches,
  // four distinct tmux windows. Note the last prompt REPEATS the first (the
  // "2x <task>" case): identical task text must still mean separate agents.
  const callsigns = agents.map(a => a.callsign);
  assert.equal(new Set(callsigns).size, 4, `callsigns must be unique, got ${callsigns.join(', ')}`);
  assert.equal(new Set(agents.map(a => a.tmux.window)).size, 4, 'tmux window names must be unique');

  const listing = await waitUntil(() => {
    const out = execFileSync('git', ['-C', repo.root, 'worktree', 'list', '--porcelain'], { encoding: 'utf8' });
    return callsigns.every(cs => out.includes(`refs/heads/fd/${cs}`)) ? out : null;
  }, { label: 'all 4 fd/<callsign> worktrees appear in git worktree list' });

  for (const cs of callsigns) {
    const wt = path.join(path.dirname(repo.root), `${path.basename(repo.root)}--fd-${cs}`);
    assert.ok(existsSync(wt), `worktree ${wt} should exist on disk`);
    assert.match(listing, new RegExp(`branch refs/heads/fd/${cs.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`));
  }

  // ...and all four cards still collapse to the SAME repo, so the board groups
  // them as one fleet working one codebase rather than four unrelated repos.
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  for (const a of agents) {
    assert.equal(findSession(state, a.session_id)?.repo_id, repo.gitCommonDir,
      `${a.callsign} must collapse to the main tree's repo_id`);
  }
});

// ---------------------------------------------------------------------------
// 5. Kill semantics
// ---------------------------------------------------------------------------

test('kill: unknown spawn id -> 404', async (t) => {
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const daemon = await startDaemon({ env: spawnCmdEnv({ recordFile: rec }) });
  t.after(async () => { await daemon.stop(); });

  const res = await postJson(`${daemon.baseUrl}/api/spawn/does-not-exist-${randomUUID()}/kill`, {});
  assert.equal(res.status, 404, `killing an unknown spawn id should 404 (got ${res.status})`);
});

test('kill: a live (non-offline) card is refused without force -> 409', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const cwd = scratchDir();
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const spawnRes = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(spawnRes.status, 200);
  const { spawn_id: spawnId, session_id: sid } = spawnRes.json;

  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return findSession(state, sid)?.spawn?.status === 'live' ? true : null;
  }, { label: 'spawn reaches status live before attempting an unforced kill' });

  const killRes = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/kill`, {});
  assert.equal(killRes.status, 409, `killing a live card without force should 409 (got ${killRes.status}: ${JSON.stringify(killRes.json)})`);
});

test("kill: an offline card's kill is accepted (200) or reports the window already gone (410) -- both are contract-legal here since the override never created a real tmux window", async (t) => {
  // The contract only documents an override for the SPAWN step
  // (FLEETDECK_SPAWN_CMD); kill presumably still name-verifies and shells
  // out to real tmux kill-window. Because this suite never creates a REAL
  // tmux window (creation was intercepted by the override), a real
  // `tmux kill-window` against the recorded window name is expected to find
  // nothing -- which the contract itself calls out as the 410 case ("window
  // already gone"). An implementation may alternatively treat that as a
  // harmless idempotent success (200, row -> killed). Both are accepted;
  // whichever happens is logged via t.diagnostic so a real run pins the
  // actual behavior.
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const cwd = scratchDir();
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const spawnRes = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(spawnRes.status, 200);
  const { spawn_id: spawnId, session_id: sid } = spawnRes.json;

  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return findSession(state, sid)?.spawn?.status === 'live' ? true : null;
  }, { label: 'spawn reaches status live' });

  await postHook(daemon.baseUrl, 'SessionEnd', {
    session_id: sid, hook_event_name: 'SessionEnd', cwd, reason: 'other',
  });
  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return findSession(state, sid)?.col === 'offline' ? true : null;
  }, { label: 'card goes offline after SessionEnd' });

  const killRes = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/kill`, {});
  assert.ok([200, 410].includes(killRes.status),
    `killing an offline card should either succeed (200) or report the window already gone (410); got ${killRes.status}: ${JSON.stringify(killRes.json)}`);
  t.diagnostic(`offline-card kill resolved as HTTP ${killRes.status}: ${JSON.stringify(killRes.json)}`);
  if (killRes.status === 200) assert.equal(killRes.json?.ok, true);
  if (killRes.status === 410) assert.equal(killRes.json?.ok, false);

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  if (card?.spawn?.status) {
    t.diagnostic(`spawn row status after kill attempt: ${card.spawn.status}`);
    assert.ok(['killed', 'gone', 'pane-dead'].includes(card.spawn.status),
      `expected a terminal spawn status after the kill attempt, got ${card.spawn.status}`);
  }
});

// ---------------------------------------------------------------------------
// 6. Stale flag
// ---------------------------------------------------------------------------

test('stale flag: a working card with no events for FLEETDECK_STALE_MS gets stale:true; a fresh event clears it', async (t) => {
  const staleMs = 500;
  const daemon = await startDaemon({ env: { FLEETDECK_STALE_MS: String(staleMs) } });
  const cwd = scratchDir();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, hook_event_name: 'SessionStart', cwd, source: 'startup' });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', { session_id: sid, hook_event_name: 'UserPromptSubmit', cwd, prompt: 'do a thing' });

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  let card = findSession(state, sid);
  assert.equal(card.col, 'working', 'sanity: the session should be working before we let it go stale');
  assert.ok(!card.stale, 'a freshly active card must not be stale yet');

  await new Promise(r => setTimeout(r, staleMs + 500)); // "after ~1s" per the task brief

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'working', 'sanity: still working (no Stop/SessionEnd happened)');
  assert.equal(card.stale, true, `a working card with no events for > ${staleMs}ms should be stale:true`);

  await postHook(daemon.baseUrl, 'PostToolUse', {
    session_id: sid, hook_event_name: 'PostToolUse', cwd, tool_name: 'Read', tool_input: { file_path: path.join(cwd, 'x.txt') },
  });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.ok(!card.stale, 'a fresh event must clear the stale flag');
});

// ---------------------------------------------------------------------------
// 7. Unrouted text
// ---------------------------------------------------------------------------

test('unrouted assign-auto carries the task text verbatim on the response (v1.2 CTA support)', async (t) => {
  const daemon = await startDaemon(); // no sessions at all -> guaranteed zero candidates
  t.after(async () => { await daemon.stop(); });

  const text = 'refactor the flux capacitor for extra gigawatts';
  const res = await postJson(`${daemon.baseUrl}/command`, { text: `assign auto ${text}` });
  assert.equal(res.json?.ok, false);
  assert.equal(res.json?.unrouted, true);
  assert.equal(res.json?.text, text, `the unrouted response should carry the task text verbatim (got: ${JSON.stringify(res.json)})`);
});

// ---------------------------------------------------------------------------
// 8. Reconciliation
// ---------------------------------------------------------------------------

test('reconciliation: a spawn row whose window was never actually created in real tmux flips to gone + card offline after a daemon restart on the same port', async (t) => {
  // What this test CAN prove: the daemon's boot-time reconciliation path
  // runs and marks a stale spawns row + its card as gone/offline when the
  // expected window is absent. What it CANNOT prove without real tmux: that
  // the name-matching is precise (i.e. it wouldn't ALSO mark a genuinely
  // live, differently-named window as gone, or fail to notice a real
  // same-named window that DOES still exist) -- the contract documents
  // no test override for the reconciliation step itself (only for spawn
  // creation), so this exercises the real `tmux list-windows`-style check
  // against a tmux session that was never actually created (since creation
  // went through FLEETDECK_SPAWN_CMD, not real tmux) -- the window is
  // genuinely, not just simulatedly, absent.
  const home = scratchDir('fleetdeck-spawn-home-');
  const cwd = scratchDir();
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  t.after(() => { rmSync(home, { recursive: true, force: true }); rmSync(cwd, { recursive: true, force: true }); });

  const first = await startDaemon({ port, home, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  let sid;
  try {
    const spawnRes = await postJson(`${first.baseUrl}/api/spawn`, { cwd });
    assert.equal(spawnRes.status, 200);
    sid = spawnRes.json.session_id;
    await waitUntil(async () => {
      const state = (await getJson(`${first.baseUrl}/state`)).json;
      return findSession(state, sid)?.spawn?.status === 'live' ? true : null;
    }, { label: 'spawn reaches status live before restart' });
  } finally {
    await first.stop({ keepHome: true });
  }

  const second = await startDaemon({ port, home });
  t.after(async () => { await second.stop({ keepHome: false }); });

  const card = await waitUntil(async () => {
    const state = (await getJson(`${second.baseUrl}/state`)).json;
    const c = findSession(state, sid);
    return c && c.col === 'offline' ? c : null;
  }, { label: 'restart reconciliation marks the orphaned spawn\'s card offline', timeoutMs: 12000 });

  assert.equal(card.col, 'offline', "restart reconciliation should mark a spawn row with no matching real window's card offline");
  if (card.spawn?.status) {
    t.diagnostic(`post-restart spawn row status: ${card.spawn.status}`);
    assert.equal(card.spawn.status, 'gone', "contract rule: a spawning/stalled/live row whose window is gone at boot should become 'gone'");
  } else {
    t.diagnostic('card carries no spawn{} descriptor after restart -- could not directly confirm the row status reached \'gone\' from /state alone; offline col is the observable proxy used here');
  }
});

// ---------------------------------------------------------------------------
// 10. Ticket-first spawn naming (0.6.0)
//
// When the SOURCE cwd's branch carries a Jira key, the spawned session is
// ticket-first: the board callsign stays animal-first (<animal>-PROJ-123) but
// the worktree dir and fd/ branch are composed as
// <repo>--fd-PROJ-123-<animal> / fd/PROJ-123-<animal> so sibling dirs and
// branch lists group by ticket. The tmux window keeps the plain callsign.
// A ticketless spawn is unchanged (--fd-<callsign> / fd/<callsign>).
// ---------------------------------------------------------------------------

test('ticket spawn: a spawn from a ticket-branch cwd is ticket-first (callsign, worktree dir, fd/ branch); its own SessionStart does not re-rename', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const repo = makeRepoWithWorktree({ repoName: 'fd-spawn-ticket', branch: 'feature/PROJ-123-work' });
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const cwd = repo.worktree; // branch feature/PROJ-123-work → ticket PROJ-123
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, worktree: true });
  assert.equal(res.status, 200, `ticket worktree spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  const { callsign, session_id: sid } = res.json;

  // Board callsign stays animal-first, ticket-suffixed.
  assert.match(callsign, /^[a-z]+-PROJ-123$/, `spawned callsign should be <animal>-PROJ-123 (got ${callsign})`);
  const animal = callsign.split('-')[0];

  // The tmux window keeps the plain callsign.
  assert.equal(res.json.tmux.window, `fd${daemon.port}-${callsign}`, 'the tmux window name stays callsign-based');

  // Ticket-first worktree dir + fd/ branch on disk.
  const expectedWorktree = path.join(path.dirname(cwd), `${path.basename(cwd)}--fd-PROJ-123-${animal}`);
  await waitUntil(() => existsSync(expectedWorktree) || null,
    { label: `ticket-first worktree dir ${expectedWorktree} appears` });

  const listing = execFileSync('git', ['-C', repo.root, 'worktree', 'list', '--porcelain'], { encoding: 'utf8' });
  assert.match(listing, new RegExp(`worktree ${expectedWorktree.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`),
    `git worktree list should show the ticket-first worktree (listing: ${listing})`);
  assert.match(listing, new RegExp(`branch refs/heads/fd/PROJ-123-${animal}\\b`),
    `the spawned branch should be fd/PROJ-123-${animal} (listing: ${listing})`);

  // The spawned session's own SessionStart (posted by the fixture; cwd = the new
  // fd/PROJ-123-<animal> worktree) re-yields PROJ-123, so inheritance and
  // self-detection agree and the rename-once guard makes it a no-op.
  const card = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const c = findSession(state, sid);
    return c && c.source === 'hooks' ? c : null;
  }, { label: 'the spawned session flips to hooks after its SessionStart' });
  assert.equal(card.ticket, 'PROJ-123', 'the spawned session inherits the ticket');
  assert.equal(card.callsign, callsign, 'the spawned SessionStart must not re-rename the inherited callsign');
  assert.equal(card.prev_callsign ?? null, null, 'no rename happened → no prev_callsign');
});

test('ticket spawn leftover-artifact: a pre-existing ticket-first worktree dir forces a deduped -<sid4> workname', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  const repo = makeRepoWithWorktree({ repoName: 'fd-spawn-ticket-dedup', branch: 'feature/PROJ-123-work' });
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const cwd = repo.worktree;
  // The FIRST session in a fresh daemon takes falcon (rotation start = count
  // 0 % 12, pinned). Pre-create the exact ticket-first dir the daemon would
  // otherwise choose so the leftover-artifact retry (existsSync → workname +
  // '-' + sid4) fires.
  const collidedDir = path.join(path.dirname(cwd), `${path.basename(cwd)}--fd-PROJ-123-falcon`);
  mkdirSync(collidedDir, { recursive: true });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, worktree: true });
  assert.equal(res.status, 200, `dedup spawn should still 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  const { callsign, session_id: sid } = res.json;
  // Confirm the rotation prediction so the pre-created dir really is the one the
  // daemon wanted (a clear failure here means rotation, not dedup, diverged).
  assert.equal(callsign, 'falcon-PROJ-123', `the first spawn should be falcon-PROJ-123 (got ${callsign})`);
  const sid4 = sid.slice(0, 4);

  // Cross-check the effective worktree cwd the daemon handed the backend: it must
  // be the DEDUPED path (differs from the pre-created dir, carries the -<sid4>).
  const specs = await waitForSpecRecords(rec, 1);
  const actualWorktree = specCwd(specs[specs.length - 1].parsed);
  assert.ok(actualWorktree, 'the spec should carry the effective worktree cwd');
  assert.equal(path.basename(actualWorktree), `${path.basename(cwd)}--fd-PROJ-123-falcon-${sid4}`,
    `the worktree dir should be the deduped -<sid4> workname (got ${actualWorktree})`);
  assert.notEqual(path.resolve(actualWorktree), path.resolve(collidedDir),
    'the deduped worktree dir must differ from the pre-created one');

  await waitUntil(() => existsSync(actualWorktree) || null,
    { label: `deduped worktree dir ${actualWorktree} appears on disk` });
  const listing = execFileSync('git', ['-C', repo.root, 'worktree', 'list', '--porcelain'], { encoding: 'utf8' });
  assert.match(listing, new RegExp(`branch refs/heads/fd/PROJ-123-falcon-${sid4}\\b`),
    `the deduped branch should be fd/PROJ-123-falcon-${sid4} (listing: ${listing})`);
  assert.ok(existsSync(collidedDir), 'the pre-created (non-worktree) dir is left in place');
});

test('ticketless spawn keeps the plain --fd-<callsign> worktree format (no ticket in the path)', async (t) => {
  const port = randomPort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const rec = path.join(scratchDir(), 'specs.jsonl');
  // A repo whose worktree branch carries NO Jira key.
  const repo = makeRepoWithWorktree({ repoName: 'fd-spawn-noticket', branch: 'feature/no-key-here' });
  const daemon = await startDaemon({ port, env: spawnCmdEnv({ recordFile: rec, postUrl: baseUrl }) });
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const cwd = repo.worktree;
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, worktree: true });
  assert.equal(res.status, 200, `ticketless worktree spawn should 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  const { callsign } = res.json;
  assert.match(callsign, /^[a-z]+-[0-9a-f]{4}$/, `a ticketless spawn keeps the hex callsign (got ${callsign})`);

  const expectedWorktree = path.join(path.dirname(cwd), `${path.basename(cwd)}--fd-${callsign}`);
  await waitUntil(() => existsSync(expectedWorktree) || null,
    { label: `plain --fd-<callsign> worktree ${expectedWorktree} appears` });
  const listing = execFileSync('git', ['-C', repo.root, 'worktree', 'list', '--porcelain'], { encoding: 'utf8' });
  assert.match(listing, new RegExp(`branch refs/heads/fd/${callsign.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`),
    'the ticketless spawn branch stays fd/<callsign>');
  assert.ok(!/PROJ-/.test(expectedWorktree), 'no ticket appears in a ticketless worktree path');
});
