// tests/hook-stubs.test.mjs
//
// Hook table, updated for Phase 3+4 (needs-you relay):
//   - /hook/PermissionRequest, /hook/Elicitation and (Phase 4, F3c)
//     /hook/AskUserQuestion no longer answer {} immediately — they HOLD the
//     response up to FLEETDECK_HOLD_MS and resolve via
//     POST /api/questions/:id/answer or expiry. The minimal assertion here:
//     an unanswered hold still resolves 200 {} at ~expiry and never breaks
//     the session. The full relay matrix (answer shapes, /state questions,
//     caps, activity expiry) lives in tests/needs-you.test.mjs and
//     tests/choice-relay.test.mjs.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

for (const [event, fixtureName, kind] of [
  ['PermissionRequest', 'permission-request', 'permission'],
  ['Elicitation', 'elicitation', 'elicitation'],
  ['AskUserQuestion', 'ask-user-question', 'choice'],
]) {
  test(`POST /hook/${event} holds, then resolves {} at expiry when unanswered (Phase 3)`, async (t) => {
    const holdMs = 1200;
    const daemon = await startDaemon({ env: { FLEETDECK_HOLD_MS: String(holdMs) } });
    const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
    t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

    const sid = randomUUID();
    await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

    const t0 = Date.now();
    const res = await postHook(daemon.baseUrl, event, loadFixture(fixtureName, { session_id: sid, cwd }), { token: daemon,
      timeout: holdMs + 5000,
    });
    const elapsed = Date.now() - t0;

    assert.equal(res.status, 200, `${event} must still 200 (fail open)`);
    assert.deepEqual(res.json, {}, `unanswered ${event} hold must resolve to {} so the normal flow resumes`);
    assert.ok(Math.abs(elapsed - holdMs) <= 800,
      `unanswered hold should resolve at ~FLEETDECK_HOLD_MS=${holdMs}ms (took ${elapsed}ms)`);

    // Telemetry side: the session card survives, and the question row is now expired.
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const card = state.sessions.find(s => s.session_id === sid);
    assert.ok(card, `${event} must not make the session vanish from /state`);
    const q = (state.questions || []).find(x => x.session_id === sid && x.kind === kind);
    assert.ok(q, `${event} should have created a '${kind}' question row`);
    assert.equal(q.status, 'expired', 'the unanswered question should be marked expired');
  });
}

