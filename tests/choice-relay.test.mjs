// tests/choice-relay.test.mjs
//
// Phase 4 F3c: AskUserQuestion PreToolUse relay ('choice' hold kind).
// Built against the live-validated payload/response shapes (validated live
// on CLI 2.1.206) and the raw evidence in
// demo/demo-logs/phase4/run1-askq-hook-recorder.jsonl / run2-askq-deny-*:
//
//   - /hook/AskUserQuestion (PreToolUse-shaped payload, tool_input.questions[]
//     of {question, header, options:[{label,description}], multiSelect} +
//     tool_use_id) holds open like F3a and shows in /state as kind 'choice'.
//   - Board answer {answers:{"<question text>":"<label>"}} (the CLI's own
//     PostToolUse answers format) or {text} → the held response resolves to
//     the deny-with-reason schema VERBATIM.
//   - Expiry → {} (the native terminal chooser renders as normal).
//   - CRITICAL: /hook/PermissionRequest with tool_name==="AskUserQuestion"
//     answers {} IMMEDIATELY (never held) — otherwise an unanswered question
//     chains two full hold windows.
//   - Activity/SessionEnd expiry semantics identical to the permission kind.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

const FIXTURE_QUESTION = 'Should this project use bcrypt or argon2 for password hashing?';

function scratchCwd() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
}

function questionsFor(state, sid, kind) {
  return (state.questions || []).filter(q => q.session_id === sid && (!kind || q.kind === kind));
}

const WAIT_SCALE = Number(process.env.FLEETDECK_TEST_WAIT_SCALE) || 1;

async function waitUntil(fn, { timeoutMs = 5000, intervalMs = 100, label = 'condition' } = {}) {
  const effectiveTimeoutMs = timeoutMs * WAIT_SCALE;
  const deadline = Date.now() + effectiveTimeoutMs;
  for (;;) {
    const result = await fn();
    if (result) return result;
    if (Date.now() >= deadline) throw new Error(`waitUntil: ${label} not met within ${effectiveTimeoutMs}ms`);
    await new Promise(r => setTimeout(r, intervalMs));
  }
}

async function holdChoice(daemon, sid, cwd, holdMs, overrides = {}) {
  const held = postHook(
    daemon.baseUrl, 'AskUserQuestion',
    loadFixture('ask-user-question', { session_id: sid, cwd }, overrides),
    { timeout: holdMs + 5000 },
  );
  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'choice')[0];
  }, { label: 'choice question to appear in /state' });
  return { held, q };
}

// ---------------------------------------------------------------------------
// hold fires → /state carries the parsed questions[]; board answer → the
// validated deny+reason schema VERBATIM
// ---------------------------------------------------------------------------

test('F3c: AskUserQuestion holds as kind=choice with parsed questions[]; {answers} resolves the held response to the validated deny schema verbatim', async (t) => {
  const holdMs = 4000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  const reg = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  const callsign = reg.json?.callsign;

  const t0 = Date.now();
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  assert.equal(q.kind, 'choice');
  assert.equal(q.session_id, sid);
  assert.equal(q.callsign, callsign);
  assert.equal(q.status, 'pending');
  assert.ok(q.expires_at, 'choice is a hold kind — it must carry a hold deadline');
  // the payload must expose the validated tool_input.questions[] shape, parsed
  const questions = q.payload?.tool_input?.questions;
  assert.ok(Array.isArray(questions) && questions.length === 1, '/state payload should carry parsed tool_input.questions[]');
  assert.equal(questions[0].question, FIXTURE_QUESTION);
  assert.equal(questions[0].header, 'Hashing algo');
  assert.equal(questions[0].multiSelect, false);
  assert.deepEqual(questions[0].options.map(o => o.label), ['bcrypt', 'argon2']);
  assert.ok(questions[0].options.every(o => typeof o.description === 'string'), 'options should keep their descriptions for the board');
  assert.ok(q.payload?.tool_use_id, 'payload should keep tool_use_id');

  // card telemetry: the session is waiting on the human
  const card = (await getJson(`${daemon.baseUrl}/state`)).json.sessions.find(s => s.session_id === sid);
  assert.equal(card.col, 'needsyou', 'a held choice question should show needsyou on the board');

  // answer in the CLI's own PostToolUse `answers` map format
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'argon2' },
  });
  assert.equal(ansRes.status, 200);
  assert.equal(ansRes.json?.delivered, true, 'a live choice hold answer is delivered synchronously');

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `answering should resolve the hold well before the ${holdMs}ms window (took ${elapsed}ms)`);
  // VERBATIM the response the live run2 deny validation proved graceful
  assert.deepEqual(heldRes.json, {
    hookSpecificOutput: {
      hookEventName: 'PreToolUse',
      permissionDecision: 'deny',
      permissionDecisionReason: 'User answered via Fleet Deck: argon2',
    },
  }, 'choice answer must produce the validated PreToolUse deny+reason schema (validated live on CLI 2.1.206) verbatim');

  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q2 = state2.questions.find(x => String(x.id) === String(q.id));
  assert.equal(q2?.status, 'answered');
});

// ---------------------------------------------------------------------------
// expiry → {} (terminal chooser renders as normal)
// ---------------------------------------------------------------------------

