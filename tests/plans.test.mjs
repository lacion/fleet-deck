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
// IN-TERMINAL SETTLEMENT (UX 2.2): a plan question retired WITHOUT a board
// answer flips its 'proposed' plan to the terminal status
// 'handled-in-terminal' once the session shows ACTIVITY — the human decided
// in the terminal and the agent moved on. A retirement that IS activity (the
// turn-boundary expireOnActivity path) settles in the same tick; a bare
// timer expiry with NO subsequent activity settles NEVER (a planner killed
// mid-hold must not be marked).
// LIBRARY: GET /state `plans` (non-archived, newest first, cap 20). POST
// /api/plans/:id/mark {status} allows proposed|approved|captured -> executed
// (optional {via} recorded) and any non-archived (incl. handled-in-terminal)
// -> archived; 404 unknown, 409 bad transition. BUG-041 (daemon half):
// marking executed while the plan's question is still PENDING dismisses that
// question through the ordinary dismiss path (the held hook fails open).
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
//   6. In-terminal settlement (UX 2.2)
//        -> "in-terminal: turn-boundary activity ..."
//        -> "in-terminal: timer expiry then activity ..."
//        -> "in-terminal: timer expiry with NO activity ..."
//        -> "in-terminal: a plan question raised in the SAME turn ..."
//        -> "in-terminal: board answer regression ..."
//        -> "in-terminal: handled-in-terminal cannot be marked executed ..."
//   7. BUG-041 daemon half
//        -> "mark executed with the question still pending dismisses it ..."
//
// Fixture: tests/fixtures/exit-plan-mode.json — a PermissionRequest payload,
// tool_name ExitPlanMode, tool_input.plan a realistic multi-line markdown
// document (headings, an ordered list, a fenced code block) so byte-fidelity
// through JSON-body -> SQLite -> /state JSON is actually exercised, not just
// a one-line string that any naive truncation/escaping bug would pass.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, existsSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { waitUntil, waitForSpecRecords } from './helpers/wait.mjs';
import { fileURLToPath } from 'node:url';

const EXIT_PLAN_FIXTURE = 'exit-plan-mode';

// BUG-040 tests spawn through the FLEETDECK_SPAWN_CMD test seam (see
// tests/spawn.test.mjs) so no real billed `claude` ever launches.
const SPAWN_CMD_FIXTURE = path.join(
  path.dirname(fileURLToPath(import.meta.url)), 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best-effort */ }

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

/** POST the ExitPlanMode fixture (held) and wait for its permission question
 * (kind 'permission', payload.tool_name === 'ExitPlanMode', status pending)
 * to register in /state. The finder takes the NEWEST pending match — tests
 * that raise a second plan for the same session would otherwise re-find the
 * first question's resolved row. `tool_input` in overrides is deep-merged
 * one level over the fixture's (the shallow loadFixture merge would
 * otherwise drop the plan text when a caller only needs to retitle the
 * tool call). Returns {held (the pending fetch promise), q (the /state
 * question entry), payload (what was actually POSTed, for byte-identity
 * checks)}. */
async function holdExitPlan(daemon, sid, cwd, holdMs, overrides = {}) {
  const merged = { ...overrides };
  if (overrides.tool_input) {
    merged.tool_input = { ...loadFixture(EXIT_PLAN_FIXTURE, { session_id: sid, cwd }).tool_input, ...overrides.tool_input };
  }
  const payload = loadFixture(EXIT_PLAN_FIXTURE, { session_id: sid, cwd }, merged);
  const held = postHook(daemon.baseUrl, 'PermissionRequest', payload, { token: daemon, timeout: holdMs + 5000 });
  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return questionsFor(state, sid, 'permission')
      .filter(x => x.payload?.tool_name === 'ExitPlanMode' && x.status === 'pending')
      .sort((a, b) => b.id - a.id)[0];
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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

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
  const upRes = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }), { token: daemon });
  const ctx = upRes.json?.hookSpecificOutput?.additionalContext ?? '';
  assert.match(ctx, /\[FLEETDECK\] Your plan was captured/, `the planner's next UserPromptSubmit must carry the verbatim "[FLEETDECK] Your plan was captured" prefix (got: ${JSON.stringify(ctx)})`);
  assert.match(ctx, /do not execute it/i, 'the pinned capture mail should tell the planner not to execute the plan');
});

