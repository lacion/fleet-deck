// tests/needs-you.test.mjs
//
// Phase 3 "needs-you relay" contract tests (PermissionRequest/Elicitation
// rows, verified against the official hooks docs).
//
// Coverage map (spec bullets -> tests below):
//   1. PermissionRequest/Elicitation hold + /state `questions` shape
//        -> "F3a: PermissionRequest ... allow", "F3a: PermissionRequest ... deny",
//           "F3b: Elicitation ..."
//   2. POST /api/questions/:id/answer resolves the held request (allow/deny/
//      elicitation accept) + hold-expiry -> {} + status 'expired'
//        -> same three tests above + "hold expiry: unanswered PermissionRequest ..."
//   3. F3d freeform detection from transcript tail + answer delivery via mail
//        -> "F3d: Stop trailing-question freeform detection ..."
//           "F3d (probe): Stop with last_assistant_message present ..."
//   4. A Stop that returns a mail block skips question detection
//        -> "a Stop that returns a mail block does not run freeform question detection"
//   5. Notification stores notification_type; subsequent activity expires
//      pending permission/elicitation questions + returns col to working;
//      SessionEnd expires all pending questions
//        -> "Notification ingest stores notification_type"
//           "subsequent activity (UserPromptSubmit) returns col to working ..."
//           "SessionEnd expires all pending permission/elicitation questions ..."
//   6. Concurrent holds per session capped at 4(5th expires oldest)
//        -> "concurrent holds per session are capped at 4 ..."
//   7. Hook endpoints stay fail-open on malformed bodies
//        -> "malformed /hook/PermissionRequest body still answers 200 ..."
//
// Timing convention (per task brief): FLEETDECK_HOLD_MS is set short
// (1200-2000ms for tests that only need one hold; up to 6000ms for tests
// where "activity"/"SessionEnd" must expire a hold well before its natural
// timeout, so the early-resolution assertion isn't confounded by the timeout
// itself) and resolution is asserted within a generous tolerance.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeTranscriptDir, writeTranscript } from './helpers/transcript.mjs';
import { waitUntil } from './helpers/wait.mjs';

function scratchCwd() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
}

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

function questionsFor(state, sid, kind) {
  return (state.questions || []).filter(q => q.session_id === sid && (!kind || q.kind === kind));
}

// ---------------------------------------------------------------------------
// 1+2. F3a permission relay: hold + board answer (allow / deny)
// ---------------------------------------------------------------------------

test('F3a: PermissionRequest holds open; board answer {behavior:"allow"} resolves the held response and the question stops being pending', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  const reg = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const callsign = reg.json?.callsign;
  assert.ok(callsign, 'sanity: SessionStart should hand back a callsign');

  const t0 = Date.now();
  const held = postHook(
    daemon.baseUrl, 'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );

  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission')[0];
  }, { label: 'permission question to appear in /state' });

  assert.equal(q.kind, 'permission');
  assert.equal(q.session_id, sid);
  assert.equal(q.callsign, callsign, 'question should carry the session callsign');
  assert.ok(q.payload, 'question should carry a payload');
  assert.equal(q.status, 'pending');
  assert.ok(q.created_at, 'question should carry created_at');
  assert.ok(q.expires_at, 'question should carry expires_at');
  assert.ok(q.id !== undefined && q.id !== null, 'question needs an id to answer it by');

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'allow' });
  assert.equal(ansRes.status, 200, 'POST /api/questions/:id/answer should 200');

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `answering should resolve the hold well before the ${holdMs}ms window (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {
    hookSpecificOutput: {
      hookEventName: 'PermissionRequest',
      decision: { behavior: 'allow' },
    },
  }, 'allow decision should produce the PermissionRequest response schema (verified against the official hooks docs)');

  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q2 = state2.questions.find(x => String(x.id) === String(q.id));
  assert.ok(q2, 'answered question should still be present in /state (with a terminal status), not vanish');
  assert.notEqual(q2.status, 'pending', 'answered question must no longer be pending');
});

test('F3a: PermissionRequest board answer {behavior:"deny"} resolves the held response with behavior=deny', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const held = postHook(
    daemon.baseUrl, 'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );

  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission')[0];
  }, { label: 'permission question to appear in /state' });

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'deny' });
  assert.equal(ansRes.status, 200);

  const heldRes = await held;
  assert.equal(heldRes.json?.hookSpecificOutput?.hookEventName, 'PermissionRequest');
  assert.equal(heldRes.json?.hookSpecificOutput?.decision?.behavior, 'deny', 'deny decision should carry behavior=deny');
});

// ---------------------------------------------------------------------------
// 1+2. F3b elicitation relay: hold + board answer (accept)
// ---------------------------------------------------------------------------

test('F3b: Elicitation holds open; board answer {action:"accept", content} resolves the held response per the handoff shape', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  const reg = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const callsign = reg.json?.callsign;

  const t0 = Date.now();
  const held = postHook(
    daemon.baseUrl, 'Elicitation',
    loadFixture('elicitation', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );

  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'elicitation')[0];
  }, { label: 'elicitation question to appear in /state' });

  assert.equal(q.kind, 'elicitation');
  assert.equal(q.session_id, sid);
  assert.equal(q.callsign, callsign);
  assert.ok(q.payload);
  assert.equal(q.status, 'pending');

  const content = { target: 'staging' };
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { action: 'accept', content });
  assert.equal(ansRes.status, 200);

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `answering should resolve the hold well before the ${holdMs}ms window (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, { action: 'accept', content }, 'elicitation answer should be carried through per the handoff-documented shape (F3b)');
});

