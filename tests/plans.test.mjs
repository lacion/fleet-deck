// tests/plans.test.mjs
//
// v1.3 — unsupervised spawns + plan library (plan library section). Built
// the same way tests/choice-relay.test.mjs and
// tests/needs-you.test.mjs were built against their v1.2/Phase-3/4 sections:
// written from the contract text, exercised against the running daemon over
// real HTTP, no daemon internals imported directly.
//
// CAPTURE: on /hook/PermissionRequest with tool_name === "ExitPlanMode",
// BEFORE holding, the daemon inserts a `plans` row (status 'proposed') and
// the held question's /state entry gains `plan_id`. ANSWER PATHS for that
// question (via the existing POST /api/questions/:id/answer):
//   {behavior:"allow"}   -> plan status 'approved',  held response = verified allow schema
//   {behavior:"capture"} -> plan status 'captured',  held response = bare deny,
//                           AND the planner's session gets mailed the pinned
//                           "[FLEETDECK] Your plan was captured..." text,
//                           delivered at its next turn boundary
//   {behavior:"deny"}    -> plan status 'rejected',  held response = bare deny
//   hold expiry          -> plan stays 'proposed' (terminal chooser owns it)
// LIBRARY: GET /state `plans` (non-archived, newest first, cap 20). POST
// /api/plans/:id/mark {status} allows proposed|approved|captured -> executed
// (optional {via} recorded) and any non-archived -> archived; 404 unknown,
// 409 bad transition.
//
// Coverage map (task brief bullets -> tests below):
//   1. Capture-before-answer
//        -> "capture-before-answer: ..."
//   2. Answer paths (allow / capture / deny / expiry)
//        -> "answer path: {behavior:allow} ..."
//        -> "answer path: {behavior:capture} ..."
//        -> "answer path: {behavior:deny} ..."
//        -> "answer path: hold expiry ..."
//   3. AskUserQuestion guard regression (v1.2 behavior untouched by v1.3)
//        -> "regression: PermissionRequest tool_name=AskUserQuestion ..."
//   4. /state plans cap 20 non-archived newest-first; archived excluded
//        -> "/state plans: caps at 20 non-archived rows, newest first; ..."
//   5. mark transitions
//        -> "mark: proposed -> executed ..."
//        -> "mark: captured -> executed ..."
//        -> "mark: archived from each non-archived status ..." (also covers
//           409 archived->executed and 409 rejected->executed as a bonus)
//        -> "mark: 404 unknown plan id ..."
//
// Fixture: tests/fixtures/exit-plan-mode.json — a PermissionRequest payload,
// tool_name ExitPlanMode, tool_input.plan a realistic multi-line markdown
// document (headings, an ordered list, a fenced code block) so byte-fidelity
// through JSON-body -> SQLite -> /state JSON is actually exercised, not just
// a one-line string that any naive truncation/escaping bug would pass.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

const EXIT_PLAN_FIXTURE = 'exit-plan-mode';

function scratchCwd() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-plans-cwd-'));
}

function findSession(state, sid) {
  return (state.sessions || []).find(s => s.session_id === sid);
}

function questionsFor(state, sid, kind) {
  return (state.questions || []).filter(q => q.session_id === sid && (!kind || q.kind === kind));
}

function plansFor(state, sid) {
  return (state.plans || []).filter(p => p.session_id === sid);
}

/** Best-effort extraction of plan_id off a /state question entry: the
 * contract only says "the held question's /state entry gains plan_id" —
 * accept it either as a top-level field or nested under payload, and report
 * via t.diagnostic which shape was actually found (a deviation to note if
 * neither). */
function questionPlanId(q) {
  if (q?.plan_id !== undefined && q.plan_id !== null) return { plan_id: q.plan_id, where: 'top-level' };
  if (q?.payload?.plan_id !== undefined && q.payload.plan_id !== null) return { plan_id: q.payload.plan_id, where: 'payload' };
  return { plan_id: undefined, where: null };
}

async function waitUntil(fn, { timeoutMs = 5000, intervalMs = 100, label = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const result = await fn();
    if (result) return result;
    if (Date.now() >= deadline) throw new Error(`waitUntil: ${label} not met within ${timeoutMs}ms`);
    await new Promise(r => setTimeout(r, intervalMs));
  }
}

