// tests/question-rearm.test.ts
//
// UX 2.1 — the survivable question window:
//
//   A. Re-arm with mail delivery. When a hold expires unanswered the hook
//      fails open ({}) and the agent is parked on its NATIVE terminal prompt.
//      After a short grace with NO session activity the daemon raises a FRESH
//      row (payload.rearmed === true) whose answer rides the mail pipeline to
//      the next turn boundary — the socket is gone, re-parking is impossible.
//      Chains cap at MAX_REARMS=2 and ANY session activity stops the chain.
//
//   B. TTL as a first-class setting. Default hold 600 s; the clamp ceiling
//      sits under the 660 s shim watchdog under the 720 s hooks.json timeout
//      (the lockstep invariant); hold_ms is settable through POST
//      /api/settings with FLEETDECK_HOLD_MS remaining the override.
//
// Windows here are SYNTHETIC (hundreds of ms); test D proves the TTL−1s
// delivery margin against a real parked socket on a synthetic 8 s window
// (the 600 s default is too long to pay in CI — the lockstep NUMBERS are
// pinned in the resolveHoldMs unit test instead).

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, type DaemonHandle } from './helpers/daemon.ts';
import { postHook, postJson, getJson, type JsonResponse } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { makeTranscriptDir, writeTranscript } from './helpers/transcript.ts';
import { waitUntil } from './helpers/wait.ts';
import { resolveHoldMs } from '../scripts/fleetd/questions.ts';

interface QuestionPayload {
  rearmed?: boolean;
}

interface Question {
  id: string;
  session_id: string;
  kind: string;
  status: string;
  payload?: QuestionPayload | null;
  expires_at: number | null;
  created_at: number;
}

interface QuestionsState {
  questions: Question[];
}

interface AnswerBody {
  delivered?: boolean;
  note?: string;
}

interface HookOutput {
  hookSpecificOutput?: {
    additionalContext?: string;
    decision?: { behavior?: string };
  };
}

interface SettingField {
  value?: number;
  source?: string;
}

interface SettingsBody {
  settings?: {
    hold_ms?: SettingField;
  };
}

function scratchCwd(): string {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
}

function questionsFor(state: QuestionsState, sid: string, kind?: string): Question[] {
  return state.questions.filter((q) => q.session_id === sid && (!kind || q.kind === kind));
}

// Park a permission hold and return the question row once it rides /state.
// The fetch timeout is generous (30 s): a re-arm grace timer still pending at
// teardown can hold the daemon's sockets open past a short budget, and a
// racing abort here reads as 'fetch failed', not as what it is.
async function holdPermission(
  daemon: DaemonHandle,
  sid: string,
  cwd: string,
  holdMs: number,
): Promise<{ held: Promise<JsonResponse>; q: Question }> {
  const held = postHook(
    daemon.baseUrl,
    'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: holdMs + 30_000 },
  );
  const q = await waitUntil(
    async () => {
      const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
      return questionsFor(state, sid, 'permission').find((x) => x.status === 'pending');
    },
    { label: 'permission question to appear in /state' },
  );
  return { held, q };
}

function waitForRearmed(
  daemon: DaemonHandle,
  sid: string,
  label = 're-armed successor to appear',
): Promise<Question> {
  return waitUntil(
    async () => {
      const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
      return questionsFor(state, sid).find(
        (x) => x.status === 'pending' && x.payload?.rearmed === true,
      );
    },
    { label },
  );
}

const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

// ---------------------------------------------------------------------------
// A. Re-arm lifecycle
// ---------------------------------------------------------------------------

