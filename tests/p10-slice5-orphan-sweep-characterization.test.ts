// tests/p10-slice5-orphan-sweep-characterization.test.ts
//
// P10 SLICE 5 — orphan-sweep characterization pins (design §1C / §2D).
//
// Slice 5 (orphan sweep -> Schedule/Clock) was ADJUDICATED CONDITIONAL/LAST
// (§6-Q2): "if the analysis finds risk>value, record a DEFER with an explicit
// trigger rather than forcing it." The slice-5 gap analysis found DEFER — the
// sweep is a domain-owned setInterval created inside createQuestions
// (questions.ts:1432), a module that imports zero Effect; it reads the private
// `holds` Map and drives the rearm machinery on every tick. Converting it to a
// P5-family supervised Schedule (background-program.ts / retention-schedule.ts)
// would push an Effect fiber across the imperative hold-manager boundary — a
// BREAK of the Q1-BINDING doctrine ("hold-manager Maps stay imperative; do not
// deepen ownership in P10"). See docs/v1/evidence/effect/p10-slice5-defer.md for
// the full gap analysis, DEFER record, and the explicit re-evaluation trigger.
//
// This file is the slice-0 discipline applied to the DEFER: freeze the sweep's
// observable contract as a characterization oracle BEFORE any future conversion,
// so the eventual port (when its R7/R8 trigger fires) has a byte/behavior floor.
// The design (§2D) names exactly the three properties a conversion MUST keep;
// each is pinned below and, at HEAD, is otherwise UNPINNED as a direct assertion:
//
//   §2D live-socket skip  -> a parked LIVE hold is never expired by the sweep
//                            (the redundancy-with-per-hold-timers proof;
//                            questions.ts:1312 `holds.has(r.id)` continue)
//   §2D redundant cleanup -> an orphaned hold-kind row (no live socket) IS
//                            expired, with onRetired + onChange fired
//                            (questions.ts:1317-1322)
//   §2D audit-silence     -> a throw inside a sweep tick is swallowed by the
//                            `catch {}` (questions.ts:1434-1438); the durable
//                            expiry still commits and the interval survives to
//                            expire a later orphan
//
// The `close()`-cancels-the-sweep teardown and the recycle-aged-rearm path are
// ALREADY pinned (p1-question-retention-lifecycle.test.ts:80 "...cancels ...
// orphan callbacks before DB close"; question-rearm.test.ts:332 "recycled by the
// 5 s orphan sweep"), so this file does not duplicate them.
//
// These run createQuestions over an in-memory DB with a small real sweepMs (the
// same real-timer pattern as p1-question-retention-lifecycle.test.ts) — the
// sweep is a private closure with no public entry, so the real setInterval must
// drive it.

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

// A generous multiple of the small sweepMs below so several ticks are guaranteed
// to have fired inside the window — real-timer slack, not exact cadence.
const SWEEP_MS = 15;
const SETTLE_MS = SWEEP_MS * 12; // ~12 ticks

test('P10 slice5 §2D the orphan sweep skips a LIVE parked hold — no expiry, no {} write (redundant with the per-hold timer)', async () => {
  const db = openDb(':memory:');
  // holdMs far larger than the window: the per-hold expiry timer cannot fire, so
  // the ONLY thing that could retire this row in-window is the sweep — and it
  // must not, because the socket is live (holds.has(id) === true).
  const questions = createQuestions(db, { holdMs: 60_000, rearmGraceMs: 0, sweepMs: SWEEP_MS });
  const sid = 'p10-slice5-live';

  const row = questions.create('permission', sid, {
    tool_name: 'Bash',
    tool_input: { command: 'true' },
  });
  const responses: { body: unknown; status: number }[] = [];
  questions.attachHold(row, (body, status) => responses.push({ body, status }));
  assert.equal(questions.isHeld(row.id), true, 'the hold parks its socket');

  // Let the sweep fire many times over the live hold.
  await delay(SETTLE_MS);

  assert.deepEqual(responses, [], 'the sweep never writes a fail-open {} to a live parked hold');
  assert.equal(
    questions.isHeld(row.id),
    true,
    'the live hold is still held after many sweep ticks',
  );
  assert.equal(
    statusOf(db, row.id),
    'pending',
    'the live hold row stays pending — the sweep skipped it',
  );

  await questions.close(); // releaseAll fails the still-live hold open (200 {}) here, not the sweep
  assert.deepEqual(
    responses,
    [{ body: {}, status: 200 }],
    'the live hold settles 200 {} at close (releaseAll), never mid-sweep',
  );
  db.close();
});