/** POST the ExitPlanMode fixture (held) and wait for its permission question
 * (kind 'permission', payload.tool_name === 'ExitPlanMode') to register in
 * /state. Returns {held (the pending fetch promise), q (the /state question
 * entry), payload (what was actually POSTed, for byte-identity checks)}. */
async function holdExitPlan(daemon, sid, cwd, holdMs, overrides = {}) {
  const payload = loadFixture(EXIT_PLAN_FIXTURE, { session_id: sid, cwd }, overrides);
  const held = postHook(daemon.baseUrl, 'PermissionRequest', payload, { timeout: holdMs + 5000 });
  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission').find(x => x.payload?.tool_name === 'ExitPlanMode');
  }, { label: 'ExitPlanMode permission question to appear in /state' });
  return { held, q, payload };
}

// ---------------------------------------------------------------------------
// 1. Capture-before-answer
// ---------------------------------------------------------------------------

test('capture-before-answer: the plan row appears in /state (status proposed, plan_md byte-identical to the fixture) WHILE the ExitPlanMode question is still pending, and the question carries plan_id', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const { held, q, payload } = await holdExitPlan(daemon, sid, cwd, holdMs);
  assert.equal(q.status, 'pending', 'sanity: the question must still be pending at this point');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan = plansFor(state, sid)[0];
  assert.ok(plan, 'a plans row should exist for this session while the ExitPlanMode question is still pending (capture happens BEFORE holding)');
  assert.equal(plan.status, 'proposed', 'a freshly captured plan should be status:proposed');
  assert.equal(plan.plan_md, payload.tool_input.plan, 'plan_md must be byte-identical to the fixture\'s tool_input.plan (markdown fidelity)');
  assert.ok(plan.plan_id !== undefined && plan.plan_id !== null, 'plan row needs an id to be marked/answered against later');
  assert.ok(plan.created_at, 'plan row should carry created_at');

  const card = findSession(state, sid);
  assert.ok(card, 'sanity: the session card should exist');
  assert.equal(plan.session_id, sid);
  assert.equal(plan.callsign, card.callsign, 'plan should carry the planner session\'s callsign');
  assert.equal(plan.repo_id, card.repo_id, 'plan should carry the planner session\'s repo_id');
  assert.equal(plan.repo_name, card.repo_name, 'plan should carry the planner session\'s repo_name');

  const { plan_id: qPlanId, where } = questionPlanId(q);
  assert.ok(qPlanId !== undefined, `the held question's /state entry must carry plan_id somewhere (top-level or payload); got question: ${JSON.stringify(q)}`);
  t.diagnostic(`question.plan_id found at: ${where}`);
  assert.equal(String(qPlanId), String(plan.plan_id), 'the question\'s plan_id must match the captured plan\'s plan_id');

  // clean up: resolve the hold so nothing dangles past teardown (not the
  // focus of this test — the answer-path effects are covered separately).
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'deny' });
  assert.equal(ansRes.status, 200);
  await held;
});

// ---------------------------------------------------------------------------
// 2. Answer paths
// ---------------------------------------------------------------------------

test('answer path: {behavior:"allow"} approves the plan and the held response is the verified allow schema', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const t0 = Date.now();
  const { held, q } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  assert.ok(planId !== undefined, 'sanity: plan captured before answering');

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'allow' });
  assert.equal(ansRes.status, 200);

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `answering should resolve the hold well before the ${holdMs}ms window (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {
    hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior: 'allow' } },
  }, 'allow on an ExitPlanMode question must still produce the PermissionRequest allow schema (verified against the official hooks docs)');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan = (state.plans || []).find(p => String(p.plan_id) === String(planId));
  assert.ok(plan, 'plan row should still be present after answering');
  assert.equal(plan.status, 'approved', '{behavior:"allow"} should move the plan to approved');
});

test('answer path: {behavior:"capture"} denies the held hook bare AND mails the pinned capture notice to the planner\'s next UserPromptSubmit; plan becomes captured', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const t0 = Date.now();
  const { held, q } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  assert.ok(planId !== undefined, 'sanity: plan captured before answering');

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'capture' });
  assert.equal(ansRes.status, 200, `{behavior:"capture"} should 200 (got ${ansRes.status}: ${JSON.stringify(ansRes.json)})`);

  const heldRes = await held;
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < holdMs, `answering should resolve the hold well before the ${holdMs}ms window (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {
    hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior: 'deny' } },
  }, '{behavior:"capture"} must resolve the held hook to a BARE deny (no message field) — capture is a board-only pseudo-behavior, the planner just sees a plain deny');

  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan1 = (state1.plans || []).find(p => String(p.plan_id) === String(planId));
  assert.ok(plan1, 'plan row should still be present after answering');
  assert.equal(plan1.status, 'captured', '{behavior:"capture"} should move the plan to captured');

  // the pinned mail must reach the planner at its next turn boundary
  const upRes = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }));
  const ctx = upRes.json?.hookSpecificOutput?.additionalContext ?? '';
  assert.match(ctx, /\[FLEETDECK\] Your plan was captured/, `the planner's next UserPromptSubmit must carry the verbatim "[FLEETDECK] Your plan was captured" prefix (got: ${JSON.stringify(ctx)})`);
  assert.match(ctx, /do not execute it/i, 'the pinned capture mail should tell the planner not to execute the plan');
});