test('answer path: {behavior:"deny"} plainly denies the held hook; plan becomes rejected', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

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

test('answer path: an unanswered ExitPlanMode hold expires to {} and the plan stays proposed (no activity follows, so it must NEVER settle)', async (t) => {
  // holdMs under the UX 2.1 re-arm floor keeps the expiry a single decisive
  // event (a re-armed expiry would leave the row pending through the grace
  // window and this test would need to wait it out).
  const holdMs = 1200;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const t0 = Date.now();
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;

  const heldRes = await held; // intentionally never answered
  const elapsed = Date.now() - t0;
  assert.ok(Math.abs(elapsed - holdMs) <= 800, `hold should resolve within +/-800ms of ${holdMs}ms (took ${elapsed}ms)`);
  assert.deepEqual(heldRes.json, {}, 'expiry should resolve to {} same as any other permission hold');

  // No activity may follow, or the UX 2.2 gate settles the plan — which is
  // exactly what the dedicated in-terminal tests below assert. Wait out the
  // re-arm grace floor (sub-floor holdMs disables re-arming, but the tick is
  // cheap insurance) before reading the status.
  await new Promise(r => setTimeout(r, 100));

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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const payload = loadFixture('permission-request', { session_id: sid, cwd }, { tool_name: 'AskUserQuestion' });
  const t0 = Date.now();
  const res = await postHook(daemon.baseUrl, 'PermissionRequest', payload, { token: daemon });
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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const TOTAL = 25;
  const pending = [];
  const indexOf = md => Number(/# Test Plan (\d+)/.exec(md)?.[1]);

  for (let i = 0; i < TOTAL; i++) {
    const sid = randomUUID();
    const planMd = `# Test Plan ${i}\n\n## Notes\n- marker ${i}\n- cap/ordering test fixture\n`;
    const payload = loadFixture(EXIT_PLAN_FIXTURE, { session_id: sid, cwd }, { tool_input: { plan: planMd } });
    pending.push(postHook(daemon.baseUrl, 'PermissionRequest', payload, { token: daemon, timeout: holdMs + 4000 }));
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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

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
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

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

// ---------------------------------------------------------------------------
// 5b. BUG-039 — daemon-side Assign. The board's Assign control used to compose
// the [FLEETDECK ASSIGNMENT] frame client-side and post it through /mail,
// which 422s every reserved frame (0.16.0) — so Assign never delivered and
// the plan was never marked executed. POST /api/plans/:id/assign is the
// daemon-authorized path: it mails the framed plan through the internal
// mail() and records the executed verdict in the same request.
// ---------------------------------------------------------------------------

test('assign: mails the daemon-reserved [FLEETDECK ASSIGNMENT] frame to the target and marks the plan executed', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // Planner holds ExitPlanMode → plan captured (status proposed).
  const plannerSid = randomUUID();
  const { held, payload } = await holdExitPlan(daemon, plannerSid, cwd, holdMs);
  const state0 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan0 = plansFor(state0, plannerSid)[0];
  assert.ok(plan0, 'sanity: plan captured');
  assert.equal(plan0.status, 'proposed');

  // Target: a live session the plan gets assigned to.
  const targetSid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: targetSid, cwd }), { token: daemon });
  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const targetCard = findSession(state1, targetSid);
  assert.ok(targetCard, 'sanity: target session card exists');

  const res = await postJson(`${daemon.baseUrl}/api/plans/${plan0.plan_id}/assign`, {
    to: targetSid,
    instructions: 'run only the migration',
  });
  assert.equal(res.status, 200, `assign should 200 (got ${res.status}: ${JSON.stringify(res.json)})`);
  assert.equal(res.json?.ok, true);
  assert.equal(res.json?.session_id, targetSid, 'assign should report the resolved target session id');

  // The assignment frame reached the target's mailbox VERBATIM — the exact
  // reserved frame POST /mail refuses to carry for an external caller.
  const box = (await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(targetSid)}`)).json;
  assert.equal(box.mail.length, 1, 'target should have exactly one mail (the assignment)');
  const m = box.mail[0];
  assert.equal(m.from, 'orchestrator', 'the assignment frame is daemon-sent (reserved sender, never board-forged)');
  assert.ok(m.text.startsWith('[FLEETDECK ASSIGNMENT]'), `mail must open with the reserved assignment frame (got: ${m.text.slice(0, 60)}...)`);
  assert.ok(m.text.includes('Custom instructions: run only the migration'), 'custom instructions ride the frame');
  assert.ok(m.text.includes(payload.tool_input.plan), 'the full plan markdown is the frame body');

  // The plan was recorded executed atomically with the assignment.
  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan2 = (state2.plans || []).find(p => String(p.plan_id) === String(plan0.plan_id));
  assert.equal(plan2?.status, 'executed', 'assign must mark the plan executed');
  if (plan2?.via !== undefined) {
    assert.equal(plan2.via, `assign:${targetSid}`, 'when `via` is exposed it should record the assign target');
  }

  // BUG-041 comes along for free: assigning while the planner's question is
  // still pending dismisses it, and the held hook resolves.
  const ans = await held;
  assert.equal(ans.status, 200, 'the planner hold should resolve once the plan is assigned elsewhere');
});

test('assign: bad requests and bad transitions are refused without sending mail', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const plannerSid = randomUUID();
  const { held, q } = await holdExitPlan(daemon, plannerSid, cwd, holdMs);
  const state0 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan0 = plansFor(state0, plannerSid)[0];

  const targetSid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: targetSid, cwd }), { token: daemon });

  // 404 unknown plan / unknown target, 400 missing or mistyped fields.
  const unknownPlan = await postJson(`${daemon.baseUrl}/api/plans/999999999/assign`, { to: targetSid });
  assert.equal(unknownPlan.status, 404, `assigning an unknown plan should 404 (got ${unknownPlan.status})`);
  const unknownTarget = await postJson(`${daemon.baseUrl}/api/plans/${plan0.plan_id}/assign`, { to: 'no-such-session-xyz' });
  assert.equal(unknownTarget.status, 404, `assigning to an unknown target should 404 (got ${unknownTarget.status})`);
  const missingTo = await postJson(`${daemon.baseUrl}/api/plans/${plan0.plan_id}/assign`, {});
  assert.equal(missingTo.status, 400, `assign without a target should 400 (got ${missingTo.status})`);
  const badInstr = await postJson(`${daemon.baseUrl}/api/plans/${plan0.plan_id}/assign`, { to: targetSid, instructions: 42 });
  assert.equal(badInstr.status, 400, `non-string instructions should 400 (got ${badInstr.status})`);

  // Reject the plan (deny answer) — a settled plan can no longer be assigned.
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'deny' });
  assert.equal(ansRes.status, 200);
  await held;
  const rejected = await postJson(`${daemon.baseUrl}/api/plans/${plan0.plan_id}/assign`, { to: targetSid });
  assert.equal(rejected.status, 409, `assigning a rejected plan should 409 (got ${rejected.status}: ${JSON.stringify(rejected.json)})`);

  // None of the refused assigns queued mail for the target.
  const box = (await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(targetSid)}`)).json;
  assert.equal(box.mail.length, 0, 'a refused assign must never queue the reserved frame');
});

