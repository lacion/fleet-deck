// tests/questions-audit.test.mjs
//
// M-B1 (R4-1/R7-1): expireOnActivity must not over-expire parallel-sibling
// holds. These tests drive REAL hook fixtures through create()/expireOnActivity
// — no hand-injected tool_use_id — to prove the correlation works against the
// payloads that actually occur. The crucial fact the earlier implementation
// missed: real PostToolUse AND PermissionRequest payloads carry NO tool_use_id,
// so correlation must key on the identity they DO share — (tool_name,
// tool_input). See tests/fixtures/{permission-request,post-tool-use-*}.json.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.ts';
import { createQuestions } from '../scripts/fleetd/questions.ts';

const FIX = path.join(path.dirname(fileURLToPath(import.meta.url)), 'fixtures');

// The SAME bytes the daemon receives from the hook client, placeholders
// resolved. Used as the create() payload so the question row stores exactly
// what a real PermissionRequest/Elicitation would leave on it.
function fixture(name, { session = 'sess-1', cwd = '/work' } = {}) {
  const raw = readFileSync(path.join(FIX, `${name}.json`), 'utf8')
    .replaceAll('__SESSION__', session).replaceAll('__CWD__', cwd);
  return JSON.parse(raw);
}

// Verify at the source: a permission hold's stored (tool_name, tool_input) is
// exactly what its completing PostToolUse carries. The real fixtures use
// different sample commands (permission = `rm -rf build/`, post-tool = `ls`),
// but for the SAME tool call Claude Code sends the input verbatim on both, so
// correlation compares the permission fixture's own tool_name/tool_input.
test('M-B1: a real permission fixture carries no tool_use_id, and PostToolUse fixtures carry none either', () => {
  const perm = fixture('permission-request');
  assert.equal(perm.tool_use_id, undefined, 'PermissionRequest fixture has no tool_use_id');
  assert.equal(perm.tool_name, 'Bash');
  assert.ok(perm.tool_input && typeof perm.tool_input === 'object', 'it does carry tool_name + tool_input');
  for (const f of ['post-tool-use-bash', 'post-tool-use-edit']) {
    assert.equal(fixture(f).tool_use_id, undefined, `${f} fixture has no tool_use_id — the old correlation key never existed`);
  }
});

test('M-B1 (a): a permission hold is expired by ITS completing PostToolUse — correlated on (tool_name, tool_input)', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });

  const perm = fixture('permission-request', { session: 's1' }); // Bash `rm -rf build/`
  const replies = [];
  const row = questions.create('permission', 's1', perm);
  questions.attachHold(row, body => replies.push(body));

  // The human approves in the terminal; the tool runs; its PostToolUse carries
  // the SAME tool_name + tool_input (Claude Code sends them verbatim on both).
  const changed = questions.expireOnActivity('s1', { toolName: perm.tool_name, toolInput: perm.tool_input });

  assert.equal(changed, true, 'the matching hold is settled');
  assert.deepEqual(replies, [{}], 'the completed request is failed open promptly');
  assert.deepEqual(questions.pendingOf('s1'), [], 'no pending hold remains');
  db.close();
});

test('M-B1 (b): a completing PostToolUse expires only its own hold; a parallel sibling for a DIFFERENT tool call is preserved', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });

  // Two parallel permission holds, same tool NAME (Bash) but different input —
  // proving correlation keys on tool_input, not just tool_name.
  const permA = fixture('permission-request', { session: 's1' });          // Bash `rm -rf build/`
  const permB = fixture('permission-request', { session: 's1' });
  permB.tool_input = { command: 'git push --force origin main' };          // a different call
  const repliesA = [];
  const repliesB = [];
  const rowA = questions.create('permission', 's1', permA);
  const rowB = questions.create('permission', 's1', permB);
  questions.attachHold(rowA, b => repliesA.push(b));
  questions.attachHold(rowB, b => repliesB.push(b));

  // A freeform question in the same session has no tool identity and must never
  // be touched by a tool completion.
  const free = questions.create('freeform', 's1', { text: 'Ship it?' });

  const changed = questions.expireOnActivity('s1', { toolName: permA.tool_name, toolInput: permA.tool_input });

  assert.equal(changed, true);
  assert.deepEqual(repliesA, [{}], 'A (the completed call) is failed open');
  assert.deepEqual(repliesB, [], 'B (a different parallel call) is left held');
  const pending = questions.pendingOf('s1').map(r => r.id).sort((x, y) => x - y);
  assert.deepEqual(pending, [rowB.id, free.id].sort((x, y) => x - y),
    'only A expired; the sibling hold AND the freeform row survive');

  questions.dismiss(rowB.id);
  db.close();
});