test('answer path: {behavior:"deny"} plainly denies the held hook; plan becomes rejected', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const { held, q } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'deny' });
  assert.equal(ansRes.status, 200);

  const heldRes = await held;
  assert.deepEqual(heldRes.json, {
    hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior: 'deny' } },
  }, 'plain deny on an ExitPlanMode question should be the same bare-deny schema as any other permission deny');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan = (state.plans || []).find(p => String(p.plan_id) === String(planId));
  assert.ok(plan);
  assert.equal(plan.status, 'rejected', '{behavior:"deny"} should move the plan to rejected');
});

test('answer path: an unanswered ExitPlanMode hold expires to {} and the plan stays proposed', async (t) => {
  const holdMs = 1200;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const t0 = Date.now();
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;

  const heldRes = await held; // intentionally never answered
  const elapsed = Date.now() - t0;
  assert.ok(Math.abs(elapsed - holdMs) <= 800, `hold should resolve within +/-800ms of ${holdMs}ms (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'expiry should resolve to {} same as any other permission hold');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan = (state.plans || []).find(p => String(p.plan_id) === String(planId));
  assert.ok(plan, 'plan row should still be present after hold expiry');
  assert.equal(plan.status, 'proposed', 'hold expiry must leave the plan status untouched at proposed — the terminal chooser owns the question now, not the plan');
});

// ---------------------------------------------------------------------------
// 3. AskUserQuestion guard regression (v1.2 behavior must survive v1.3 wiring)
// ---------------------------------------------------------------------------

test('regression: PermissionRequest tool_name=AskUserQuestion still answers {} in <200ms untouched by the v1.3 ExitPlanMode capture wiring', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));

  const payload = loadFixture('permission-request', { session_id: sid, cwd }, { tool_name: 'AskUserQuestion' });
  const t0 = Date.now();
  const res = await postHook(daemon.baseUrl, 'PermissionRequest', payload);
  const elapsed = Date.now() - t0;

  assert.equal(res.status, 200);
  assert.deepEqual(res.json, {}, 'PermissionRequest for AskUserQuestion must still answer {} immediately (v1.2 F3c guard)');
  assert.ok(elapsed < 200, `must answer immediately, never hold (took ${elapsed}ms)`);

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(questionsFor(state, sid, 'permission').length, 0, 'the guard must not create a permission hold row');
  assert.equal(plansFor(state, sid).length, 0, 'AskUserQuestion is not ExitPlanMode — no plan row should ever be captured for it');
});

// ---------------------------------------------------------------------------
// 4. /state plans: cap 20, non-archived, newest first; archived excluded
// ---------------------------------------------------------------------------

test('/state plans: caps at 20 non-archived rows, newest first; archiving frees a cap slot and excludes the archived row', async (t) => {
  const holdMs = 800;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const TOTAL = 25;
  const pending = [];
  const indexOf = md => Number(/# Test Plan (\d+)/.exec(md)?.[1]);

  for (let i = 0; i < TOTAL; i++) {
    const sid = randomUUID();
    const planMd = `# Test Plan ${i}\n\n## Notes\n- marker ${i}\n- cap/ordering test fixture\n`;
    const payload = loadFixture(EXIT_PLAN_FIXTURE, { session_id: sid, cwd }, { tool_input: { plan: planMd } });
    pending.push(postHook(daemon.baseUrl, 'PermissionRequest', payload, { timeout: holdMs + 4000 }));
    // small stagger so created_at (ms epoch) strictly increases in creation order
    await new Promise(r => setTimeout(r, 8));
  }

  const state = await waitUntil(async () => {
    const s = (await getJson(`${daemon.baseUrl}/state`)).json;
    return Array.isArray(s.plans) && s.plans.length >= 20 ? s : null;
  }, { label: 'at least 20 plans visible in /state', timeoutMs: 10000 });

  assert.equal(state.plans.length, 20, `/state plans must cap at 20 rows (got ${state.plans.length})`);
  for (let i = 1; i < state.plans.length; i++) {
    assert.ok(state.plans[i - 1].created_at >= state.plans[i].created_at,
      `plans must be newest-first by created_at (position ${i - 1}=${state.plans[i - 1].created_at}, position ${i}=${state.plans[i].created_at})`);
  }
  const indices = state.plans.map(p => indexOf(p.plan_md));
  const expectedVisible = new Set(Array.from({ length: 20 }, (_, k) => TOTAL - 20 + k)); // newest 20: indices 5..24
  assert.deepEqual(new Set(indices), expectedVisible,
    `expected the newest 20 plans (indices 5-24) visible, got indices: ${[...indices].sort((a, b) => a - b).join(',')}`);

  // archive the newest one (index 24): it must vanish from the listing, and
  // the next-newest previously-cap-excluded plan (index 4) must now appear.
  const newest = state.plans[0];
  assert.equal(indexOf(newest.plan_md), TOTAL - 1, 'sanity: state.plans[0] should be the newest (index 24)');
  const markRes = await postJson(`${daemon.baseUrl}/api/plans/${newest.plan_id}/mark`, { status: 'archived' });
  assert.equal(markRes.status, 200, `archiving a proposed plan should 200 (got ${markRes.status}: ${JSON.stringify(markRes.json)})`);

  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const indices2 = state2.plans.map(p => indexOf(p.plan_md));
  const expectedVisible2 = new Set(Array.from({ length: 20 }, (_, k) => 4 + k)); // now 4..23
  assert.deepEqual(new Set(indices2), expectedVisible2,
    `after archiving index 24, expected indices 4-23 visible, got: ${[...indices2].sort((a, b) => a - b).join(',')}`);

  await Promise.allSettled(pending);
});