// ---------------------------------------------------------------------------
// 2. Hold expiry: no answer -> {} + status 'expired'
// ---------------------------------------------------------------------------

test('hold expiry: an unanswered PermissionRequest resolves to {} within tolerance and the question becomes expired', async (t) => {
  const holdMs = 1200;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const t0 = Date.now();
  const held = postHook(
    daemon.baseUrl, 'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );

  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission')[0];
  }, { label: 'permission question to appear in /state' });

  const heldRes = await held; // intentionally never answered
  const elapsed = Date.now() - t0;
  assert.ok(Math.abs(elapsed - holdMs) <= 800, `hold should resolve within +/-800ms of ${holdMs}ms (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'timed-out hold should resolve to {} (normal permission flow resumes)');

  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q2 = state2.questions.find(x => String(x.id) === String(q.id));
  assert.ok(q2, 'expired question should still be present in /state');
  assert.equal(q2.status, 'expired', 'unanswered hold should leave the question status=expired');
});

// ---------------------------------------------------------------------------
// 3. F3d: freeform detection from a Stop's transcript tail + answer delivery
// ---------------------------------------------------------------------------

test('F3d: Stop trailing-question freeform detection creates a needsyou card, and the board answer is delivered as mail', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });

  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'Should I use bcrypt or argon2?',
  });

  // last_assistant_message intentionally ABSENT -- forces the transcript_path
  // branch per the F3d plan (Stop payloads don't carry last_assistant_message
  // per the official hooks docs).
  const stopRes = await postHook(daemon.baseUrl, 'Stop', {
    session_id: sid,
    hook_event_name: 'Stop',
    cwd,
    transcript_path: transcriptPath,
  }, { token: daemon });
  assert.equal(stopRes.status, 200);

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  assert.equal(card.col, 'needsyou', 'a trailing question at Stop should move the card to needsyou');

  const q = questionsFor(state, sid, 'freeform')[0];
  assert.ok(q, 'a freeform question should be created from the transcript tail');
  assert.equal(q.status, 'pending');
  assert.ok(q.id !== undefined && q.id !== null);

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: 'argon2' });
  assert.equal(ansRes.status, 200, 'POST /api/questions/:id/answer should 200 for a freeform answer');

  // Delivery channel is either the next UserPromptSubmit's additionalContext,
  // or the next Stop returning a mail block -- spec allows either.
  const upRes = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, {
    prompt: 'continue',
  }), { token: daemon });
  const upCtx = upRes.json?.hookSpecificOutput?.additionalContext ?? '';
  let delivered = /\[FLEETDECK ANSWER\]/.test(upCtx) && upCtx.includes('argon2');
  let deliveryChannel = delivered ? 'UserPromptSubmit additionalContext' : null;

  if (!delivered) {
    const transcriptPath2 = writeTranscript(transcriptDir, {
      sessionId: sid,
      assistantText: 'Noted, moving on.',
    });
    const stopRes2 = await postHook(daemon.baseUrl, 'Stop', {
      session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath2,
    }, { token: daemon });
    const reason = stopRes2.json?.reason ?? '';
    delivered = stopRes2.json?.decision === 'block' && /\[FLEETDECK ANSWER\]/.test(reason) && reason.includes('argon2');
    deliveryChannel = delivered ? 'Stop block' : null;
  }

  assert.ok(delivered, 'answering a freeform question should enqueue mail that reaches the session via UserPromptSubmit additionalContext or a Stop block, carrying "[FLEETDECK ANSWER]" and the answer text');
  t.diagnostic(`freeform answer delivered via: ${deliveryChannel}`);
});

test('F3d (probe): Stop with last_assistant_message present on the live payload', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });

  // Deliberately mismatched: the transcript tail is NOT a question, but the
  // live payload's last_assistant_message IS one. This distinguishes which
  // source the implementation actually reads.
  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'All done, tests pass.',
  });
  const stopRes = await postHook(daemon.baseUrl, 'Stop', {
    session_id: sid,
    hook_event_name: 'Stop',
    cwd,
    transcript_path: transcriptPath,
    last_assistant_message: 'Should I use REST or GraphQL for this endpoint?',
  }, { token: daemon });
  assert.equal(stopRes.status, 200, 'Stop should 200 regardless of which detection path is taken');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q = questionsFor(state, sid, 'freeform')[0];
  if (q) {
    t.diagnostic('implementation reads last_assistant_message from the live Stop payload when present (freeform question created from it, not the transcript tail)');
    assert.equal(q.status, 'pending');
  } else {
    t.diagnostic('implementation reads only the transcript_path tail (per the F3d plan) -- last_assistant_message on the live payload is not used for detection; no freeform question created here, which is an accepted alternative');
  }
});

// ---------------------------------------------------------------------------
// 4. A Stop that returns a mail block skips question detection
// ---------------------------------------------------------------------------

test('a Stop that returns a mail block does not run freeform question detection for that request', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'tester', text: 'please wrap up soon' }, { token: daemon });

  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'Should I use REST or GraphQL for this endpoint?',
  });
  const stopRes = await postHook(daemon.baseUrl, 'Stop', {
    session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath,
  }, { token: daemon });
  assert.equal(stopRes.json?.decision, 'block', 'pending mail should still take priority and block, same as Phase 1/2 behavior');
  assert.match(stopRes.json?.reason ?? '', /\[FLEETDECK MAIL\]/);

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const qs = questionsFor(state, sid, 'freeform');
  assert.equal(qs.length, 0, 'freeform question detection must be skipped when Stop returns a mail block');
});

// ---------------------------------------------------------------------------
// 5. Notification stores notification_type
// ---------------------------------------------------------------------------

test('Notification ingest stores notification_type', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const res = await postHook(daemon.baseUrl, 'Notification', loadFixture('notification', { session_id: sid, cwd }, {
    notification_type: 'permission_prompt',
  }), { token: daemon });
  assert.equal(res.status, 200);

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = findSession(state, sid);
  assert.ok(card, 'session card should exist');
  assert.equal(card.col, 'needsyou', 'Notification is the F3e safety net: col should move to needsyou');

  const carriesType = JSON.stringify(card).includes('permission_prompt');
  assert.ok(carriesType, `notification_type should be stored/exposed somewhere on the session card; got ${JSON.stringify(card)}`);
});

// ---------------------------------------------------------------------------
// 5. Subsequent activity expires pending permission/elicitation questions and
//    returns col to working
// ---------------------------------------------------------------------------

test('subsequent activity (UserPromptSubmit) returns col to working and expires a pending permission question ahead of its own hold timeout', async (t) => {
  const holdMs = 6000; // long enough that early resolution can only be activity-driven, not the natural timeout
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const held = postHook(
    daemon.baseUrl, 'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );

  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission')[0];
  }, { label: 'permission question registered' });

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  let card = findSession(state, sid);
  assert.equal(card.col, 'needsyou', 'a held permission request should show needsyou on the board');

  const t0 = Date.now();
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  card = findSession(state, sid);
  assert.equal(card.col, 'working', 'subsequent activity (UserPromptSubmit) should return the card to working');

  const q2 = state.questions.find(x => String(x.id) === String(q.id));
  assert.ok(q2, 'the permission question should still be present in /state');
  assert.equal(q2.status, 'expired', 'subsequent activity should expire the pending permission question');

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `activity-triggered expiry should resolve the hold promptly, not wait out the full ${holdMs}ms hold (took ${elapsed}ms after the triggering activity)`);
  assert.deepEqual(heldRes.json, {}, 'activity-expired hold should resolve to {}');
});

// ---------------------------------------------------------------------------
// 5. SessionEnd expires all pending questions for the session
// ---------------------------------------------------------------------------

test('SessionEnd expires all pending permission/elicitation questions for the session', async (t) => {
  const holdMs = 6000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const heldA = postHook(
    daemon.baseUrl, 'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );
  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission').length === 1 || null;
  }, { label: 'permission question registered' });

  const heldB = postHook(
    daemon.baseUrl, 'Elicitation',
    loadFixture('elicitation', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );
  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'elicitation').length === 1 || null;
  }, { label: 'elicitation question registered' });

  const t0 = Date.now();
  const endRes = await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sid, cwd }), { token: daemon });
  assert.deepEqual(endRes.json, {}, 'SessionEnd should still respond {}');

  const [resA, resB] = await Promise.all([heldA, heldB]);
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `both holds should resolve promptly on SessionEnd, not wait out the ${holdMs}ms hold (took ${elapsed}ms)`);
  assert.deepEqual(resA.json, {}, 'SessionEnd-expired permission hold should resolve to {}');
  assert.deepEqual(resB.json, {}, 'SessionEnd-expired elicitation hold should resolve to {}');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const qs = questionsFor(state, sid);
  assert.ok(qs.length >= 2, 'both questions should still be present in /state');
  for (const q of qs) {
    assert.equal(q.status, 'expired', `question ${q.id} (${q.kind}) should be expired after SessionEnd`);
  }
  const card = findSession(state, sid);
  assert.equal(card.col, 'offline', 'SessionEnd is the tombstone regardless of pending questions');
});

test('freeform questions SURVIVE SessionEnd and deliver on resume', async (t) => {
  // An ended session is resumable (`claude --resume`); its unanswered
  // trailing question stays in the human's queue and the answer arrives as
  // mail at the resumed session's first turn boundary. (Live Phase 3
  // acceptance run 1 proved the old expire-everything rule orphans answers.)
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'Should the project use bcrypt or argon2 for password hashing?',
  });
  await postHook(daemon.baseUrl, 'Stop', {
    session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath,
  }, { token: daemon });
  await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sid, cwd }), { token: daemon });

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(findSession(state, sid).col, 'offline', 'session tombstoned');
  const q = questionsFor(state, sid, 'freeform')[0];
  assert.ok(q, 'freeform question still present after SessionEnd');
  assert.equal(q.status, 'pending', 'freeform question still pending after SessionEnd');

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: 'argon2id' });
  assert.equal(ansRes.status, 200, 'answering a question of an ended session must succeed');

  // Resume: SessionStart(resume) + first UserPromptSubmit drains the answer.
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }, { source: 'resume' }), { token: daemon });
  const upRes = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }), { token: daemon });
  const ctx = upRes.json?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(/\[FLEETDECK ANSWER\]/.test(ctx) && ctx.includes('argon2id'),
    `resumed session's first UserPromptSubmit must carry the answer (got: ${ctx.slice(0, 120)})`);
});

