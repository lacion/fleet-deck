import test from 'node:test';
import assert from 'node:assert/strict';
import { openDb } from '../scripts/fleetd/db.mjs';
import { createQuestions } from '../scripts/fleetd/questions.mjs';

test('M-B1: correlated activity expires only its matching hold and leaves a parallel sibling pending', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });
  const replies = new Map([['tool-a', []], ['tool-b', []]]);
  const first = questions.create('permission', 'same-session', { tool_use_id: 'tool-a' });
  const sibling = questions.create('permission', 'same-session', { tool_use_id: 'tool-b' });
  questions.attachHold(first, body => replies.get('tool-a').push(body));
  questions.attachHold(sibling, body => replies.get('tool-b').push(body));

  assert.equal(questions.expireOnActivity('same-session', { toolUseId: 'tool-a' }), true);
  assert.deepEqual(replies.get('tool-a'), [{}], 'the completed request must fail open promptly');
  assert.deepEqual(replies.get('tool-b'), [], 'the unrelated parallel request must remain held');

  const rows = questions.pendingOf('same-session');
  assert.deepEqual(rows.map(row => row.id), [sibling.id]);
  assert.equal(rows[0].status, 'pending');

  questions.dismiss(sibling.id);
  db.close();
});

test('M-B7: a timer persistence failure still fails the hold open without an uncaught exception', async () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 25 });
  const row = questions.create('permission', 'timer-session', { tool_use_id: 'timer-tool' });
  let resolveReply;
  const reply = new Promise(resolve => { resolveReply = resolve; });
  const logged = [];
  const originalError = console.error;
  console.error = (...args) => logged.push(args);
  // attachHold intentionally unrefs production timers. Keep this short unit
  // test alive long enough to observe that callback's error boundary.
  const keepAlive = setTimeout(() => {}, 250);
  try {
    questions.attachHold(row, resolveReply);
    db.close(); // force markExpired.run() in the timer to throw
    assert.deepEqual(await reply, {}, 'persistence failure must not leave the hook caller hanging');
    await new Promise(resolve => setTimeout(resolve, 25));
    assert.equal(logged.length, 1);
    assert.match(String(logged[0][0]), /expiry persistence error/);
  } finally {
    clearTimeout(keepAlive);
    console.error = originalError;
  }
});

test('dismiss resolves callsign through the session lookup instead of a nonexistent question column', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, {
    callsignOf: sessionId => sessionId === 'known-session' ? 'viper' : null,
  });
  const row = questions.create('freeform', 'known-session', { text: 'Still needed?' });

  assert.deepEqual(questions.dismiss(row.id), { ok: true, callsign: 'viper' });
  db.close();
});
