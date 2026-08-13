// tests/audit-cleanup.test.ts
//
// Regression coverage for the audit-cleanup fixes that are awkward to prove
// through the daemon's HTTP surface: cache invalidation after git init,
// transcript append stability/read tiers, and poller scheduling concurrency.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs, { appendFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { branchOf, deriveRepo } from '../scripts/fleetd/repo-identity.ts';
import { lastAssistantModel, lastAssistantText } from '../scripts/fleetd/transcript.ts';

function scratch(t: TestContext, prefix: string): string {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

function gitInit(dir: string): void {
  execFileSync('git', ['init', '-q', dir], { stdio: 'ignore' });
}

function assistant(text: string, model = 'claude-opus-test'): string {
  return JSON.stringify({
    type: 'assistant',
    message: { role: 'assistant', model, content: [{ type: 'text', text }] },
  });
}

test('repo identity retries a cached non-git directory after its short TTL', async (t) => {
  const dir = scratch(t, 'fleetdeck-identity-negative-');
  assert.equal(deriveRepo(dir).is_git, false);

  gitInit(dir);
  assert.equal(
    deriveRepo(dir).is_git,
    false,
    'the short quiet-period cache may still serve immediately',
  );
  await new Promise((resolve) => setTimeout(resolve, 2_100));
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
  assert.equal(
    deriveRepo(missing).is_git,
    true,
    'a formerly missing cwd must be checked immediately once created',
  );
});

test('a malformed final transcript record never resurrects older assistant text', (t) => {
  const dir = scratch(t, 'fleetdeck-transcript-partial-');
  const file = path.join(dir, 'session.jsonl');
  writeFileSync(file, `${assistant('Should I deploy the old build?')}\n`);
  appendFileSync(file, '{"type":"assistant","message":{"role":"assistant","content":');

  assert.equal(
    lastAssistantText(file),
    null,
    'the newest append is not stable enough to answer from history',
  );
});

test('assistant text scan skips large older non-assistant tail rows without losing the newest assistant', (t) => {
  const dir = scratch(t, 'fleetdeck-transcript-filter-');
  const file = path.join(dir, 'session.jsonl');
  const largeRows = Array.from({ length: 3 }, (_, index) =>
    JSON.stringify({ type: 'user', tool_result: 'x'.repeat(100_000), index }),
  );
  writeFileSync(file, `${assistant('Newest assistant answer')}\n${largeRows.join('\n')}\n`);

  const jsonPatch = JSON as { parse: typeof JSON.parse };
  const originalParse = JSON.parse;
  let parses = 0;
  jsonPatch.parse = ((...args: Parameters<typeof JSON.parse>): unknown => {
    parses++;
    return originalParse(...args) as unknown;
  }) as typeof JSON.parse;
  try {
    assert.equal(lastAssistantText(file), 'Newest assistant answer');
  } finally {
    jsonPatch.parse = originalParse;
  }
  assert.equal(parses, 2, 'only the newest row and eventual assistant row should need parsing');
});

test('assistant text scan ignores unread zero-fill after a short transcript read', (t) => {
  const dir = scratch(t, 'fleetdeck-transcript-short-read-');
  const file = path.join(dir, 'session.jsonl');
  writeFileSync(file, `${assistant('Complete final answer')}\n`);

  const fsPatch = fs as { statSync: typeof fs.statSync };
  const originalStatSync = fs.statSync;
  fsPatch.statSync = function inflatedStat(...args: unknown[]): unknown {
    const stat = (originalStatSync as (...a: unknown[]) => { size: number })(...args);
    return { ...stat, size: stat.size + 64 };
  } as unknown as typeof fs.statSync;
  try {
    assert.equal(lastAssistantText(file), 'Complete final answer');
  } finally {
    fsPatch.statSync = originalStatSync;
  }
});

test('model tracking reads only the 16 KB first tier when the newest assistant is nearby', (t) => {
  const dir = scratch(t, 'fleetdeck-transcript-tier-');
  const file = path.join(dir, 'session.jsonl');
  const hugeOldToolResult = JSON.stringify({ type: 'user', bulk: 'x'.repeat(300_000) });
  writeFileSync(file, `${hugeOldToolResult}\n${assistant('done', 'claude-nearby-model')}\n`);

  const fsPatch = fs as { readSync: typeof fs.readSync };
  const originalReadSync = fs.readSync;
  const readSizes: number[] = [];
  fsPatch.readSync = function trackedRead(fd: number, buffer: Buffer, ...args: unknown[]): number {
    readSizes.push(buffer.length);
    return (originalReadSync as (...a: unknown[]) => number)(fd, buffer, ...args);
  } as unknown as typeof fs.readSync;
  try {
    assert.equal(lastAssistantModel(file), 'claude-nearby-model');
  } finally {
    fsPatch.readSync = originalReadSync;
  }
  // The second read is the single boundary-check byte (BUG-194): is the row
  // at the window's edge complete? The point stands — no 256 KB first tier.
  assert.deepEqual(
    readSizes,
    [16_384, 1],
    'the common case must not start with a 256 KB tail read',
  );
});

test('agents polling is single-flight and backs off the CLI while liveness stays responsive', async (t) => {
  const dir = scratch(t, 'fleetdeck-agents-schedule-');
  const runner = path.join(dir, 'slow-poll.mjs');
  const log = path.join(dir, 'poll.log');
  writeFileSync(
    runner,
    [
      "import { appendFileSync } from 'node:fs';",
      'const log = process.argv[2];',
      'appendFileSync(log, `start ${Date.now()}\\n`);',
      'setTimeout(() => {',
      '  appendFileSync(log, `end ${Date.now()}\\n`);',
      "  console.log('[]');",
      '}, 250);',
    ].join('\n'),
  );

  // These three tune the in-process poller for this test only. Save and restore
  // them: `bun test` shares ONE process across all files, so an unrestored set
  // leaks into a sibling test's daemon (the CMD would even point at this test's
  // already-deleted scratch dir). `node --test` forks a child per file and never
  // saw the leak; this restore is a no-op there and keeps both runtimes green.
  const prevAgentsEnv = {
    poll: process.env['FLEETDECK_AGENTS_POLL_MS'],
    idle: process.env['FLEETDECK_AGENTS_IDLE_POLL_MS'],
    cmd: process.env['FLEETDECK_AGENTS_CMD'],
  };
  t.after(() => {
    const restore = (key: string, was: string | undefined) => {
      if (was === undefined) delete process.env[key];
      else process.env[key] = was;
    };
    restore('FLEETDECK_AGENTS_POLL_MS', prevAgentsEnv.poll);
    restore('FLEETDECK_AGENTS_IDLE_POLL_MS', prevAgentsEnv.idle);
    restore('FLEETDECK_AGENTS_CMD', prevAgentsEnv.cmd);
  });
  process.env['FLEETDECK_AGENTS_POLL_MS'] = '100';
  process.env['FLEETDECK_AGENTS_IDLE_POLL_MS'] = '500';
  // FLEETDECK_AGENTS_CMD is whitespace-tokenized and run WITHOUT a shell, so
  // these tmpdir/execPath paths (all quote-free) are passed as-is; wrapping
  // them in quotes would make the quote characters literal argv bytes → ENOENT.
  process.env['FLEETDECK_AGENTS_CMD'] = `${process.execPath} ${runner} ${log}`;
  const { startAgentsPoll } = (await import(
    `../scripts/fleetd/agents-poll.ts?audit=${Date.now()}`
  )) as typeof import('../scripts/fleetd/agents-poll.ts');

  let livenessTicks = 0;
  const poller = startAgentsPoll({
    ingestAgentsPoll() {
      /* no ingest under test */
    },
    spawnLivenessTick() {
      livenessTicks++;
      return Promise.resolve();
    },
  });
  t.after(() => {
    poller.stop();
  });
  await new Promise((resolve) => setTimeout(resolve, 1_250));
  poller.stop();

  const events = fs
    .readFileSync(log, 'utf8')
    .trim()
    .split('\n')
    .map((line) => {
      const [kind, at] = line.split(' ');
      return { kind, at: Number(at) };
    });
  const starts = events.filter((event) => event.kind === 'start');
  assert.equal(starts.length, 2, 'an empty fleet should run the CLI at the idle cadence');
  let concurrent = 0;
  let peak = 0;
  for (const event of events) {
    concurrent += event.kind === 'start' ? 1 : -1;
    peak = Math.max(peak, concurrent);
  }
  assert.equal(peak, 1, 'a slow poll command must never overlap another tick');
  assert.ok(
    livenessTicks >= 5,
    'the cheap owned-pane sweep must keep its active cadence during CLI backoff',
  );
});
