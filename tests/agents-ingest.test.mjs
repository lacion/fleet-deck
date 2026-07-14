// tests/agents-ingest.test.mjs
//
// Phase 2 daemon feature (handoff F1): `claude agents --json` as a secondary
// session source that catches sessions that predate plugin install — no
// hook ever fired for them. scripts/fleetd/agents-poll.mjs polls the CLI
// (or FLEETDECK_AGENTS_CMD override, see tests/helpers/agents-cmd-fixture.mjs)
// and merges results via derive.mjs's ingestAgentsPoll().
//
// Precedence rule under test throughout: hook-derived state ALWAYS wins.
// The poller may only create cards for sessionIds never seen via hooks
// (source='agents-cli'), and may only update cards whose source is STILL
// 'agents-cli'. The instant a real hook event lands, source flips to
// 'hooks' and the poller must never touch that card again.
//
// Trust rules (install-day fix — the CLI's agent registry lies):
//   1. Only kind='interactive' entries count. Background entries are
//      subagents inside a parent session and the registry keeps them for
//      hours after completion (observed live: phantom WORKING cards).
//   2. An interactive entry needs a LIVE pid — the registry outlives procs.
//   3. Absence from the filtered poll tombstones agents-cli cards ONLY
//      (offline + "no longer reported by agents CLI"); reappearance revives.
//      Hook-sourced cards: SessionEnd stays the only tombstone.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { openDb } from '../scripts/fleetd/db.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeRepoWithWorktree } from './helpers/gitrepo.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const AGENTS_CMD_FIXTURE = path.join(HERE, 'helpers/agents-cmd-fixture.mjs');

// A pid that is definitely alive on this machine: our own test process.
const LIVE_PID = process.pid;

// A pid that is definitely dead: spawn a no-op process, let it exit, reuse
// its pid. (PID reuse within a test run is theoretically possible but
// astronomically unlikely on Linux's sequential allocator.)
function deadPid() {
  const p = spawnSync(process.execPath, ['-e', '']);
  return p.pid;
}

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

function agentsFixtureEnv(fixtureFile, pollMs = 400) {
  return {
    FLEETDECK_AGENTS_CMD: `${process.execPath} ${AGENTS_CMD_FIXTURE}`,
    FLEETDECK_TEST_AGENTS_FIXTURE: fixtureFile,
    FLEETDECK_AGENTS_POLL_MS: String(pollMs),
  };
}

function writeFixture(file, records) {
  writeFileSync(file, JSON.stringify(records));
}

const WAIT_SCALE = Number(process.env.FLEETDECK_TEST_WAIT_SCALE) || 1;

async function waitUntil(fn, { timeoutMs = 8000, intervalMs = 150, label = 'condition' } = {}) {
  const effectiveTimeoutMs = timeoutMs * WAIT_SCALE;
  const deadline = Date.now() + effectiveTimeoutMs;
  for (;;) {
    const result = await fn();
    if (result) return result;
    if (Date.now() >= deadline) throw new Error(`waitUntil: ${label} not met within ${effectiveTimeoutMs}ms`);
    await new Promise(r => setTimeout(r, intervalMs));
  }
}

async function waitForSession(baseUrl, sid, opts) {
  return waitUntil(async () => {
    const state = (await getJson(`${baseUrl}/state`)).json;
    return findSession(state, sid);
  }, { label: `session ${sid} in /state`, ...opts });
}

