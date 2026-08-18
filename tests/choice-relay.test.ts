// tests/choice-relay.test.ts
//
// Phase 4 F3c: AskUserQuestion PreToolUse relay ('choice' hold kind).
// Built against the captured payload shape and Claude Code's current documented
// AskUserQuestion PreToolUse response contract:
// demo/demo-logs/phase4/run1-askq-hook-recorder.jsonl / run2-askq-deny-*:
//
//   - /hook/AskUserQuestion (PreToolUse-shaped payload, tool_input.questions[]
//     of {question, header, options:[{label,description}], multiSelect} +
//     tool_use_id) holds open like F3a and shows in /state as kind 'choice'.
//   - Board answer {answers:{"<question text>":"<label>"}} (the CLI's own
//     PostToolUse answers format) or {text} → the held response resolves to
//     permissionDecision:"allow" with updatedInput.questions + answers.
//   - Expiry → {} (the native terminal chooser renders as normal).
//   - CRITICAL: /hook/PermissionRequest with tool_name==="AskUserQuestion"
//     answers {} IMMEDIATELY (never held) — otherwise an unanswered question
//     chains two full hold windows.
//   - Activity/SessionEnd expiry semantics identical to the permission kind.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import type { DaemonHandle } from './helpers/daemon.ts';
import { postHook, postJson } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { waitUntil, scaleMs } from './helpers/wait.ts';
import { getState, questionsFor } from './helpers/state.ts';
import { startRegisteredDaemon } from './helpers/session.ts';

const FIXTURE_QUESTION = 'Should this project use bcrypt or argon2 for password hashing?';

// ── Parsed-JSON shapes the daemon serves; postHook/postJson/getJson hand back
// `unknown`, so each is narrowed to one of these local interfaces at the point
// of use (per-file, never `any`). ─────────────────────────────────────────────
interface ChoiceOption {
  label: string;
  description?: string;
}
interface ChoiceQuestion {
  question: string;
  header: string;
  multiSelect: boolean;
  options: ChoiceOption[];
}
interface QuestionPayload {
  tool_input?: { questions?: ChoiceQuestion[] };
  tool_use_id?: string;
}
interface StateQuestion {
  id: number;
  session_id: string;
  kind: string;
  callsign: string | null;
  status: string;
  expires_at: number | null;
  held?: boolean;
  payload?: QuestionPayload;
}
interface StateCard {
  session_id: string;
  col: string;
}
interface StateResponse {
  questions: StateQuestion[];
  sessions: StateCard[];
}
interface SessionStartResponse {
  callsign?: string;
}
interface AnswerResponse {
  delivered?: boolean;
  err?: string;
}
interface HookRelayResponse {
  hookSpecificOutput?: {
    hookEventName?: string;
    permissionDecision?: string;
    updatedInput?: {
      questions?: ChoiceQuestion[];
      answers?: Record<string, string>;
    };
  };
}

function relayedAnswers(response: unknown): Record<string, string> | undefined {
  return (response as HookRelayResponse | null)?.hookSpecificOutput?.updatedInput?.answers;
}

async function holdChoice(
  daemon: DaemonHandle,
  sid: string,
  cwd: string,
  holdMs: number,
  overrides: Record<string, unknown> = {},
) {
  const held = postHook(
    daemon.baseUrl,
    'AskUserQuestion',
    loadFixture('ask-user-question', { session_id: sid, cwd }, overrides),
    { token: daemon, timeout: holdMs + 5000, boardClient: true },
  );
  const q = await waitUntil(
    async () => {
      const state = await getState<StateResponse>(daemon.baseUrl);
      return questionsFor(state, sid, 'choice')[0];
    },
    { label: 'choice question to appear in /state' },
  );
  return { held, q };
}

// ---------------------------------------------------------------------------
// hold fires → /state carries the parsed questions[]; board answer → the
// current allow+updatedInput schema VERBATIM
// ---------------------------------------------------------------------------

