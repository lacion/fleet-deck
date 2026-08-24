// tests/p10-slice0-hold-manager.test.ts
//
// P10 SLICE 0 — in-process hold-manager characterization pins for the three
// first-settlement legs (design §1A) that the existing suites leave UNPINNED at
// the manager seam. These run createQuestions over an in-memory DB (the same
// pattern as p1-question-retention-lifecycle.test.ts) so they assert the exact
// wire bytes a RespondFn receives, with no daemon and near-zero wall.
//
// The P10 conversions turn each of these settlement paths into a
// Deferred<HookResponse, never> fold; the byte + ordering pins below are the
// floor that fold must reproduce EXACTLY.
//
//   1A.3  socket disconnect on a parked hold  -> releaseHold, NO {} write, NO re-arm
//   1A.4  per-session hold cap eviction        -> OLDEST failed open 200 {}
//   1A.5  board disconnect (failOpenAllHolds)   -> ALL 200 {}, responder-first, no re-arm
//
// COVERAGE NOTE (deviation from the design gap list): 1A.6-HTTP (the D2 shutdown
// flush barrier) is already pinned end-to-end by http-lifecycle.test.ts — it
// proves releaseHolds settles held sockets to 200 {} while the transport is
// alive and the listener is released only after the flush is observable. No add
// here; adding one would duplicate that oracle.

import assert from 'node:assert/strict';
import test from './helpers/harness-test.ts';
import { openDb } from '../src/daemon/db.ts';
import { createQuestions } from '../src/daemon/questions.ts';

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

function statusOf(db: ReturnType<typeof openDb>, id: number): string {
  const row = db.prepare('SELECT status FROM questions WHERE id = ?').get(id) as
    | { status: string }
    | undefined;
  return row?.status ?? '(gone)';
}

test('P10 1A.3 a hook-client disconnect on a parked hold releases it silently — no {} write, no re-arm successor', async () => {
  const db = openDb(':memory:');
  const rearmGraceMs = 40;
  // sweepMs long so the orphan sweep never fires during the test window.
  const questions = createQuestions(db, { holdMs: 60_000, rearmGraceMs, sweepMs: 60_000 });
  const sid = 'p10-socket-disconnect';

  const row = questions.create('permission', sid, {
    tool_name: 'Bash',
    tool_input: { command: 'true' },
  });
  const responses: { body: unknown; status: number }[] = [];
  questions.attachHold(row, (body, status) => responses.push({ body, status }));
  assert.equal(questions.isHeld(row.id), true, 'the hold parks its socket');

  // Peer vanished before an answer (path (c)). socketClosed retires the row
  // WITHOUT writing a fail-open body — the socket is already gone — and WITHOUT
  // arming a re-arm, because there is no live peer to receive a successor.
  questions.socketClosed(row.id);
  assert.deepEqual(responses, [], 'a disconnected hold must never receive a {} write');
  assert.equal(questions.isHeld(row.id), false, 'the disconnected hold is released');
  assert.equal(statusOf(db, row.id), 'expired', 'the disconnected hold row is retired expired');

  // Past the grace window the disconnect path raises no successor — contrast a
  // timer lapse (question-rearm.test.ts pins that settleExpired DOES re-arm).
  await delay(rearmGraceMs * 4 + 60);
  assert.deepEqual(
    questions.pendingOf(sid),
    [],
    'socket disconnect suppresses the re-arm successor',
  );

  // A second close (the normal-completion 'close' that also fires) is a no-op:
  // the hold is already gone, so nothing is written or re-retired.
  questions.socketClosed(row.id);
  assert.deepEqual(responses, [], 'a post-retirement close writes nothing');

  await questions.close();
  db.close();
});