test('re-arm: hold expiry with a still-parked session raises a fresh rearmed row; its answer rides the mail pipeline', async (t) => {
  const holdMs = 400;
  const graceMs = 300;
  const daemon = await startDaemon({
    env: { FLEETDECK_HOLD_MS: String(holdMs), FLEETDECK_REARM_GRACE_MS: String(graceMs) },
  });
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const { held, q } = await holdPermission(daemon, sid, cwd, holdMs);
  const heldRes = await held; // never answered — the hold expires
  assert.deepEqual(
    heldRes.json,
    {},
    'expiry still fails open with {} — nothing is ever auto-answered',
  );

  // The re-armed successor: same kind, rearmed flag, NO expires_at (there is
  // no socket to expire — its answer can only go by mail).
  const succ = await waitForRearmed(daemon, sid);
  assert.equal(succ.kind, 'permission');
  assert.notEqual(succ.id, q.id, 'the re-arm is a FRESH row, not the expired one resurrected');
  assert.equal(succ.payload?.rearmed, true);
  assert.equal(succ.expires_at, null, 'a re-armed row is not a hold — no expiry deadline');
  const orig = ((await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState).questions.find(
    (x) => x.id === q.id,
  );
  assert.equal(orig?.status, 'expired', 'the original row expires when the successor is raised');

  // Answer the re-armed card: 200 with the honest queued note, and the answer
  // lands as a mail row from 'fleetdeck-answer' — the freeform mechanism.
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${succ.id}/answer`, {
    behavior: 'allow',
  });
  assert.equal(ansRes.status, 200);
  assert.equal(
    (ansRes.json as AnswerBody | null)?.delivered,
    false,
    'no socket — the answer cannot be delivered synchronously',
  );
  assert.match(
    (ansRes.json as AnswerBody | null)?.note ?? '',
    /next turn boundary/,
    'the response must say what the delivery actually is',
  );

  // The answer is mail now — the freeform mechanism, drained at the next turn
  // boundary (GET /mail would ALSO drain it, so deliver through the hook).
  const upRes = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
    { token: daemon },
  );
  const ctx = (upRes.json as HookOutput | null)?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(
    ctx.includes('[FLEETDECK ANSWER]'),
    `the re-armed answer must arrive as a [FLEETDECK ANSWER] mail (got: ${ctx.slice(0, 120)})`,
  );
  assert.ok(ctx.includes('allow'), 'the mail carries the decision');
  assert.ok(
    ctx.includes('from fleetdeck-answer'),
    'the mail is from the answer pipeline, not an operator',
  );
});

test('re-arm: activity inside the grace window cancels the re-arm (no successor)', async (t) => {
  const holdMs = 400;
  const graceMs = 400;
  const daemon = await startDaemon({
    env: { FLEETDECK_HOLD_MS: String(holdMs), FLEETDECK_REARM_GRACE_MS: String(graceMs) },
  });
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const { held, q } = await holdPermission(daemon, sid, cwd, holdMs);
  await held; // hold expires; the grace timer is now armed

  // The human answers in the terminal: the session takes a new prompt well
  // inside the grace window. (UserPromptSubmit also retires the original row
  // session-wide — the pre-2.1 behaviour, unchanged.)
  await sleep(100);
  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
    { token: daemon },
  );

  // Outwait the full grace window plus margin: no re-armed card may appear.
  await sleep(graceMs + 500);
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
  const rearmed = questionsFor(state, sid).filter((x) => x.payload?.rearmed === true);
  assert.equal(rearmed.length, 0, 'activity inside the grace window must cancel the re-arm');
  const orig = state.questions.find((x) => x.id === q.id);
  assert.equal(orig?.status, 'expired');
});

test('re-arm: the chain caps at two re-arms, then the daemon gets out of the way', async (t) => {
  const holdMs = 400;
  const graceMs = 250;
  const daemon = await startDaemon({
    env: { FLEETDECK_HOLD_MS: String(holdMs), FLEETDECK_REARM_GRACE_MS: String(graceMs) },
  });
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const { held, q } = await holdPermission(daemon, sid, cwd, holdMs);
  await held;

  // An unanswered re-armed card is recycled by the 5 s orphan sweep (its next
  // grace window), so the second card takes sweep + grace to appear.
  const first = await waitForRearmed(daemon, sid, 'first re-arm');
  const second = await waitUntil(
    async () => {
      const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
      const rows = questionsFor(state, sid).filter(
        (x) => x.status === 'pending' && x.payload?.rearmed === true,
      );
      return rows.find((x) => x.id !== first.id);
    },
    { label: 'second re-arm (the cap is two, so exactly one more)', timeoutMs: 9000 },
  );

  // Cap reached: the second card is recycled by the sweep, and NO third card
  // may ever appear — the daemon gets out of the way permanently.
  await waitUntil(
    async () => {
      const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
      const row = state.questions.find((x) => x.id === second.id);
      return row?.status === 'expired' || null;
    },
    { label: 'second re-armed card recycled', timeoutMs: 9000 },
  );
  await sleep(graceMs + 600);
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
  const rearmed = questionsFor(state, sid).filter((x) => x.payload?.rearmed === true);
  assert.equal(
    rearmed.length,
    2,
    `the chain must stop at MAX_REARMS=2 (chain: ${q.id} → ${first.id} → ${second.id})`,
  );
  assert.ok(
    rearmed.every((x) => x.status !== 'pending'),
    'no re-armed row stays pending past the cap',
  );
});

test('re-arm: freeform rows never re-arm (they have no timer to expire on)', async (t) => {
  const graceMs = 300;
  const daemon = await startDaemon({ env: { FLEETDECK_REARM_GRACE_MS: String(graceMs) } });
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );
  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
    { token: daemon },
  );
  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: 'Should I use bcrypt or argon2?',
  });
  await postHook(
    daemon.baseUrl,
    'Stop',
    { session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath },
    { token: daemon.token },
  );

  let state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
  const ff = questionsFor(state, sid, 'freeform')[0];
  assert.ok(ff, 'sanity: the trailing question became a freeform card');
  assert.equal(ff.status, 'pending');

  // A freeform card lingers precisely because nothing expires it on a timer.
  // Outwait several grace windows: it must stay the ONLY card, and stay pending.
  await sleep(graceMs * 3 + 500);
  state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
  const rows = questionsFor(state, sid);
  assert.equal(
    rows.filter((x) => x.status === 'pending').length,
    1,
    'the freeform card stays pending, alone',
  );
  assert.equal(
    rows.filter((x) => x.payload?.rearmed === true).length,
    0,
    'no re-armed card may ever spawn from a freeform row',
  );
});

test('re-arm: dismissing the expired original during the grace window cancels the re-arm', async (t) => {
  const holdMs = 400;
  const graceMs = 400;
  const daemon = await startDaemon({
    env: { FLEETDECK_HOLD_MS: String(holdMs), FLEETDECK_REARM_GRACE_MS: String(graceMs) },
  });
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const { held, q } = await holdPermission(daemon, sid, cwd, holdMs);
  await held;

  const res = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/dismiss`, {});
  assert.equal(res.status, 200);

  await sleep(graceMs + 500);
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
  assert.equal(
    questionsFor(state, sid).filter((x) => x.payload?.rearmed === true).length,
    0,
    'a dismissed question must not resurrect one grace window later',
  );
});