test('BUG 5: IDENTICAL-input parallel holds — a single PostToolUse expires exactly ONE, its twin is preserved', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });

  // Two parallel permission holds for the SAME tool call — identical tool_name
  // AND tool_input — so they share a toolCallKey. A single completing
  // PostToolUse settles exactly ONE tool call and must retire exactly ONE hold
  // (the oldest still-live match), leaving its twin parked on the human. The
  // old code matched on the shared key and expired BOTH.
  const perm = fixture('permission-request', { session: 's1' }); // Bash `rm -rf build/`
  const repliesA = [];
  const repliesB = [];
  const rowA = questions.create('permission', 's1', perm);
  const rowB = questions.create('permission', 's1', perm);
  questions.attachHold(rowA, b => repliesA.push(b));
  questions.attachHold(rowB, b => repliesB.push(b));

  const changed = questions.expireOnActivity('s1', { toolName: perm.tool_name, toolInput: perm.tool_input });

  assert.equal(changed, true);
  assert.deepEqual(repliesA, [{}], 'the OLDEST matching hold is failed open — one completion, one release');
  assert.deepEqual(repliesB, [], 'its identical-input twin stays held — one PostToolUse cannot retire both');
  assert.deepEqual(questions.pendingOf('s1').map(r => r.id), [rowB.id],
    'only the older identical sibling expired; the newer is still the human’s open question');

  // A SECOND identical completion retires the survivor — nothing is stranded.
  const changed2 = questions.expireOnActivity('s1', { toolName: perm.tool_name, toolInput: perm.tool_input });
  assert.equal(changed2, true);
  assert.deepEqual(repliesB, [{}], 'the second identical completion retires the remaining twin');
  assert.deepEqual(questions.pendingOf('s1'), []);
  db.close();
});

test('BUG-138: completing an ANSWERED duplicate permission does NOT expire its still-pending identical twin', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });

  // Two parallel permission holds for the SAME tool call (identical
  // tool_name + tool_input → one shared toolCallKey). The board answers A;
  // A's row flips to 'answered' and leaves the pending set that
  // expireOnActivity correlates against. When A's OWN completing PostToolUse
  // then arrives, the old code matched only pending twin B and expired it —
  // the second permission vanished from the board and fell back to the
  // terminal, even though the human had never decided it.
  const perm = fixture('permission-request', { session: 's1' }); // Bash `rm -rf build/`
  const repliesA = [];
  const repliesB = [];
  const rowA = questions.create('permission', 's1', perm);
  const rowB = questions.create('permission', 's1', perm);
  questions.attachHold(rowA, b => repliesA.push(b));
  questions.attachHold(rowB, b => repliesB.push(b));

  const res = questions.answer(rowA.id, { behavior: 'allow' });
  assert.equal(res.status, 200);
  assert.deepEqual(questions.pendingOf('s1').map(r => r.id), [rowB.id], 'only twin B remains pending');

  // A's tool completes — its PostToolUse carries the same (tool_name, tool_input).
  const changed = questions.expireOnActivity('s1', { toolName: perm.tool_name, toolInput: perm.tool_input });

  assert.equal(changed, false, 'the answered call absorbs its own completion — nothing pending was retired');
  assert.deepEqual(repliesB, [], 'the still-pending twin keeps its hold — the board card stays live');
  assert.deepEqual(questions.pendingOf('s1').map(r => r.id), [rowB.id],
    'B survives its answered sibling’s completion');

  // B is then decided in the TERMINAL (no board answer, no ledger entry): its
  // own identical PostToolUse retires it exactly as before the fix.
  const changed2 = questions.expireOnActivity('s1', { toolName: perm.tool_name, toolInput: perm.tool_input });
  assert.equal(changed2, true);
  assert.deepEqual(repliesB, [{}], 'a completion with no recorded answer still retires its hold');
  assert.deepEqual(questions.pendingOf('s1'), []);
  db.close();
});