// ---------------------------------------------------------------------------
// 6. In-terminal settlement (UX 2.2): a plan question retired WITHOUT a board
// answer settles its 'proposed' plan to 'handled-in-terminal' once the
// session shows ACTIVITY — never on bare timer expiry.
// ---------------------------------------------------------------------------

test('in-terminal: turn-boundary activity retires the question and settles the plan handled-in-terminal in the same tick', async (t) => {
  // The real-world shape: the planner's ExitPlanMode hold expires to {}, the
  // native terminal chooser owns the decision, the human approves there, and
  // the agent's next turn (UserPromptSubmit) is the observable proof. The
  // retire IS the activity, so no second event is needed.
  const holdMs = 1000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  assert.ok(planId !== undefined, 'sanity: plan captured');
  await held; // hold expires to {}; the terminal prompt owns the decision now

  const state0 = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((state0.plans || []).find(p => String(p.plan_id) === String(planId))?.status, 'proposed',
    'bare expiry must leave the plan proposed — no activity has been seen yet');

  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'looks good, go' }), { token: daemon });

  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan1 = (state1.plans || []).find(p => String(p.plan_id) === String(planId));
  assert.equal(plan1?.status, 'handled-in-terminal',
    `the turn boundary must settle the plan to handled-in-terminal within one snapshot refresh (got ${plan1?.status})`);
  const q1 = questionsFor(state1, sid, 'permission').find(x => x.payload?.tool_name === 'ExitPlanMode');
  assert.equal(q1?.status, 'expired', 'the question row retires as expired, not answered — nobody answered it from the board');
});