test('F3c: AskUserQuestion holds as kind=choice; {answers} resolves with allow + updatedInput.answers', async (t) => {
  const holdMs = 4000;
  const { daemon, cwd, sid, registration } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });
  const callsign = (registration.json as SessionStartResponse | null)?.callsign;

  const t0 = Date.now();
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  assert.equal(q.kind, 'choice');
  assert.equal(q.session_id, sid);
  assert.equal(q.callsign, callsign);
  assert.equal(q.status, 'pending');
  assert.ok(q.expires_at, 'choice is a hold kind — it must carry a hold deadline');
  // the payload must expose the validated tool_input.questions[] shape, parsed
  const questions = q.payload?.tool_input?.questions;
  assert.ok(
    Array.isArray(questions) && questions.length === 1,
    '/state payload should carry parsed tool_input.questions[]',
  );
  const first = questions[0];
  assert(first);
  assert.equal(first.question, FIXTURE_QUESTION);
  assert.equal(first.header, 'Hashing algo');
  assert.equal(first.multiSelect, false);
  assert.deepEqual(
    first.options.map((o) => o.label),
    ['bcrypt', 'argon2'],
  );
  assert.ok(
    first.options.every((o) => typeof o.description === 'string'),
    'options should keep their descriptions for the board',
  );
  assert.ok(q.payload?.tool_use_id, 'payload should keep tool_use_id');

  // card telemetry: the session is waiting on the human
  const state1 = await getState<StateResponse>(daemon.baseUrl);
  const card = state1.sessions.find((s) => s.session_id === sid);
  assert.equal(card?.col, 'needsyou', 'a held choice question should show needsyou on the board');

  // answer in the CLI's own PostToolUse `answers` map format
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'argon2' },
  });
  assert.equal(ansRes.status, 200);
  assert.equal(
    (ansRes.json as AnswerResponse | null)?.delivered,
    true,
    'a live choice hold answer is delivered synchronously',
  );

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(
    elapsed < holdMs,
    `answering should resolve the hold well before the ${holdMs}ms window (took ${elapsed}ms)`,
  );
  // Current Claude Code contract: echo the original questions and add the
  // board's answers via updatedInput so AskUserQuestion executes normally.
  assert.deepEqual(
    heldRes.json,
    {
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'allow',
        updatedInput: {
          questions,
          answers: { [FIXTURE_QUESTION]: 'argon2' },
        },
      },
    },
    'choice answer must satisfy AskUserQuestion through updatedInput',
  );

  const state2 = await getState<StateResponse>(daemon.baseUrl);
  const q2 = state2.questions.find((x) => String(x.id) === String(q.id));
  assert.equal(q2?.status, 'answered');
});

// ---------------------------------------------------------------------------
// expiry → {} (terminal chooser renders as normal)
// ---------------------------------------------------------------------------

test('F3c: an unanswered AskUserQuestion hold expires to {} and the question becomes expired', async (t) => {
  const holdMs = 1200;
  // Re-arm disabled (grace 0): this test asserts the late answer 409s on the
  // EXPIRED row — under the default grace the daemon would have re-armed the
  // question into a fresh mail-delivered card (covered in question-rearm.test.mjs).
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: {
      env: { FLEETDECK_HOLD_MS: String(holdMs), FLEETDECK_REARM_GRACE_MS: '0' },
    },
  });

  const t0 = Date.now();
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);
  const heldRes = await held; // never answered
  const elapsed = Date.now() - t0;

  assert.ok(
    Math.abs(elapsed - holdMs) <= 800,
    `hold should resolve within +/-800ms of ${holdMs}ms (took ${elapsed}ms)`,
  );
  assert.deepEqual(
    heldRes.json,
    {},
    'expiry must resolve to {} so the native terminal chooser renders as normal',
  );

  const state = await getState<StateResponse>(daemon.baseUrl);
  const q2 = state.questions.find((x) => String(x.id) === String(q.id));
  assert.equal(q2?.status, 'expired');

  // once expired, the terminal owns the question: a late board answer 409s
  const late = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'argon2' },
  });
  assert.equal(
    late.status,
    409,
    'a late answer to an expired choice must be refused — the terminal chooser owns it now',
  );
});

// ---------------------------------------------------------------------------
// CRITICAL guard: PermissionRequest for AskUserQuestion is NEVER held
// ---------------------------------------------------------------------------