test('BUG-138 (triple): one answered hold absorbs exactly ONE completion; further identical completions retire twins in order', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });

  // Three parallel identical holds; the board answers only A. Each completing
  // PostToolUse settles exactly one call: A's completion is absorbed by the
  // answer ledger, then B's and C's completions retire their own holds —
  // proving the ledger is counted, not a blanket suppression for the key.
  const perm = fixture('permission-request', { session: 's1' });
  const repliesB = [];
  const repliesC = [];
  const rowA = questions.create('permission', 's1', perm);
  const rowB = questions.create('permission', 's1', perm);
  const rowC = questions.create('permission', 's1', perm);
  questions.attachHold(rowA, () => {});
  questions.attachHold(rowB, b => repliesB.push(b));
  questions.attachHold(rowC, b => repliesC.push(b));

  assert.equal(questions.answer(rowA.id, { behavior: 'allow' }).status, 200);

  const act = () => questions.expireOnActivity('s1', { toolName: perm.tool_name, toolInput: perm.tool_input });
  assert.equal(act(), false, 'A’s completion is consumed by its answer — both twins survive');
  assert.deepEqual(questions.pendingOf('s1').map(r => r.id), [rowB.id, rowC.id]);
  assert.equal(act(), true, 'the next completion retires the oldest remaining twin');
  assert.deepEqual(repliesB, [{}]);
  assert.deepEqual(questions.pendingOf('s1').map(r => r.id), [rowC.id]);
  assert.equal(act(), true);
  assert.deepEqual(repliesC, [{}]);
  assert.deepEqual(questions.pendingOf('s1'), []);
  db.close();
});

test('M-B1 (b2): a completing tool call that matches NO hold (e.g. a Read that needed no permission) leaves every hold pending', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });
  const perm = fixture('permission-request', { session: 's1' }); // Bash `rm -rf build/`
  const replies = [];
  const row = questions.create('permission', 's1', perm);
  questions.attachHold(row, b => replies.push(b));

  // The human is still being asked about `rm -rf build/`; an unrelated tool
  // completing must not release its hold.
  const changed = questions.expireOnActivity('s1', { toolName: 'Read', toolInput: { file_path: '/work/util.js' } });

  assert.equal(changed, false, 'no matching hold → nothing expires');
  assert.deepEqual(replies, [], 'the unrelated Bash permission hold is left held');
  assert.deepEqual(questions.pendingOf('s1').map(r => r.id), [row.id]);
  questions.dismiss(row.id);
  db.close();
});

test('M-B1 (c): an activity with no tool identity (a turn boundary / UserPromptSubmit) expires session-wide — every hold and freeform row', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });

  const perm = fixture('permission-request', { session: 's1' });
  const elic = fixture('elicitation', { session: 's1' }); // no tool_name at all
  const repliesP = [];
  const repliesE = [];
  const rowP = questions.create('permission', 's1', perm);
  const rowE = questions.create('elicitation', 's1', elic);
  questions.create('freeform', 's1', { text: 'Anything else?' });
  questions.attachHold(rowP, b => repliesP.push(b));
  questions.attachHold(rowE, b => repliesE.push(b));

  // UserPromptSubmit passes NO toolName → the turn-boundary session-wide path.
  const changed = questions.expireOnActivity('s1');

  assert.equal(changed, true);
  assert.deepEqual(repliesP, [{}], 'the permission hold is failed open');
  assert.deepEqual(repliesE, [{}], 'the elicitation hold (no tool_name) is failed open on the turn boundary');
  assert.deepEqual(questions.pendingOf('s1'), [], 'the turn boundary clears everything, freeform included');
  db.close();
});