test('P10 slice5 §2D the orphan sweep expires an orphaned hold-kind row (no live socket) and fires onRetired + onChange', async () => {
  const db = openDb(':memory:');
  const retired: number[] = [];
  let changes = 0;
  const questions = createQuestions(db, {
    holdMs: 60_000,
    rearmGraceMs: 0,
    sweepMs: SWEEP_MS,
    onRetired: (r) => {
      // r is the freshly-expired row (or undefined if it vanished); record its id.
      if (r) retired.push(r.id);
    },
    onChange: () => {
      changes++;
    },
  });
  const sid = 'p10-slice5-orphan';

  // A created-but-never-attached hold-kind row is an orphan by construction
  // (attachHold's own contract: a pending hold-kind row without a holds entry is
  // always an orphan). Only the sweep can retire it — no per-hold timer was armed.
  const orphan = questions.create('permission', sid, { tool_name: 'Bash' });
  assert.equal(questions.isHeld(orphan.id), false, 'the orphan parks no socket');
  assert.equal(statusOf(db, orphan.id), 'pending', 'the orphan starts pending');

  await delay(SETTLE_MS);

  assert.equal(
    statusOf(db, orphan.id),
    'expired',
    'the sweep expires the orphaned hold-kind row (restart/disconnect hygiene)',
  );
  assert.deepEqual(
    questions.pendingOf(sid),
    [],
    'no pending hold-kind row survives the sweep once its socket is gone',
  );
  assert.ok(retired.includes(orphan.id), 'onRetired fired with the freshly-expired orphan row');
  assert.ok(changes >= 1, 'onChange fired for the sweep that changed durable state');

  await questions.close();
  db.close();
});

test('P10 slice5 §2D a throwing sweep tick is swallowed (audit-silence): expiry still commits and the interval survives to expire a later orphan', async () => {
  const db = openDb(':memory:');
  // onRetired throws on EVERY invocation. In the real tick this throw propagates
  // out of expireOrphans and is swallowed by `try { expireOrphans() } catch {}`
  // (questions.ts:1434-1438). markExpired runs BEFORE onRetired, so the durable
  // expiry is already committed when the throw happens. Removing that `catch {}`
  // turns this into an uncaught setInterval exception that crashes the sweep —
  // this test is the load-bearing proof of the audit-silent fail-open.
  let onRetiredCalls = 0;
  const questions = createQuestions(db, {
    holdMs: 60_000,
    rearmGraceMs: 0,
    sweepMs: SWEEP_MS,
    onRetired: () => {
      onRetiredCalls++;
      throw new Error('slice5 characterization: retirement callback throws');
    },
  });

  // First orphan: its tick throws inside expireOrphans, after markExpired.
  const first = questions.create('permission', 'p10-slice5-throw-a', { tool_name: 'Bash' });
  await delay(SETTLE_MS);
  assert.equal(
    statusOf(db, first.id),
    'expired',
    'the durable expiry commits even though the retirement callback threw',
  );
  assert.ok(onRetiredCalls >= 1, 'the throwing retirement callback was actually reached');

  // Second orphan created AFTER the first tick already threw. If the throw had
  // escaped `catch {}` it would have crashed the interval and this row would
  // never be swept — expiring it proves the sweep loop survived the earlier throw.
  const second = questions.create('permission', 'p10-slice5-throw-b', { tool_name: 'Bash' });
  await delay(SETTLE_MS);
  assert.equal(
    statusOf(db, second.id),
    'expired',
    'the sweep interval survived the earlier throw and expired a later orphan (audit-silent fail-open)',
  );

  await questions.close();
  db.close();
});