test('in-terminal: timer expiry THEN later PostToolUse activity settles handled-in-terminal (the deferred gate)', async (t) => {
  const holdMs = 1000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  await held;

  assert.equal(
    (await getJson(`${daemon.baseUrl}/state`)).json.plans.find(p => String(p.plan_id) === String(planId))?.status,
    'proposed', 'expiry alone is not a decision — the plan must wait for activity');

  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-edit', { session_id: sid, cwd }), { token: daemon });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((state.plans || []).find(p => String(p.plan_id) === String(planId))?.status, 'handled-in-terminal',
    'the first activity event after expiry must settle the plan');
});

test('in-terminal: timer expiry with NO subsequent activity never settles — the plan stays proposed', async (t) => {
  const holdMs = 900;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  await held;

  // Sit well past the expiry with NO hook activity from the session. A
  // planner killed mid-hold looks exactly like this — marking it would be a
  // lie the library then offers Execute/Assign against.
  await new Promise(r => setTimeout(r, 1200));
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((state.plans || []).find(p => String(p.plan_id) === String(planId))?.status, 'proposed',
    'expiry with no activity must NEVER settle the plan (killed-planner guard)');
});

test('in-terminal: a plan question raised in the SAME turn (still pending, never parked) must NOT be flipped by the gate', async (t) => {
  const holdMs = 60_000; // long window: no hold expires on its own in this test
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  // First plan: the human dismisses it on the board (retired unanswered,
  // without session activity) — armed, awaiting the activity gate.
  const first = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId1 = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  const disRes = await postJson(`${daemon.baseUrl}/api/questions/${first.q.id}/dismiss`, {});
  assert.equal(disRes.status, 200);
  assert.deepEqual((await first.held).json, {}, 'sanity: the dismissal failed the first hold open');

  // A turn boundary arrives and the planner — in the SAME turn — raises a
  // SECOND ExitPlanMode question. The hook order is deliberately
  // UserPromptSubmit FIRST: the gate settles the dismissed plan at that
  // boundary, and the second hold's row lands only when the CLI calls
  // ExitPlanMode inside the new turn (its hookHoldQuestion runs applyEvent
  // — activity — BEFORE the row insert and BEFORE the socket parks, so no
  // activity-driven scan can ever see that plan as settleable). The second
  // plan must stay proposed: its prompt has not been seen by anyone.
  //
  // tool_input gets a distinct command so the second question never
  // coincides with the first one's identity anywhere identity is read.
  const upsRes = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'looks good, one change' }), { token: daemon });
  assert.equal(upsRes.status, 200);
  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((state1.plans || []).find(p => String(p.plan_id) === String(planId1))?.status, 'handled-in-terminal',
    'the dismissed plan settles at the turn boundary');

  const second = await holdExitPlan(daemon, sid, cwd, holdMs, { tool_input: { command: 'revise the plan' } });
  const plansNow = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  const planId2 = plansNow.find(p => String(p.plan_id) !== String(planId1))?.plan_id;
  assert.ok(planId2 !== undefined, 'sanity: second plan captured');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((state.plans || []).find(p => String(p.plan_id) === String(planId2))?.status, 'proposed',
    'the plan raised in the SAME turn must NOT be flipped — its prompt was never seen');
  const q2 = questionsFor(state, sid, 'permission').find(x => x.id === second.q.id);
  assert.equal(q2?.status, 'pending', 'the second question is still pending');
  assert.equal(q2?.held, true, 'the second hold is still parked (its chooser never rendered)');

  // clean up: answer the second hold from the board so nothing dangles.
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${second.q.id}/answer`, { behavior: 'deny' });
  assert.equal(ansRes.status, 200);
  await second.held;
  const stateEnd = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal((stateEnd.plans || []).find(p => String(p.plan_id) === String(planId2))?.status, 'rejected',
    'board answer regression: deny on the second plan still flips it to rejected');
});

test('in-terminal: board answer regression — allow still flips approved and is never re-settled by later activity', async (t) => {
  const holdMs = 1500;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held, q } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'allow' });
  assert.equal(ansRes.status, 200);
  await held;
  assert.equal((await getJson(`${daemon.baseUrl}/state`)).json.plans.find(p => String(p.plan_id) === String(planId))?.status, 'approved');

  // Later activity from the session must NOT touch the board's verdict.
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }), { token: daemon });
  assert.equal((await getJson(`${daemon.baseUrl}/state`)).json.plans.find(p => String(p.plan_id) === String(planId))?.status, 'approved',
    'the activity gate only ever touches proposed plans — a board verdict is final');
});

test('in-terminal: handled-in-terminal cannot be marked executed (409), can be archived', async (t) => {
  const holdMs = 1000;
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held } = await holdExitPlan(daemon, sid, cwd, holdMs);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  await held;
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'go' }), { token: daemon });
  assert.equal((await getJson(`${daemon.baseUrl}/state`)).json.plans.find(p => String(p.plan_id) === String(planId))?.status, 'handled-in-terminal',
    'sanity: plan settled before probing the mark matrix');

  const execRes = await postJson(`${daemon.baseUrl}/api/plans/${planId}/mark`, { status: 'executed' });
  assert.equal(execRes.status, 409,
    `handled-in-terminal -> executed must 409 (got ${execRes.status}: ${JSON.stringify(execRes.json)})`);

  const archRes = await postJson(`${daemon.baseUrl}/api/plans/${planId}/mark`, { status: 'archived' });
  assert.equal(archRes.status, 200,
    `handled-in-terminal -> archived should 200 like every non-archived status (got ${archRes.status}: ${JSON.stringify(archRes.json)})`);
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(!(state.plans || []).some(p => String(p.plan_id) === String(planId)), 'archived plan leaves /state plans');
});

// ---------------------------------------------------------------------------
// 7. BUG-041 (daemon half): marking a plan executed while its ExitPlanMode
// question is STILL PENDING retires that question through the dismiss path —
// the planner must not sit parked on a stale prompt.
// ---------------------------------------------------------------------------

test('mark executed with the question still pending dismisses it (fails the hold open with {})', async (t) => {
  const holdMs = 60_000; // the hold must still be parked when the mark lands
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
  let held = Promise.resolve(null); // rebound once the hold POST is in flight
  const cwd = scratchCwd();
  // holdExitPlan's held POST outlives the daemon by the length of the hold
  // window; the daemon's own SIGKILL/SIGTERM then leaves that fetch hung on
  // an open-but-dead connection, outliving the daemon.stop() in t.after and
  // wedging test-runner teardown. Kill the child ourselves first, then wait
  // the stray fetch out before letting t.after remove the home dir.
  t.after(async () => {
    try { daemon.proc.kill('SIGKILL'); } catch { /* already gone */ }
    await Promise.allSettled([held]);
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const out = await holdExitPlan(daemon, sid, cwd, holdMs);
  held = out.held;
  const { q } = out;
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;

  const markRes = await postJson(`${daemon.baseUrl}/api/plans/${planId}/mark`, { status: 'executed', via: 'assign' });
  assert.equal(markRes.status, 200);

  const heldRes = await held;
  assert.deepEqual(heldRes.json, {},
    'the parked hook must fail OPEN (dismiss path) — the planner resumes in the terminal instead of sitting on a stale prompt');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const plan = (state.plans || []).find(p => String(p.plan_id) === String(planId));
  assert.equal(plan?.status, 'executed', 'the mark itself is untouched');
  const qrow = questionsFor(state, sid, 'permission').find(x => x.id === q.id);
  assert.equal(qrow?.status, 'expired', 'the dismissed question retires as expired');
});

// ---------------------------------------------------------------------------
// 8. BUG-040 — atomic pre-spawn execution claim. The board's plan-execute
// flow used to spawn FIRST and mark the plan executed only after the daemon
// accepted, so two boards acting on one stale snapshot both launched billed
// agents before either mark could 409. plan_id now rides the spawn body and
// the daemon claims the plan (one guarded UPDATE) BEFORE any clone, pane, or
// durable row exists; a spawn failure releases the claim back to its
// pre-claim status. Spawns here go through FLEETDECK_SPAWN_CMD (the fixture
// records specs, never launches Claude).
// ---------------------------------------------------------------------------

async function captureProposedPlan(t, daemon, cwd) {
  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held } = await holdExitPlan(daemon, sid, cwd, 1000);
  const plan = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0];
  assert.ok(plan, 'sanity: plan captured');
  assert.equal(plan.status, 'proposed');
  await held; // hold expires; a planner with no follow-up activity stays proposed
  return plan;
}

function planById(state, planId) {
  return (state.plans || []).find(p => String(p.plan_id) === String(planId));
}

test('BUG-040: spawn carrying plan_id claims the plan atomically BEFORE launch; a second claim 409s and launches nothing', async (t) => {
  const recDir = scratchCwd();
  const rec = path.join(recDir, 'specs.jsonl');
  const cwd = scratchCwd();
  const daemon = await startDaemon({ env: {
    FLEETDECK_HOLD_MS: '1000',
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: rec,
  } });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(recDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const plan = await captureProposedPlan(t, daemon, cwd);

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'Execute this approved plan exactly.', plan_id: plan.plan_id });
  assert.equal(res.status, 200, `the claiming spawn should succeed (got ${res.status}: ${JSON.stringify(res.json)})`);
  assert.equal(res.json.ok, true);

  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  const claimed = planById(state1, plan.plan_id);
  assert.equal(claimed?.status, 'executed', 'the plan must be executed the moment the spawn is accepted, not after a client mark');
  if (claimed?.via !== undefined) {
    assert.equal(claimed.via, `spawn:${res.json.spawn_id}`, 'the recorded via must name the spawn that executed the plan');
  }

  // The race that motivated the fix: a second board on a stale snapshot
  // executes the SAME plan. It must be refused before launch — and must
  // never reach the spawn backend.
  const res2 = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'Execute this approved plan exactly.', plan_id: plan.plan_id });
  assert.equal(res2.status, 409, `a second execution claim must 409 (got ${res2.status}: ${JSON.stringify(res2.json)})`);
  assert.equal(res2.json.ok, false);

  const specs = await waitForSpecRecords(rec, 1);
  assert.equal(specs.length, 1, 'exactly ONE executor was launched; the losing claim never reached the spawn backend');

  // The claim is terminal through the mark endpoint too — the old client-composed
  // second mark (the race's honest half) still 409s verbatim.
  const markRes = await postJson(`${daemon.baseUrl}/api/plans/${plan.plan_id}/mark`, { status: 'executed', via: 'spawn:late' });
  assert.equal(markRes.status, 409, 'executed-from-executed still 409s');
});

test('BUG-040: a refused spawn releases the claim — plan returns to proposed and is executable again', async (t) => {
  const recDir = scratchCwd();
  const rec = path.join(recDir, 'specs.jsonl');
  const cwd = scratchCwd();
  const daemon = await startDaemon({ env: {
    FLEETDECK_HOLD_MS: '1000',
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: rec,
  } });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(recDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const plan = await captureProposedPlan(t, daemon, cwd);

  // cwd missing → the spawn 400s AFTER the claim; the claim must be released.
  const bad = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd: path.join(cwd, 'does-not-exist'), prompt: 'x', plan_id: plan.plan_id });
  assert.equal(bad.status, 400, `a spawn with a missing cwd must still 400 (got ${bad.status}: ${JSON.stringify(bad.json)})`);

  const state1 = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(planById(state1, plan.plan_id)?.status, 'proposed',
    'a spawn the daemon refused must release the execution claim back to the pre-claim status');

  // And the released plan is executable again — the retry path a human
  // actually takes after fixing the failure.
  const good = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'Execute this approved plan exactly.', plan_id: plan.plan_id });
  assert.equal(good.status, 200, `the retry after a released claim should succeed (got ${good.status}: ${JSON.stringify(good.json)})`);
  const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(planById(state2, plan.plan_id)?.status, 'executed');

  const specs = await waitForSpecRecords(rec, 1);
  assert.equal(specs.length, 1, 'only the retried spawn launched an executor');
});

test('BUG-040: spawn claim refusals — unknown plan 404, non-executable plan 409, shell kind cannot carry plan_id', async (t) => {
  const recDir = scratchCwd();
  const rec = path.join(recDir, 'specs.jsonl');
  const cwd = scratchCwd();
  const daemon = await startDaemon({ env: {
    FLEETDECK_HOLD_MS: '1000',
    FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: rec,
  } });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(recDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const unknown = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'x', plan_id: 999999 });
  assert.equal(unknown.status, 404, `an unknown plan_id must 404 (got ${unknown.status}: ${JSON.stringify(unknown.json)})`);

  const shell = await postJson(`${daemon.baseUrl}/api/spawn`, { kind: 'shell', cwd, plan_id: 1 });
  assert.equal(shell.status, 400, `a shell spawn must refuse plan_id (got ${shell.status}: ${JSON.stringify(shell.json)})`);

  // A rejected plan is not executable: capture one, deny it, then try.
  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  const { held, q } = await holdExitPlan(daemon, sid, cwd, 1000);
  const planId = plansFor((await getJson(`${daemon.baseUrl}/state`)).json, sid)[0]?.plan_id;
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'deny' });
  assert.equal(ansRes.status, 200);
  await held;
  const denied = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'x', plan_id: planId });
  assert.equal(denied.status, 409, `a rejected plan must refuse an execution claim (got ${denied.status}: ${JSON.stringify(denied.json)})`);

  assert.equal(existsSync(rec), false, 'no refusal above may have reached the spawn backend');
});