// ---------------------------------------------------------------------------
// 6. Concurrent holds per session capped at 4
// ---------------------------------------------------------------------------

test('concurrent holds per session are capped at 4; the 5th arrival expires the oldest', async (t) => {
  const holdMs = 4000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const kinds = ['permission', 'elicitation', 'permission', 'elicitation', 'permission'];
  const promises = [];
  let firstFourPending = null;

  for (let i = 0; i < kinds.length; i++) {
    const kind = kinds[i];
    const event = kind === 'permission' ? 'PermissionRequest' : 'Elicitation';
    const fixtureName = kind === 'permission' ? 'permission-request' : 'elicitation';
    const p = postHook(
      daemon.baseUrl, event,
      loadFixture(fixtureName, { session_id: sid, cwd }),
      { token: daemon, timeout: holdMs + 8000 },
    );
    promises.push(p);

    if (i < 4) {
      const expectedCount = i + 1;
      const pending = await waitUntil(async () => {
        const state = (await getJson(`${daemon.baseUrl}/state`)).json;
        const rows = questionsFor(state, sid).filter(x => x.status === 'pending');
        return rows.length === expectedCount ? rows : null;
      }, { label: `hold #${expectedCount} registered as pending`, timeoutMs: 3000 });
      if (i === 3) firstFourPending = [...pending].sort((a, b) => a.created_at - b.created_at);
    }
  }

  assert.ok(firstFourPending, 'sanity: should have observed exactly 4 pending questions before the 5th arrival');
  const oldestId = firstFourPending[0].id;

  const oldestSettled = await Promise.race([
    promises[0],
    new Promise(resolve => setTimeout(() => resolve('TIMEOUT'), 1500)),
  ]);
  assert.notEqual(oldestSettled, 'TIMEOUT', 'the oldest hold should be expired promptly once a 5th concurrent hold arrives for the same session (cap=4)');
  assert.deepEqual(oldestSettled.json, {}, 'cap-expired hold should resolve to {}');

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const pendingNow = questionsFor(state, sid).filter(x => x.status === 'pending');
  assert.equal(pendingNow.length, 4, 'exactly 4 questions should remain pending for the session (cap=4) after the 5th arrival');

  const oldestRow = state.questions.find(x => String(x.id) === String(oldestId));
  assert.ok(oldestRow, 'the oldest question should still be present in /state');
  assert.equal(oldestRow.status, 'expired', 'the oldest (1st) hold should now be marked expired, having been evicted by the cap');

  // Clean up the remaining 4 held requests so nothing dangles past teardown.
  for (const q of pendingNow) {
    const body = q.kind === 'permission' ? { behavior: 'allow' } : { action: 'accept', content: { target: 'staging' } };
    await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, body);
  }
  await Promise.all(promises.slice(1));

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(
    questionsFor(state, sid).filter(x => x.status === 'pending').length,
    0,
    'all remaining holds should be resolved after answering',
  );
});

