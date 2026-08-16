// tests/ts-migration-coercion.test.ts
//
// Regression pins for the JS→TS "coercion drift" class the adversarial review
// surfaced. The migration mechanically rewrote the old defensive `String(x ??
// '')` / `x || y` into "trust the (now-erased) TS type" — correct for callers
// the compiler actually narrows, but WRONG at the untrusted seams where the
// value arrives from the wire (hook payloads, the `claude agents --json`
// poller, board POST bodies) and the compile-time type is a promise the runtime
// never made. A null/number where a string was declared then threw, and the
// daemon's fail-open swallowed the throw into DROPPED telemetry — silent, not a
// crash. These tests feed each seam the poisoned shape and assert the pre-TS
// behaviour: coerce and carry on, never drop.
//
// Coverage:
//   1. helpers.colFromAgentState — a null/absent agents-cli state (pure)
//   2. questions.answer — a non-string board body / stored text on the freeform
//      AND the re-armed-choice (answer-after-hold-expiry) mail paths (db)
//   3. events.applyEvent — SessionStart model:null, a non-string Bash command,
//      and a non-string AskUserQuestion (integration)

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { rmSync } from 'node:fs';

import { openDb } from '../src/daemon/db.ts';
import { createQuestions } from '../src/daemon/questions.ts';
import { colFromAgentState } from '../src/daemon/helpers.ts';

import { startDaemon, type DaemonHandle } from './helpers/daemon.ts';
import { postHook, postJson } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { getState, scratchCwd, findSession, getSession, questionsFor } from './helpers/state.ts';
import { waitUntil, scaleMs } from './helpers/wait.ts';
import type { StateResponse } from '../contracts/state.ts';

// ---------------------------------------------------------------------------
// 1. colFromAgentState — the poller feeds it whatever `agents --json` printed,
//    including a null/absent `state`. Pre-TS `String(raw ?? '')` tolerated it;
//    a bare `raw.toLowerCase()` would throw and strand the poll.
// ---------------------------------------------------------------------------
test('colFromAgentState: the documented states still map, case-insensitively', () => {
  assert.equal(colFromAgentState('busy', false), 'working');
  assert.equal(colFromAgentState('running', false), 'working');
  assert.equal(colFromAgentState('BLOCKED', false), 'needsyou');
  assert.equal(colFromAgentState('waiting', false), 'needsyou');
  assert.equal(colFromAgentState('idle', false), 'idle');
});

test('colFromAgentState: a null/undefined or non-string state never throws — it falls back by newness', () => {
  // Contract check, NOT a regression pin: pre-fix `(raw ?? '').toLowerCase()`
  // already mapped null/undefined to '' and never threw, so these four pass on
  // either side of the fix. They pin the fallback mapping (queued for a fresh
  // card, idle for a known one), which the String() change must not disturb.
  assert.equal(colFromAgentState(null, true), 'queued');
  assert.equal(colFromAgentState(null, false), 'idle');
  assert.equal(colFromAgentState(undefined, true), 'queued');
  assert.equal(colFromAgentState(undefined, false), 'idle');
  // THE regression pin: a non-null non-string is what the migration broke. The
  // `agents --json` poller's type erases to `string`, but a number on the wire
  // hit `(123).toLowerCase()` and threw pre-fix, stranding the poll. String()
  // coerces it, so it matches nothing and takes the same unknown-state fallback.
  assert.equal(colFromAgentState(123 as unknown as string, true), 'queued');
});

// ---------------------------------------------------------------------------
// 2. questions.answer (freeform) — the board POSTs {text} and the stored row
//    carries the question text; both are untrusted. A non-string either side
//    used to 500 (`body.text.trim()` / `stored.slice()` on a number). asText()
//    restores the pre-TS coercion.
// ---------------------------------------------------------------------------
test('questions.answer: a freeform answer with a non-string body/stored text coerces to a 200, not a 500', () => {
  const db = openDb(':memory:');
  const mails: Array<{ sid: string; tag: string; body: string }> = [];
  const questions = createQuestions(db, {
    mail: (sid, tag, body) => {
      mails.push({ sid, tag, body });
    },
  });

  // A freeform row whose STORED question text is a number (untrusted payload).
  const row = questions.create('freeform', 'sess-1', { text: 999 });
  // Answer with a non-string body.text. JSON.parse mirrors the untrusted wire
  // body the HTTP handler hands answer() — `any`, not the declared {text:string}.
  const res = questions.answer(row.id, JSON.parse('{"text":12345}'));

  assert.equal(res.status, 200, 'a non-string answer must not throw into a 500');
  assert.equal(res.body.ok, true);
  assert.equal(mails.length, 1, 'the coerced answer is framed and mailed to the session');
  assert.equal(mails[0]?.tag, 'fleetdeck-answer');
  assert.equal(
    mails[0]?.body,
    '[FLEETDECK ANSWER] Q: 999 — A: 12345',
    'both the stored question and the answer body are String()-coerced into the frame',
  );
});