test('F3c: an unanswered AskUserQuestion hold expires to {} and the question becomes expired', async (t) => {
  const holdMs = 1200;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const t0 = Date.now();
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);
  const heldRes = await held; // never answered
  const elapsed = Date.now() - t0;

  assert.ok(Math.abs(elapsed - holdMs) <= 800, `hold should resolve within +/-800ms of ${holdMs}ms (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'expiry must resolve to {} so the native terminal chooser renders as normal');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const q2 = state.questions.find(x => String(x.id) === String(q.id));
  assert.equal(q2?.status, 'expired');

  // once expired, the terminal owns the question: a late board answer 409s
  const late = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'argon2' },
  });
  assert.equal(late.status, 409, 'a late answer to an expired choice must be refused — the terminal chooser owns it now');
});

// ---------------------------------------------------------------------------
// CRITICAL guard: PermissionRequest for AskUserQuestion is NEVER held
// ---------------------------------------------------------------------------

test('F3c: /hook/PermissionRequest with tool_name=AskUserQuestion answers {} in <200ms even while another hold is open (and leaves that hold undisturbed)', async (t) => {
  const holdMs = 6000; // long, so a wrongly-held request would blow the 200ms budget by construction
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  // open a choice hold first — the guard must answer around it, not through it
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  // the PermissionRequest side-effect event the live run captured: same
  // tool_input, tool_name AskUserQuestion, no permission_suggestions
  const permPayload = loadFixture('ask-user-question', { session_id: sid, cwd }, { hook_event_name: 'PermissionRequest' });
  delete permPayload.tool_use_id;
  const t0 = Date.now();
  const permRes = await postHook(daemon.baseUrl, 'PermissionRequest', permPayload);
  const elapsed = Date.now() - t0;

  assert.equal(permRes.status, 200);
  assert.deepEqual(permRes.json, {}, 'PermissionRequest for AskUserQuestion must answer {} — the question already had its hold at PreToolUse');
  assert.ok(elapsed < 200, `PermissionRequest for AskUserQuestion must answer immediately, never hold (took ${elapsed}ms)`);

  // no second question row was created for it
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(questionsFor(state, sid, 'permission').length, 0, 'the guard must not create a permission question row');

  // the original choice hold is still live and still answerable
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { answers: { [FIXTURE_QUESTION]: 'bcrypt' } });
  assert.equal(ansRes.status, 200, 'the choice hold must survive the PermissionRequest guard');
  const heldRes = await held;
  assert.equal(heldRes.json?.hookSpecificOutput?.permissionDecisionReason, 'User answered via Fleet Deck: bcrypt');
});

// ---------------------------------------------------------------------------
// multi-question / multiSelect serialization + {text} fallback
// ---------------------------------------------------------------------------

test('F3c: multi-question answers serialize compactly (header: label; multiSelect labels joined)', async (t) => {
  const holdMs = 4000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const toolInput = {
    questions: [
      { question: 'Which hashing algorithm?', header: 'Hashing', options: [{ label: 'bcrypt' }, { label: 'argon2' }], multiSelect: false },
      { question: 'Which deploy targets?', header: 'Deploy', options: [{ label: 'staging' }, { label: 'prod' }, { label: 'docker' }], multiSelect: true },
    ],
  };
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs, { tool_input: toolInput });

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: {
      'Which hashing algorithm?': 'argon2',
      'Which deploy targets?': ['staging', 'docker'], // arrays accepted for multiSelect
    },
  });
  assert.equal(ansRes.status, 200);

  const heldRes = await held;
  assert.equal(
    heldRes.json?.hookSpecificOutput?.permissionDecisionReason,
    'User answered via Fleet Deck: Hashing: argon2; Deploy: staging, docker',
    'multi-question answers should compact to "<header>: <label(s)>" pairs',
  );
});

test('F3c: {text} freeform fallback answers a choice hold with the text as the relayed answer', async (t) => {
  const holdMs = 4000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const bad = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { answers: {} });
  assert.equal(bad.status, 400, 'an empty answers map must be rejected');

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: 'neither — use scrypt, and ask me about cost params later' });
  assert.equal(ansRes.status, 200);
  const heldRes = await held;
  assert.equal(
    heldRes.json?.hookSpecificOutput?.permissionDecisionReason,
    'User answered via Fleet Deck: neither — use scrypt, and ask me about cost params later',
  );
});

// ---------------------------------------------------------------------------
// activity / SessionEnd expiry parity with the permission kind
// ---------------------------------------------------------------------------

test('F3c: session activity (UserPromptSubmit) expires a pending choice hold with {} promptly — identical to the permission kind', async (t) => {
  const holdMs = 6000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const t0 = Date.now();
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
  const heldRes = await held;
  const elapsed = Date.now() - t0;

  assert.ok(elapsed < holdMs, `activity-triggered expiry should resolve the hold promptly (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'activity-expired choice hold must resolve to {}');
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.questions.find(x => String(x.id) === String(q.id))?.status, 'expired');
});

test('F3c: SessionEnd expires a pending choice hold with {} — identical to the permission kind', async (t) => {
  const holdMs = 6000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const t0 = Date.now();
  await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sid, cwd }));
  const heldRes = await held;
  const elapsed = Date.now() - t0;

  assert.ok(elapsed < holdMs, `SessionEnd should settle the hold promptly (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'SessionEnd-expired choice hold must resolve to {}');
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.questions.find(x => String(x.id) === String(q.id))?.status, 'expired');
});