// ---------------------------------------------------------------------------
// B. hold_ms as a first-class setting + the lockstep invariant
// ---------------------------------------------------------------------------

test('hold_ms: POST /api/settings sets the window for NEW holds; FLEETDECK_HOLD_MS stays the override', async (t) => {
  const daemon = await startDaemon(); // no env knob — the setting must drive
  const cwd = scratchCwd();
  let held: Promise<JsonResponse> | null = null; // the parked hook fetch — released before daemon.stop()
  t.after(async () => {
    await (held
      ? held.catch(() => {
          /* swallow: the parked fetch is released, not asserted on */
        })
      : Promise.resolve());
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // Default first: 600 s, source 'default'.
  let res = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(
    (res.json as SettingsBody | null)?.settings?.hold_ms?.value,
    600_000,
    'the default hold window is 600 s',
  );
  assert.equal((res.json as SettingsBody | null)?.settings?.hold_ms?.source, 'default');

  // Set it through the settings surface…
  res = await postJson(
    `${daemon.baseUrl}/api/settings`,
    { hold_ms: 5000 },
    { token: daemon.token },
  );
  assert.equal(res.status, 200);
  assert.equal((res.json as SettingsBody | null)?.settings?.hold_ms?.value, 5000);
  assert.equal((res.json as SettingsBody | null)?.settings?.hold_ms?.source, 'override');

  // …and a NEW hold carries the setting-driven deadline. (Live holds resolved
  // their window at boot — same rule FLEETDECK_HOLD_MS always had; the
  // settings row is read live by the create path.)
  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );
  const h = await holdPermission(daemon, sid, cwd, 5000);
  held = h.held; // keep the parked fetch reachable so teardown can release it
  const q = h.q;
  assert(q.expires_at !== null);
  const windowMs = q.expires_at - q.created_at;
  assert.ok(
    Math.abs(windowMs - 5000) < 1500,
    `a new hold should carry the configured 5000ms window (got ${windowMs}ms)`,
  );
  const bad = await postJson(
    `${daemon.baseUrl}/api/settings`,
    { hold_ms: 'soon' },
    { token: daemon.token },
  );
  assert.equal(bad.status, 400, 'a non-numeric hold_ms must fail loud, never silently no-op');

  // The env var stays the override.
  const daemon2 = await startDaemon({ env: { FLEETDECK_HOLD_MS: '7000' } });
  t.after(async () => {
    await daemon2.stop();
  });
  res = await postJson(
    `${daemon2.baseUrl}/api/settings`,
    { hold_ms: 5000 },
    { token: daemon2.token },
  );
  assert.equal(res.status, 200);
  res = await getJson(`${daemon2.baseUrl}/api/settings`);
  assert.equal(
    (res.json as SettingsBody | null)?.settings?.hold_ms?.value,
    7000,
    'FLEETDECK_HOLD_MS wins over the settings row',
  );
  assert.equal((res.json as SettingsBody | null)?.settings?.hold_ms?.source, 'env');
});

test('hold_ms: resolveHoldMs clamps to [250, 650_000] under the 660 s shim watchdog, default 600 s', () => {
  assert.equal(resolveHoldMs({}), 600_000, 'no env, no setting → the 600 s default');
  assert.equal(resolveHoldMs({ FLEETDECK_HOLD_MS: '45000' }), 45_000);
  assert.equal(resolveHoldMs({ FLEETDECK_HOLD_MS: '100' }), 250, 'floor clamps up');
  assert.equal(
    resolveHoldMs({ FLEETDECK_HOLD_MS: '999999' }),
    650_000,
    'the ceiling must stay under the 660 s shim watchdog — the lockstep invariant (daemon hold < watchdog < hooks.json 720 s timeout)',
  );
  assert.equal(
    resolveHoldMs({ FLEETDECK_HOLD_MS: 'junk' }),
    600_000,
    'unparseable env falls through',
  );
  // The settings row is the fallback between env and default.
  assert.equal(
    resolveHoldMs({}, () => '30000'),
    30_000,
    'the settings row drives when env is absent',
  );
  assert.equal(
    resolveHoldMs({ FLEETDECK_HOLD_MS: '45000' }, () => '30000'),
    45_000,
    'env overrides the settings row',
  );
  assert.equal(
    resolveHoldMs({}, () => '999999'),
    650_000,
    'the settings row is clamped identically',
  );
});

// ---------------------------------------------------------------------------
// D. Watchdog margin: an answer at TTL−1s still reaches the parked socket
// ---------------------------------------------------------------------------

test('answer at TTL−1s: a board answer one second before the window lapses still reaches the parked hook response', async (t) => {
  // The margin this guards is between the daemon's hold window and the shim
  // watchdog / hooks.json timeout, and only a real parked socket proves it.
  // The default window is 600 s — too long to pay in CI — so this runs a
  // synthetic 8 s hold and answers at 7 s: the same structural assertion
  // (an answer at window−1s is delivered to a socket that is still live),
  // one hundredth of the wall clock. The NUMBERS of the lockstep (600 <
  // 660 < 720) are pinned in the resolveHoldMs unit test above and in
  // hooks.json's _lockstep note.
  const HOLD_MS = 8_000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(HOLD_MS) } });
  const cwd = scratchCwd();
  let held: Promise<JsonResponse> | null = null;
  t.after(async () => {
    await (held
      ? held.catch(() => {
          /* swallow: the parked fetch is released, not asserted on */
        })
      : Promise.resolve());
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const t0 = Date.now();
  held = postHook(
    daemon.baseUrl,
    'PermissionRequest',
    loadFixture('permission-request', { session_id: sid, cwd }),
    { token: daemon, timeout: HOLD_MS + 30_000 },
  );
  const q = await waitUntil(
    async () => {
      const state = (await getJson(`${daemon.baseUrl}/state`)).json as QuestionsState;
      return questionsFor(state, sid, 'permission').find((x) => x.status === 'pending');
    },
    { label: 'permission question to appear in /state' },
  );

  const wait = Math.max(0, HOLD_MS - 1_000 - (Date.now() - t0));
  await sleep(wait);

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    behavior: 'allow',
  });
  assert.equal(ansRes.status, 200, 'an answer at TTL−1s must still find the live hold');
  assert.equal((ansRes.json as AnswerBody | null)?.delivered, true);

  const heldRes = await held;
  assert.equal(
    (heldRes.json as HookOutput | null)?.hookSpecificOutput?.decision?.behavior,
    'allow',
    'the parked socket received the decision one second before the window lapsed',
  );
});