test('poller cards live interactive entries only: background and dead-pid entries are excluded', async (t) => {
  const scratchDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-agents-scratch-'));
  const fixtureFile = path.join(scratchDir, 'agents.json');
  const repo = makeRepoWithWorktree({ repoName: 'agents-repo-test' });
  t.after(() => {
    repo.cleanup();
    rmSync(scratchDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sidRoot = randomUUID();
  const sidWorktree = randomUUID();
  const sidBackground = randomUUID();
  const sidDeadPid = randomUUID();
  const now = Date.now();

  writeFixture(fixtureFile, [
    // legit: interactive + live pid, main tree, busy
    { pid: LIVE_PID, cwd: repo.root, kind: 'interactive', startedAt: now, sessionId: sidRoot, name: 'root task', status: 'busy' },
    // legit: interactive + live pid, linked worktree, waiting (undocumented state observed live)
    { pid: LIVE_PID, cwd: repo.worktree, kind: 'interactive', startedAt: now, sessionId: sidWorktree, name: 'worktree task', status: 'waiting', waitingFor: 'permission prompt' },
    // registry garbage #1: a background subagent stuck "blocked" (trust rule 1)
    { id: 'bg1', cwd: repo.root, kind: 'background', startedAt: now, sessionId: sidBackground, name: 'stale background agent', state: 'blocked' },
    // registry garbage #2: interactive entry whose process is gone (trust rule 2)
    { pid: deadPid(), cwd: repo.root, kind: 'interactive', startedAt: now, sessionId: sidDeadPid, name: 'dead interactive', status: 'busy' },
  ]);

  const daemon = await startDaemon({ env: agentsFixtureEnv(fixtureFile) });
  t.after(async () => { await daemon.stop(); });

  const rootCard = await waitForSession(daemon.baseUrl, sidRoot);
  const wtCard = await waitForSession(daemon.baseUrl, sidWorktree);

  // col mapping: busy -> working; waiting -> needsyou
  assert.equal(rootCard.col, 'working', 'interactive status=busy should map to col=working');
  assert.equal(wtCard.col, 'needsyou', 'interactive status=waiting should map to col=needsyou');

  for (const [card, name] of [[rootCard, 'root task'], [wtCard, 'worktree task']]) {
    assert.equal(card.source, 'agents-cli');
    assert.equal(card.note, 'seen via agents CLI');
    assert.equal(card.task, name, 'name should map to task');
  }

  // repo identity (F1): root vs. linked worktree collapse to one repo_id
  assert.equal(rootCard.repo_id, repo.gitCommonDir, 'repo_id should be the canonicalized git-common-dir');
  assert.equal(wtCard.repo_id, repo.gitCommonDir, 'worktree session should collapse to the same repo_id');
  assert.equal(rootCard.repo_name, repo.repoName);
  assert.equal(wtCard.repo_name, repo.repoName);
  assert.equal(rootCard.worktree, repo.root);
  assert.equal(wtCard.worktree, repo.worktree);

  // the garbage never appears, even after several more poll cycles
  await new Promise(r => setTimeout(r, 1200));
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(findSession(state, sidBackground), undefined, 'background entries must never be carded (trust rule 1)');
  assert.equal(findSession(state, sidDeadPid), undefined, 'dead-pid interactive entries must never be carded (trust rule 2)');
});

test('a hook event for the same sessionId flips source to hooks and the poller stops touching it', async (t) => {
  const scratchDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-agents-scratch-'));
  const fixtureFile = path.join(scratchDir, 'agents.json');
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(() => {
    rmSync(scratchDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  writeFixture(fixtureFile, [
    { pid: LIVE_PID, cwd, kind: 'interactive', startedAt: Date.now(), sessionId: sid, name: 'predates plugin', status: 'busy' },
  ]);

  const daemon = await startDaemon({ env: agentsFixtureEnv(fixtureFile) });
  t.after(async () => { await daemon.stop(); });

  // Phase 1: the poller discovers the session on its own.
  let card = await waitForSession(daemon.baseUrl, sid);
  assert.equal(card.source, 'agents-cli');
  assert.equal(card.col, 'working', 'status=busy should map to col=working');

  // Phase 2: a real hook arrives -> source flips, SessionStart derives queued.
  const startRes = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  assert.equal(startRes.status, 200);
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.source, 'hooks', 'a real hook event must flip source to hooks');
  assert.equal(card.col, 'queued', 'SessionStart should still derive col=queued as normal');

  // Phase 3a: differing poll state must not move the column any more.
  writeFixture(fixtureFile, [
    { pid: LIVE_PID, cwd, kind: 'interactive', startedAt: Date.now(), sessionId: sid, name: 'predates plugin', status: 'waiting' },
  ]);
  await new Promise(r => setTimeout(r, 1200)); // several 400ms poll cycles

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.source, 'hooks', 'source must remain hooks after further poll ticks');
  assert.equal(card.col, 'queued', 'col must be untouched by the poller once source is hooks');

  // Phase 3b: ABSENCE must not tombstone a hooks-sourced card either
  // (SessionEnd is its only tombstone — trust rule 3 scopes to agents-cli).
  writeFixture(fixtureFile, []);
  await new Promise(r => setTimeout(r, 1200));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'queued', 'absence from the poll must not touch a hooks-sourced card');
  assert.equal(card.endedAt ?? null, null, 'absence from the poll must not tombstone a hooks-sourced card');
});

test('absence tombstones agents-cli cards; reappearance revives them', async (t) => {
  const scratchDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-agents-scratch-'));
  const fixtureFile = path.join(scratchDir, 'agents.json');
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(() => {
    rmSync(scratchDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  const record = { pid: LIVE_PID, cwd, kind: 'interactive', startedAt: Date.now(), sessionId: sid, name: 'ephemeral', status: 'busy' };
  writeFixture(fixtureFile, [record]);

  const daemon = await startDaemon({ env: agentsFixtureEnv(fixtureFile) });
  t.after(async () => { await daemon.stop(); });

  let card = await waitForSession(daemon.baseUrl, sid);
  assert.equal(card.col, 'working');

  // Disappear from the poll -> offline with the honest note.
  writeFixture(fixtureFile, []);
  card = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const c = findSession(state, sid);
    return c && c.col === 'offline' ? c : null;
  }, { label: 'absence tombstone' });
  assert.equal(card.note, 'no longer reported by agents CLI');
  assert.ok(card.endedAt, 'absence tombstone must set endedAt');
  // 0.7.0 Move-to-tmux: absence from one registry poll is a GUESS, not proof
  // of death — the sweep must stamp end_reason='presumed' so the adopt-now
  // allowlist refuses to `claude --resume` a possibly-still-live CLI.
  const dbFile = path.join(daemon.home, 'fleetd.db');
  const stampDb = openDb(dbFile);
  try {
    const row = stampDb.prepare('SELECT end_reason FROM sessions WHERE session_id = ?').get(sid);
    assert.equal(row.end_reason, 'presumed', 'absence tombstone is stamped as a guess');
  } finally { stampDb.close(); }
  assert.equal(card.adopt.eligible, null, 'a presumed absence is never adopt-now-eligible');

  // Reappear -> revived (endedAt cleared, back in a live column).
  writeFixture(fixtureFile, [record]);
  card = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const c = findSession(state, sid);
    return c && c.col === 'working' ? c : null;
  }, { label: 'reappearance revival' });
  assert.equal(card.endedAt ?? null, null, 'reappearance must clear endedAt');
  const revDb = openDb(dbFile);
  try {
    const row = revDb.prepare('SELECT end_reason FROM sessions WHERE session_id = ?').get(sid);
    assert.equal(row.end_reason, null, 'reappearance clears the absence guess');
  } finally { revDb.close(); }
});

test('FLEETDECK_AGENTS_CMD=false disables the poller entirely', async (t) => {
  const scratchDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-agents-scratch-'));
  const fixtureFile = path.join(scratchDir, 'agents.json');
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(() => {
    rmSync(scratchDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  writeFixture(fixtureFile, [
    { pid: LIVE_PID, cwd, kind: 'interactive', startedAt: Date.now(), sessionId: sid, name: 'should never appear', status: 'busy' },
  ]);

  const daemon = await startDaemon({
    env: {
      FLEETDECK_AGENTS_CMD: 'false',
      FLEETDECK_TEST_AGENTS_FIXTURE: fixtureFile,
      FLEETDECK_AGENTS_POLL_MS: '300',
    },
  });
  t.after(async () => { await daemon.stop(); });

  // Give the (disabled) poller ample time to have fired if it were enabled.
  await new Promise(r => setTimeout(r, 2500));

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.sessions.length, 0, 'no sessions should exist when the poller is disabled and no hooks fired');
  const health = await getJson(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200, 'daemon should still be healthy with the poller disabled');
});

test('poll command exiting non-zero harms nothing', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_AGENTS_CMD: 'exit 1', FLEETDECK_AGENTS_POLL_MS: '300' } });
  t.after(async () => { await daemon.stop(); });

  await new Promise(r => setTimeout(r, 2500)); // let several failing ticks run

  const health = await getJson(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200, 'daemon should stay healthy when the poll command fails');
  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.equal(state.status, 200, '/state should still respond fine');
  assert.ok(Array.isArray(state.json.sessions), '/state should still have a sessions array');
});