// A re-armed hold (the original window expired, the native terminal owns the
// decision, the answer rides mail) frames the STORED question text — untrusted,
// so it too can be a number. questions.ts:772 asText()s it before .slice; the
// live-hold choice test above never reaches this branch (it answers in-window).
test('questions.answer: a re-armed choice with a non-string stored question frames a 200, not a 500', () => {
  const db = openDb(':memory:');
  const mails: Array<{ sid: string; tag: string; body: string }> = [];
  const questions = createQuestions(db, {
    mail: (sid, tag, body) => {
      mails.push({ sid, tag, body });
    },
  });

  // rearmed:true takes the mail branch with no live socket; the stored question
  // is a number. Pre-fix `first.slice(...)` on that number threw into a 500 and
  // the answer was never framed or mailed for the next turn boundary.
  const row = questions.create('choice', 'sess-2', {
    rearmed: true,
    tool_input: {
      questions: [
        {
          question: 999,
          header: 'x',
          multiSelect: false,
          options: [{ label: 'a', description: 'd' }],
        },
      ],
    },
  });
  const res = questions.answer(row.id, { text: 'ok' });

  assert.equal(
    res.status,
    200,
    'a re-armed answer with a non-string question must not throw into a 500',
  );
  assert.equal(res.body.ok, true);
  assert.equal(
    mails.length,
    1,
    'the coerced answer is framed and mailed for the next turn boundary',
  );
  assert.equal(mails[0]?.tag, 'fleetdeck-answer');
  assert.equal(
    mails[0]?.body,
    '[FLEETDECK ANSWER] choice (answered after the hold expired) Q: 999 — A: ok',
    'the numeric stored question is String()-coerced into the re-armed frame',
  );
});

// ---------------------------------------------------------------------------
// 3. events.applyEvent — the hot path off every hook payload. A throw here is
//    swallowed by http.ts's fail-open (200 {}), so the ONLY observable is the
//    derived state: a dropped event never increments the counter, sets the
//    note, or parks the hold.
// ---------------------------------------------------------------------------
async function eventsHarness(t: TestContext): Promise<{
  daemon: DaemonHandle;
  cwd: string;
  sid: string;
}> {
  const daemon = await startDaemon();
  const cwd = scratchCwd('fleetdeck-tscoerce-cwd-');
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { daemon, cwd, sid: randomUUID() };
}

test('events: a SessionStart carrying model:null is applied, not dropped', async (t) => {
  const { daemon, cwd, sid } = await eventsHarness(t);
  // Pre-fix `typeof m === 'object'` (no `m &&`) dereferenced null.display_name
  // and threw before the counter/model updates flushed.
  const res = await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }, { model: null }),
    { token: daemon },
  );
  assert.equal(res.status, 200, 'hooks always fail open');

  const s = getSession(await getState<StateResponse>(daemon.baseUrl), sid);
  assert.equal(s.model, null, 'a null model stays null — never a crash or the string "undefined"');
  assert.ok(
    s.events >= 1,
    'applyEvent ran to completion: the event counter incremented past the model block',
  );
});

test('events: a PostToolUse Bash with a non-string command is coerced, not dropped', async (t) => {
  const { daemon, cwd, sid } = await eventsHarness(t);
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );
  // Pre-fix `input.command.slice(...)` on a number threw; the note never landed.
  const res = await postHook(
    daemon.baseUrl,
    'PostToolUse',
    loadFixture('post-tool-use-bash', { session_id: sid, cwd }, { tool_input: { command: 12345 } }),
    { token: daemon },
  );
  assert.equal(res.status, 200);

  const s = getSession(await getState<StateResponse>(daemon.baseUrl), sid);
  assert.equal(s.note, 'sh: 12345', 'the numeric command is String()-coerced before .slice');
});

test('events: an AskUserQuestion whose question is non-string still holds as a choice', async (t) => {
  const holdMs = scaleMs(4000);
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd('fleetdeck-tscoerce-cwd-');
  const sid = randomUUID();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  // events.ts runs applyEvent (which emits the join tick) BEFORE questions.create,
  // so a tick that threw on a non-string question dropped the WHOLE choice — it
  // would never appear in /state. Pre-fix: `first.slice(0,50)` on a number.
  const held = postHook(
    daemon.baseUrl,
    'AskUserQuestion',
    loadFixture(
      'ask-user-question',
      { session_id: sid, cwd },
      {
        tool_input: {
          questions: [
            {
              question: 999,
              header: 'x',
              multiSelect: false,
              options: [{ label: 'a', description: 'd' }],
            },
          ],
        },
      },
    ),
    { token: daemon, timeout: holdMs + 5000 },
  );

  const q = await waitUntil(
    async () => questionsFor(await getState<StateResponse>(daemon.baseUrl), sid, 'choice')[0],
    { label: 'non-string choice question to appear in /state' },
  );
  assert.ok(q, 'the choice held — a thrown tick would have dropped it before questions.create');

  const state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(
    findSession(state, sid)?.col,
    'needsyou',
    'the held choice moves the card to needsyou',
  );
  assert.ok(
    state.ticker.some((tk) => tk.msg.includes('asks:')),
    'the join tick fired with the coerced question text',
  );

  // Settle the hold so the parked request drains before the daemon stops.
  await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: 'ok' });
  await held;
});