// ---------------------------------------------------------------------------
// 5. mark transitions
// ---------------------------------------------------------------------------

test('mark: proposed -> executed (optional {via} recorded if exposed on /state, otherwise 200 alone is accepted)', async (t) => {
  const holdMs = 1000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const state0 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan0 = plansFor(state0, sid)[0];
  assert.ok(plan0, 'sanity: plan captured');
  assert.equal(plan0.status, 'proposed');

  const markRes = await postJson(`${daemon.baseUrl}/api/plans/${plan0.plan_id}/mark`, { status: 'executed', via: 'assign' });
  assert.equal(markRes.status, 200, `proposed -> executed should 200 (got ${markRes.status}: ${JSON.stringify(markRes.json)})`);

  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan1 = (state1.plans || []).find(p => String(p.plan_id) === String(plan0.plan_id));
  assert.ok(plan1, 'executed plan should still be listed (non-archived)');
  assert.equal(plan1.status, 'executed');
  if (plan1.via !== undefined) {
    assert.equal(plan1.via, 'assign', 'when `via` is exposed on /state it should carry the value passed to mark');
  } else {
    t.diagnostic('plan /state entry does not expose `via` — accepting 200 alone per the task brief');
  }

  await held; // let the still-open hold expire naturally; must not revert the mark
  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan2 = (state2.plans || []).find(p => String(p.plan_id) === String(plan0.plan_id));
  assert.equal(plan2?.status, 'executed', 'an unrelated hold timeout must not revert a plan already marked executed');
});