test('poll command producing garbage (non-JSON) output harms nothing', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_AGENTS_CMD: 'echo not-json-output', FLEETDECK_AGENTS_POLL_MS: '300' } });
  t.after(async () => { await daemon.stop(); });

  await new Promise(r => setTimeout(r, 2500)); // let several garbage ticks run

  const health = await getJson(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200, 'daemon should stay healthy when the poll command emits garbage');
  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.equal(state.status, 200, '/state should still respond fine');
  assert.ok(Array.isArray(state.json.sessions), '/state should still have a sessions array');
});

// 0.6.0: an agents-cli birth follows the same ticket-detection rules as a hook
// birth — the entry's cwd branch is read server-side and, when it carries a
// Jira key, the discovered card is ticketed (<animal>-KEY, ticket_source
// 'branch') instead of hex-suffixed.
test('agents-cli birth on a ticket branch → a ticketed callsign', async (t) => {
  const scratchDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-agents-scratch-'));
  const fixtureFile = path.join(scratchDir, 'agents.json');
  const repo = makeRepoWithWorktree({ repoName: 'agents-ticket-test', branch: 'feature/PROJ-123-agent' });
  t.after(() => { repo.cleanup(); rmSync(scratchDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  writeFixture(fixtureFile, [
    { pid: LIVE_PID, cwd: repo.worktree, kind: 'interactive', startedAt: Date.now(), sessionId: sid, name: 'ticketed agent', status: 'busy' },
  ]);

  const daemon = await startDaemon({ env: agentsFixtureEnv(fixtureFile) });
  t.after(async () => { await daemon.stop(); });

  const card = await waitForSession(daemon.baseUrl, sid);
  assert.equal(card.source, 'agents-cli', 'a poller-discovered card');
  assert.match(card.callsign, /^[a-z]+-PROJ-123$/, `an agents-cli birth on a ticket branch should be ticketed (got ${card.callsign})`);
  assert.equal(card.ticket, 'PROJ-123', 'the ticket is detected from the entry cwd branch');
  assert.equal(card.ticket_source, 'branch');
});
