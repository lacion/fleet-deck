// tests/audit-cleanup.test.mjs
//
// Regression coverage for the audit-cleanup fixes that are awkward to prove
// through the daemon's HTTP surface: cache invalidation after git init,
// transcript append stability/read tiers, and poller scheduling concurrency.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs, { appendFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { branchOf, deriveRepo } from '../scripts/fleetd/repo-identity.mjs';
import { lastAssistantModel, lastAssistantText } from '../scripts/fleetd/transcript.mjs';

function scratch(t, prefix) {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function gitInit(dir) {
  execFileSync('git', ['init', '-q', dir], { stdio: 'ignore' });
}

function assistant(text, model = 'claude-opus-test') {
  return JSON.stringify({
    type: 'assistant',
    message: { role: 'assistant', model, content: [{ type: 'text', text }] },
  });
}

test('repo identity retries a cached non-git directory after its short TTL', async (t) => {
  const dir = scratch(t, 'fleetdeck-identity-negative-');
  assert.equal(deriveRepo(dir).is_git, false);

  gitInit(dir);
  assert.equal(deriveRepo(dir).is_git, false, 'the short quiet-period cache may still serve immediately');
  await new Promise(resolve => setTimeout(resolve, 2_100));
  assert.equal(deriveRepo(dir).is_git, true, 'git init must become visible without daemon restart');
});

test('invalid cwd values skip git and are not cached across later directory creation', (t) => {
  const parent = scratch(t, 'fleetdeck-identity-missing-');
  const missing = path.join(parent, 'created-later');
  const regularFile = path.join(parent, 'not-a-directory');
  writeFileSync(regularFile, 'plain file');

  assert.equal(deriveRepo(missing).is_git, false);
  assert.equal(branchOf(missing), null);
  assert.equal(deriveRepo(regularFile).is_git, false);
  assert.equal(branchOf(regularFile), null);

  mkdirSync(missing);
  gitInit(missing);
  assert.equal(deriveRepo(missing).is_git, true, 'a formerly missing cwd must be checked immediately once created');
});

test('a malformed final transcript record never resurrects older assistant text', (t) => {
  const dir = scratch(t, 'fleetdeck-transcript-partial-');
  const file = path.join(dir, 'session.jsonl');
  writeFileSync(file, `${assistant('Should I deploy the old build?')}\n`);
  appendFileSync(file, '{"type":"assistant","message":{"role":"assistant","content":');

  assert.equal(lastAssistantText(file), null, 'the newest append is not stable enough to answer from history');
});

test('model tracking reads only the 16 KB first tier when the newest assistant is nearby', (t) => {
  const dir = scratch(t, 'fleetdeck-transcript-tier-');
  const file = path.join(dir, 'session.jsonl');
  const hugeOldToolResult = JSON.stringify({ type: 'user', bulk: 'x'.repeat(300_000) });
  writeFileSync(file, `${hugeOldToolResult}\n${assistant('done', 'claude-nearby-model')}\n`);

  const originalReadSync = fs.readSync;
  const readSizes = [];
  fs.readSync = function trackedRead(fd, buffer, ...args) {
    readSizes.push(buffer.length);
    return originalReadSync.call(this, fd, buffer, ...args);
  };
  try {
    assert.equal(lastAssistantModel(file), 'claude-nearby-model');
  } finally {
    fs.readSync = originalReadSync;
  }
  assert.deepEqual(readSizes, [16_384], 'the common case must not start with a 256 KB tail read');
});

test('agents polling is single-flight and backs off the CLI while liveness stays responsive', async (t) => {
  const dir = scratch(t, 'fleetdeck-agents-schedule-');
  const runner = path.join(dir, 'slow-poll.mjs');
  const log = path.join(dir, 'poll.log');
  writeFileSync(runner, [
    "import { appendFileSync } from 'node:fs';",
    "const log = process.argv[2];",
    "appendFileSync(log, `start ${Date.now()}\\n`);",
    "setTimeout(() => {",
    "  appendFileSync(log, `end ${Date.now()}\\n`);",
    "  console.log('[]');",
    "}, 250);",
  ].join('\n'));

  process.env.FLEETDECK_AGENTS_POLL_MS = '100';
  process.env.FLEETDECK_AGENTS_IDLE_POLL_MS = '500';
  process.env.FLEETDECK_AGENTS_CMD = `"${process.execPath}" "${runner}" "${log}"`;
  const { startAgentsPoll } = await import(`../scripts/fleetd/agents-poll.mjs?audit=${Date.now()}`);

  let livenessTicks = 0;
  const poller = startAgentsPoll({
    ingestAgentsPoll() {},
    async spawnLivenessTick() { livenessTicks++; },
  });
  t.after(() => poller.stop());
  await new Promise(resolve => setTimeout(resolve, 1_250));
  poller.stop();

  const events = fs.readFileSync(log, 'utf8').trim().split('\n').map(line => {
    const [kind, at] = line.split(' ');
    return { kind, at: Number(at) };
  });
  const starts = events.filter(event => event.kind === 'start');
  assert.equal(starts.length, 2, 'an empty fleet should run the CLI at the idle cadence');
  let concurrent = 0;
  let peak = 0;
  for (const event of events) {
    concurrent += event.kind === 'start' ? 1 : -1;
    peak = Math.max(peak, concurrent);
  }
  assert.equal(peak, 1, 'a slow poll command must never overlap another tick');
  assert.ok(livenessTicks >= 5, 'the cheap owned-pane sweep must keep its active cadence during CLI backoff');
});