test('M-B1 (c2): a correlated PostToolUse never touches an elicitation hold (no tool_name to match)', () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 60_000 });
  const elic = fixture('elicitation', { session: 's1' });
  const replies = [];
  const rowE = questions.create('elicitation', 's1', elic);
  questions.attachHold(rowE, b => replies.push(b));

  const changed = questions.expireOnActivity('s1', { toolName: 'Bash', toolInput: { command: 'rm -rf build/' } });

  assert.equal(changed, false, 'a tool completion cannot correlate with MCP form input');
  assert.deepEqual(replies, [], 'the elicitation hold stays held');
  questions.dismiss(rowE.id);
  db.close();
});

test('M-B7: a timer persistence failure still fails the hold open without an uncaught exception', async () => {
  const db = openDb(':memory:');
  const questions = createQuestions(db, { holdMs: 25 });
  const row = questions.create('permission', 'timer-session', fixture('permission-request', { session: 'timer-session' }));
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

// ---------------------------------------------------------------------------
// BUG-137: mail-delivered answers (freeform + re-armed holds) must not be
// silently truncated. mail() clamps at MAIL_MAX_LEN and reports
// {truncated:true}, but the answer paths used to ignore the receipt and settle
// the row anyway — the human got "answer queued", the agent got a truncated
// instruction, and the question was gone. Oversized framed answers are now
// rejected (413) BEFORE settle: the row stays pending and nothing is mailed.
// mailMaxLen is shrunk here so the tests don't have to build 4 KB bodies.
// ---------------------------------------------------------------------------
test('BUG-137: an oversized freeform answer is rejected 413 and the question stays pending', () => {
  const db = openDb(':memory:');
  const mailed = [];
  const questions = createQuestions(db, {
    mail: (sid, from, text) => mailed.push({ sid, from, text }),
    mailMaxLen: 120,
  });
  const row = questions.create('freeform', 's1', { text: 'Ship it?' });

  const out = questions.answer(row.id, { text: 'yes — ' + 'x'.repeat(200) });

  assert.equal(out.status, 413, 'an answer whose framed form exceeds the mailbox clamp is rejected');
  assert.equal(out.body.ok, false);
  assert.match(out.body.err, /too long/);
  assert.equal(mailed.length, 0, 'nothing truncated ever reaches the mailbox');
  assert.equal(questions.pendingOf('s1').length, 1, 'the question is still pending — the human can retry');
  db.close();
});

test('BUG-137: an oversized re-armed hold answer is rejected 413; the row and its re-arm chain survive', () => {
  const db = openDb(':memory:');
  const mailed = [];
  const questions = createQuestions(db, {
    mail: (sid, from, text) => mailed.push({ sid, from, text }),
    // The fixed re-arm frame ("[FLEETDECK ANSWER] permission (answered after
    // the hold expired) Q: Run the migration? — A: allow") is ~95 code units —
    // 80 puts even a bare decision over the clamp.
    mailMaxLen: 80,
    rearmGraceMs: 0, // chain bookkeeping only; no grace timers needed here
  });
  const row = questions.create('permission', 's1', { text: 'Run the migration?', rearmed: true });

  const out = questions.answer(row.id, { behavior: 'allow' });

  assert.equal(out.status, 413, 'even a short decision rejects when the fixed re-arm frame alone overflows');
  assert.equal(out.body.ok, false);
  assert.equal(mailed.length, 0, 'no truncated [FLEETDECK ANSWER] frame is mailed');
  assert.equal(questions.pendingOf('s1').length, 1, 'the re-armed card stays on the rail');
  db.close();
});

test('BUG-137: an answer that fits the clamp still settles and mails exactly as before', () => {
  const db = openDb(':memory:');
  const mailed = [];
  const questions = createQuestions(db, {
    mail: (sid, from, text) => mailed.push({ sid, from, text }),
    mailMaxLen: 4000,
  });
  const row = questions.create('freeform', 's1', { text: 'Ship it?' });

  const out = questions.answer(row.id, { text: 'yes, ship it' });

  assert.equal(out.status, 200);
  assert.equal(mailed.length, 1);
  assert.match(mailed[0].text, /^\[FLEETDECK ANSWER\] Q: Ship it\? — A: yes, ship it$/);
  assert.equal(questions.pendingOf('s1').length, 0, 'the question settled normally');
  db.close();
});