test('F3c: /hook/PermissionRequest with tool_name=AskUserQuestion answers {} in <200ms even while another hold is open (and leaves that hold undisturbed)', async (t) => {
  const holdMs = 6000; // long, so a wrongly-held request would blow the 200ms budget by construction
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });

  // open a choice hold first — the guard must answer around it, not through it
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  // the PermissionRequest side-effect event the live run captured: same
  // tool_input, tool_name AskUserQuestion, no permission_suggestions
  const permPayload = loadFixture(
    'ask-user-question',
    { session_id: sid, cwd },
    { hook_event_name: 'PermissionRequest' },
  );
  delete permPayload['tool_use_id'];
  const t0 = Date.now();
  const permRes = await postHook(daemon.baseUrl, 'PermissionRequest', permPayload, {
    token: daemon,
  });
  const elapsed = Date.now() - t0;

  assert.equal(permRes.status, 200);
  assert.deepEqual(
    permRes.json,
    {},
    'PermissionRequest for AskUserQuestion must answer {} — the question already had its hold at PreToolUse',
  );
  assert.ok(
    elapsed < scaleMs(200),
    `PermissionRequest for AskUserQuestion must answer immediately, never hold (took ${elapsed}ms)`,
  );

  // no second question row was created for it
  const state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(
    questionsFor(state, sid, 'permission').length,
    0,
    'the guard must not create a permission question row',
  );

  // the original choice hold is still live and still answerable
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'bcrypt' },
  });
  assert.equal(ansRes.status, 200, 'the choice hold must survive the PermissionRequest guard');
  const heldRes = await held;
  assert.deepEqual(relayedAnswers(heldRes.json), { [FIXTURE_QUESTION]: 'bcrypt' });
});

// ---------------------------------------------------------------------------
// multi-question / multiSelect serialization + {text} fallback
// ---------------------------------------------------------------------------

test('F3c: multi-question answers serialize compactly (header: label; multiSelect labels joined)', async (t) => {
  const holdMs = 4000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });

  const toolInput = {
    questions: [
      {
        question: 'Which hashing algorithm?',
        header: 'Hashing',
        options: [{ label: 'bcrypt' }, { label: 'argon2' }],
        multiSelect: false,
      },
      {
        question: 'Which deploy targets?',
        header: 'Deploy',
        options: [{ label: 'staging' }, { label: 'prod' }, { label: 'docker' }],
        multiSelect: true,
      },
    ],
  };
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs, { tool_input: toolInput });

  const partial = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { 'Which hashing algorithm?': 'argon2' },
  });
  assert.equal(partial.status, 400, 'a partial multi-question answer must stay retryable');
  const pending = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(pending.questions.find((x) => String(x.id) === String(q.id))?.status, 'pending');
  assert.equal(pending.questions.find((x) => String(x.id) === String(q.id))?.held, true);

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: {
      'Which hashing algorithm?': 'argon2',
      'Which deploy targets?': ['staging', 'docker'], // arrays accepted for multiSelect
    },
  });
  assert.equal(ansRes.status, 200);

  const heldRes = await held;
  assert.deepEqual(relayedAnswers(heldRes.json), {
    'Which hashing algorithm?': 'argon2',
    'Which deploy targets?': 'staging, docker',
  });
});

test('F3c: {text} freeform fallback answers a choice hold with the text as the relayed answer', async (t) => {
  const holdMs = 4000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });

  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const bad = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { answers: {} });
  assert.equal(bad.status, 400, 'an empty answers map must be rejected');

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    text: 'neither — use scrypt, and ask me about cost params later',
  });
  assert.equal(ansRes.status, 200);
  const heldRes = await held;
  assert.deepEqual(relayedAnswers(heldRes.json), {
    [FIXTURE_QUESTION]: 'neither — use scrypt, and ask me about cost params later',
  });
});

// ---------------------------------------------------------------------------
// BUG-139: an answer is the operator's decision, not a display string — it
// is relayed in full (never the 300-unit display clamp), and a serialized
// answer over the documented 2000-unit limit is rejected BEFORE the hold is
// released rather than silently clipped. Length is compared in code units
// but slicing never happens, so no surrogate pair can be split.
// ---------------------------------------------------------------------------

test('BUG-139: a long {text} answer (>300 units) is relayed to the agent in full — no silent clip', async (t) => {
  const holdMs = 4000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });

  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const long = `neither — ${'x'.repeat(400)}`;
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: long });
  assert.equal(ansRes.status, 200, 'a >300-unit answer under the 2000-unit limit must be accepted');
  const heldRes = await held;
  assert.deepEqual(
    relayedAnswers(heldRes.json),
    { [FIXTURE_QUESTION]: long },
    'the agent must receive the COMPLETE answer — no display clamp, no ellipsis',
  );
});