test('P10 1A.4 the per-session hold cap fails the OLDEST hold open as 200 {} to make room for a fifth', async () => {
  const db = openDb(':memory:');
  // rearmGraceMs 0 isolates the eviction wire bytes from the re-arm machinery
  // (scheduleRearm returns false when the grace is disabled).
  const questions = createQuestions(db, { holdMs: 60_000, rearmGraceMs: 0, sweepMs: 60_000 });
  const sid = 'p10-session-cap';

  const holds: { id: number; responses: { body: unknown; status: number }[] }[] = [];
  for (let i = 0; i < 5; i++) {
    const row = questions.create('permission', sid, {
      tool_name: 'Bash',
      tool_input: { command: `c${i}` },
    });
    const responses: { body: unknown; status: number }[] = [];
    questions.attachHold(row, (body, status) => responses.push({ body, status }));
    holds.push({ id: row.id, responses });
  }

  // MAX_HOLDS_PER_SESSION is 4: attaching the 5th failed the OLDEST (first) hold
  // open with the byte-exact 200 {} fail-open, leaving the four newest parked.
  const [oldest, ...rest] = holds;
  assert.ok(oldest, 'the oldest hold was created');
  assert.deepEqual(
    oldest.responses,
    [{ body: {}, status: 200 }],
    'the oldest hold is failed open (200 {}) to make room for the fifth',
  );
  assert.equal(questions.isHeld(oldest.id), false, 'the evicted oldest hold is released');
  assert.equal(statusOf(db, oldest.id), 'expired', 'the evicted hold row is retired expired');
  rest.forEach((h, idx) => {
    const n = idx + 1;
    assert.deepEqual(h.responses, [], `hold #${n} keeps its socket parked (no write)`);
    assert.equal(questions.isHeld(h.id), true, `hold #${n} remains held`);
    assert.equal(statusOf(db, h.id), 'pending', `hold #${n} row stays pending`);
  });

  await questions.close();
  db.close();
});

test('P10 1A.5 failOpenAllHolds (board disconnect) settles every hold 200 {} responder-first and suppresses re-arm', async () => {
  const db = openDb(':memory:');
  const rearmGraceMs = 40;
  const questions = createQuestions(db, { holdMs: 60_000, rearmGraceMs, sweepMs: 60_000 });
  const sids = ['p10-boarddc-a', 'p10-boarddc-b'];

  const observed: {
    sid: string;
    body: unknown;
    status: number;
    heldAtRespond: boolean;
    statusAtRespond: string;
  }[] = [];
  const ids = sids.map((sid) => {
    const row = questions.create('permission', sid, {
      tool_name: 'Bash',
      tool_input: { command: sid },
    });
    questions.attachHold(row, (body, status) => {
      // Captured AT the moment the responder fires, mid-releaseAll.
      observed.push({
        sid,
        body,
        status,
        heldAtRespond: questions.isHeld(row.id),
        statusAtRespond: statusOf(db, row.id),
      });
    });
    return row.id;
  });

  const count = questions.failOpenAllHolds();
  assert.equal(count, 2, 'every parked hold is settled and counted');

  // Responder-first / persist-after ordering: ownership is removed and the {} is
  // written for EVERY hold before any durable row is marked expired. A
  // synchronous close arriving during the loop therefore observes "not held" and
  // cannot double-retire.
  for (const sid of sids) {
    const o = observed.find((e) => e.sid === sid);
    assert.ok(o, `${sid} received its fail-open`);
    assert.deepEqual(
      { body: o.body, status: o.status },
      { body: {}, status: 200 },
      `${sid} fail-open bytes are 200 {}`,
    );
    assert.equal(o.heldAtRespond, false, `${sid} ownership removed before its responder ran`);
    assert.equal(
      o.statusAtRespond,
      'pending',
      `${sid} durable row still pending when its responder ran (persist-after)`,
    );
  }

  for (const id of ids) {
    assert.equal(questions.isHeld(id), false, 'no hold remains after board disconnect');
    assert.equal(statusOf(db, id), 'expired', 'every hold row is retired expired');
  }

  // Board disconnect hands the questions to the terminal and does NOT re-arm
  // (contrast settleExpired). Nothing new appears past the grace window.
  await delay(rearmGraceMs * 4 + 60);
  for (const sid of sids) {
    assert.deepEqual(
      questions.pendingOf(sid),
      [],
      `${sid} board disconnect suppresses the re-arm successor`,
    );
  }

  await questions.close();
  db.close();
});