test('mark: captured -> executed', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const sid = randomUUID();
  const { held, q } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'capture' });
  assert.equal(ansRes.status, 200);
  await held;

  const stateA = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((stateA.plans || []).find(p => String(p.plan_id) === String(planId))?.status, 'captured', 'sanity: plan should be captured before marking executed');

  const markRes = await postJson(`${daemon.baseUrl}/api/plans/${planId}/mark`, { status: 'executed' });
  assert.equal(markRes.status, 200, `captured -> executed should 200 (got ${markRes.status}: ${JSON.stringify(markRes.json)})`);

  const stateB = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((stateB.plans || []).find(p => String(p.plan_id) === String(planId))?.status, 'executed');
});

test('mark: archived from each non-archived status (proposed, approved, captured, rejected, executed); rejected/archived -> executed both 409', async (t) => {
  const holdMs = 1200;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const behaviorFor = { approved: 'allow', rejected: 'deny', captured: 'capture', executed: 'allow' };
  const pendingHolds = [];

  async function makePlanInStatus(status) {
    const sid = randomUUID();
    const { held, q } = await holdExitPlan(daemon, sid, cwd, holdMs);
    const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
    assert.ok(planId !== undefined, `sanity: plan captured for target status ${status}`);
    if (status === 'proposed') {
      pendingHolds.push(held);
      return planId;
    }
    const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: behaviorFor[status] });
    assert.equal(ansRes.status, 200, `sanity: reaching status ${status} via answer should 200`);
    await held;
    if (status === 'executed') {
      const markRes = await postJson(`${daemon.baseUrl}/api/plans/${planId}/mark`, { status: 'executed' });
      assert.equal(markRes.status, 200, `sanity: setting up the executed fixture should 200 (got ${markRes.status})`);
    }
    return planId;
  }

  for (const status of ['proposed', 'approved', 'captured', 'executed']) {
    const planId = await makePlanInStatus(status);
    const before = (await getJson(`${daemon.baseUrl}/state`)).json;
    assert.ok((before.plans || []).some(p => String(p.plan_id) === String(planId)), `plan (status ${status}) should be visible before archiving`);

    const markRes = await postJson(`${daemon.baseUrl}/api/plans/${planId}/mark`, { status: 'archived' });
    assert.equal(markRes.status, 200, `archiving a ${status} plan should 200 (got ${markRes.status}: ${JSON.stringify(markRes.json)})`);

    const after = (await getJson(`${daemon.baseUrl}/state`)).json;
    assert.ok(!(after.plans || []).some(p => String(p.plan_id) === String(planId)), `archived ${status} plan must be excluded from /state plans`);
  }

  // rejected is exercised separately so we can additionally probe the
  // rejected -> executed bad-transition (409) BEFORE archiving it, then
  // reuse the same plan for the explicit "409 archived -> executed" case.
  const rejectedPlanId = await makePlanInStatus('rejected');
  const before = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok((before.plans || []).some(p => String(p.plan_id) === String(rejectedPlanId)), 'rejected plan should be visible before archiving');

  const badTransition = await postJson(`${daemon.baseUrl}/api/plans/${rejectedPlanId}/mark`, { status: 'executed' });
  assert.equal(badTransition.status, 409, `rejected -> executed is not an allowed transition (got ${badTransition.status}: ${JSON.stringify(badTransition.json)})`);

  const archiveRes = await postJson(`${daemon.baseUrl}/api/plans/${rejectedPlanId}/mark`, { status: 'archived' });
  assert.equal(archiveRes.status, 200, `rejected -> archived should 200 (got ${archiveRes.status}: ${JSON.stringify(archiveRes.json)})`);

  const after = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(!(after.plans || []).some(p => String(p.plan_id) === String(rejectedPlanId)), 'archived rejected plan must be excluded from /state plans');

  const archivedTransition = await postJson(`${daemon.baseUrl}/api/plans/${rejectedPlanId}/mark`, { status: 'executed' });
  assert.equal(archivedTransition.status, 409, `archived -> executed must 409 (got ${archivedTransition.status}: ${JSON.stringify(archivedTransition.json)})`);

  await Promise.allSettled(pendingHolds);
});

test('mark: 404 for an unknown plan id', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });

  const res = await postJson(`${daemon.baseUrl}/api/plans/does-not-exist-${randomUUID()}/mark`, { status: 'executed' });
  assert.equal(res.status, 404, `marking an unknown plan id should 404 (got ${res.status})`);
});