test('BUG-139: an answer over the 2000-unit limit is rejected with 400 and the hold stays pending (no silent truncation)', async (t) => {
  const holdMs = 60000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });

  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const big = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    text: 'y'.repeat(2500),
  });
  assert.equal(big.status, 400, 'an oversized serialized answer must be rejected, not clipped');
  assert.match((big.json as AnswerResponse | null)?.err ?? '', /too long/);
  let state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(
    state.questions.find((x) => String(x.id) === String(q.id))?.status,
    'pending',
    'a rejected answer must NOT settle the hold — the operator can retry',
  );

  const longAnswers = { [FIXTURE_QUESTION]: 'z'.repeat(2500) };
  const big2 = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: longAnswers,
  });
  assert.equal(big2.status, 400, 'an oversized answers-map serialization is rejected too');
  state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(
    state.questions.find((x) => String(x.id) === String(q.id))?.status,
    'pending',
    'still retryable',
  );

  const ok = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'bcrypt' },
  });
  assert.equal(ok.status, 200, 'the hold is still answerable after the rejections');
  const heldRes = await held;
  assert.deepEqual(relayedAnswers(heldRes.json), { [FIXTURE_QUESTION]: 'bcrypt' });
});

// ---------------------------------------------------------------------------
// BUG-140: an answers map that does not match the held question schema must
// be REJECTED (400) and the hold must stay open — never String()-coerce an
// object into "[object Object]", settle the question, and suppress the
// native chooser with meaningless input
// ---------------------------------------------------------------------------

test('BUG-140: malformed answers maps are rejected with 400 and the choice hold stays open for a valid answer', async (t) => {
  const holdMs = 4000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });

  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const malformed: Record<string, unknown>[] = [
    { answers: { [FIXTURE_QUESTION]: { bogus: 'object' } } }, // object value → would become "[object Object]"
    { answers: { 'what is this question?': 'argon2' } }, // key is not a held question's text
    { answers: { [FIXTURE_QUESTION]: 'scrypt' } }, // label not among the question's options
    { answers: { [FIXTURE_QUESTION]: ['bcrypt', 'argon2'] } }, // array on a non-multiSelect question
    { answers: { [FIXTURE_QUESTION]: 42 } }, // non-string scalar
  ];
  for (const body of malformed) {
    const res = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, body);
    assert.equal(res.status, 400, `malformed answers must be rejected: ${JSON.stringify(body)}`);
    const state = await getState<StateResponse>(daemon.baseUrl);
    const qNow = state.questions.find((x) => String(x.id) === String(q.id));
    assert(qNow);
    assert.equal(qNow.status, 'pending', 'a rejected answer must not settle the question');
    assert.equal(qNow.held, true, 'a rejected answer must keep the hold open');
  }

  // the hold is still answerable with a valid map afterwards
  const good = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    answers: { [FIXTURE_QUESTION]: 'bcrypt' },
  });
  assert.equal(good.status, 200);
  const heldRes = await held;
  assert.deepEqual(relayedAnswers(heldRes.json), { [FIXTURE_QUESTION]: 'bcrypt' });
});

// ---------------------------------------------------------------------------
// activity / SessionEnd expiry parity with the permission kind
// ---------------------------------------------------------------------------

test('F3c: session activity (UserPromptSubmit) expires a pending choice hold with {} promptly — identical to the permission kind', async (t) => {
  const holdMs = 6000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const t0 = Date.now();
  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
    { token: daemon },
  );
  const heldRes = await held;
  const elapsed = Date.now() - t0;

  assert.ok(
    elapsed < holdMs,
    `activity-triggered expiry should resolve the hold promptly (took ${elapsed}ms)`,
  );
  assert.deepEqual(heldRes.json, {}, 'activity-expired choice hold must resolve to {}');
  const state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(state.questions.find((x) => String(x.id) === String(q.id))?.status, 'expired');
});

test('F3c: SessionEnd expires a pending choice hold with {} — identical to the permission kind', async (t) => {
  const holdMs = 6000;
  const { daemon, cwd, sid } = await startRegisteredDaemon(t, {
    daemon: { env: { FLEETDECK_HOLD_MS: String(holdMs) } },
  });
  const { held, q } = await holdChoice(daemon, sid, cwd, holdMs);

  const t0 = Date.now();
  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    loadFixture('session-end', { session_id: sid, cwd }),
    { token: daemon },
  );
  const heldRes = await held;
  const elapsed = Date.now() - t0;

  assert.ok(elapsed < holdMs, `SessionEnd should settle the hold promptly (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'SessionEnd-expired choice hold must resolve to {}');
  const state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(state.questions.find((x) => String(x.id) === String(q.id))?.status, 'expired');
});