// ---------------------------------------------------------------------------
// 7. Hook endpoints stay fail-open on malformed bodies
// ---------------------------------------------------------------------------

test('malformed /hook/PermissionRequest body still answers 200 and the daemon stays healthy', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  t.after(async () => { await daemon.stop(); });

  const res = await fetch(`${daemon.baseUrl}/hook/PermissionRequest`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: '{not valid json at all',
    signal: AbortSignal.timeout(holdMs + 5000),
  });
  assert.equal(res.status, 200, 'malformed body must still 200 (fail-open, per the hook contract)');

  const text = await res.text();
  if (text) {
    assert.doesNotThrow(() => JSON.parse(text), 'response body, if any, must be valid JSON so the hook consumer never chokes');
  }

  const health = await getJson(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200, 'daemon must stay healthy after a malformed hook body');
  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.equal(state.status, 200, '/state must still respond fine after a malformed hook body');
});

// A freeform card must not outlive the question it represents. The human very
// often answers in the terminal (or the board's live-terminal modal) instead of
// the rail — and the rail used to keep showing the card anyway, forever, until
// NEEDS YOU was a wall of ghosts nobody trusted. Any activity from the session
// proves it is no longer waiting.
test('F3d: a freeform card clears when the session moves on (answered in the terminal)', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });
  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'Should I use bcrypt or argon2?',
  });
  await postHook(daemon.baseUrl, 'Stop', {
    session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath,
  }, { token: daemon });

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q = questionsFor(state, sid, 'freeform')[0];
  assert.ok(q, 'sanity: the trailing question became a card');
  assert.equal(q.status, 'pending');

  // The human answers in the terminal: the session takes a new prompt.
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const after = (state.questions || []).find(x => x.id === q.id);
  assert.notEqual(after?.status, 'pending', 'the card must not still be pending once the session moved on');
});

test('a stale needs-you card can be dismissed without telling the session anything', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon });
  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'Want me to open the PR?',
  });
  await postHook(daemon.baseUrl, 'Stop', {
    session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath,
  }, { token: daemon });

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q = questionsFor(state, sid, 'freeform')[0];
  assert.ok(q, 'sanity: the question became a card');

  const res = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/dismiss`, {});
  assert.equal(res.status, 200);
  assert.equal(res.json.ok, true);

  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const after = (state.questions || []).find(x => x.id === q.id);
  assert.notEqual(after?.status, 'pending', 'a dismissed card is retired');

  // Dismissing is NOT answering: the session must receive no mail for it.
  const mail = (await getJson(`${daemon.baseUrl}/mail?session=${sid}`)).json.mail || [];
  assert.equal(mail.length, 0, 'dismiss must not mail the session anything');

  const again = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/dismiss`, {});
  assert.equal(again.status, 200, 'dismiss is idempotent');
  const missing = await postJson(`${daemon.baseUrl}/api/questions/999999/dismiss`, {});
  assert.equal(missing.status, 404, 'an unknown question id is a 404');
});
