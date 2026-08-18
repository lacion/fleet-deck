import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { rmSync } from 'node:fs';

import test from './helpers/harness-test.ts';
import { startDaemon } from './helpers/daemon.ts';
import { loadFixture } from './helpers/fixtures.ts';
import {
  closeBoardClient,
  connectBoardClient,
  postHook,
  postJson,
  type JsonResponse,
} from './helpers/http.ts';
import { getState, questionsFor, scratchCwd } from './helpers/state.ts';
import { scaleMs, waitUntil } from './helpers/wait.ts';
import type { WebSocket } from 'ws';

interface Question {
  id: number;
  session_id: string;
  kind: string;
  status: string;
  held: boolean;
  payload?: { rearmed?: boolean } | null;
}

interface QuestionsState {
  questions: Question[];
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, scaleMs(ms)));
}

test('interactive holds exist only while an authorized board consumer is connected', async (t) => {
  const holdMs = 5000;
  const rearmGraceMs = 150;
  const daemon = await startDaemon({
    env: {
      FLEETDECK_HOLD_SCOPE: 'all',
      FLEETDECK_HOLD_MS: String(holdMs),
      FLEETDECK_REARM_GRACE_MS: String(rearmGraceMs),
    },
  });
  const cwd = scratchCwd('fleetdeck-board-presence-cwd-');
  const clients = new Set<WebSocket>();
  t.after(async () => {
    for (const ws of clients) await closeBoardClient(ws);
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const register = async (sid: string): Promise<void> => {
    await postHook(
      daemon.baseUrl,
      'SessionStart',
      loadFixture('session-start', { session_id: sid, cwd }),
      { token: daemon },
    );
  };
  const permission = (sid: string): Promise<JsonResponse> =>
    postHook(
      daemon.baseUrl,
      'PermissionRequest',
      loadFixture('permission-request', { session_id: sid, cwd }),
      { token: daemon, timeout: holdMs + 3000, boardClient: false },
    );

  // No board: even the explicit legacy/all scope must not create or park a
  // question. Claude gets its native prompt immediately.
  const unattendedSid = randomUUID();
  await register(unattendedSid);
  const started = Date.now();
  const unattended = await permission(unattendedSid);
  assert.deepEqual(unattended.json, {});
  assert.ok(
    Date.now() - started < scaleMs(1000),
    'an unattended board must fail open promptly, not wait for the hold window',
  );
  let state = await getState<QuestionsState>(daemon.baseUrl);
  assert.equal(
    questionsFor(state, unattendedSid).length,
    0,
    'observation-only intake must not leave a dead answer card',
  );

  // One authorized snapshot client: the question is genuinely held and a board
  // answer still reaches the parked hook.
  const boardA = await connectBoardClient(daemon.baseUrl, daemon.token);
  clients.add(boardA);
  const answeredSid = randomUUID();
  await register(answeredSid);
  const answeredHold = permission(answeredSid);
  const answerable = await waitUntil(
    async () => {
      const next = await getState<QuestionsState>(daemon.baseUrl);
      return questionsFor(next, answeredSid, 'permission').find((q) => q.status === 'pending');
    },
    { label: 'connected-board permission hold' },
  );
  const beforeAnswer = await Promise.race([
    answeredHold.then(() => 'resolved'),
    delay(150).then(() => 'held'),
  ]);
  assert.equal(beforeAnswer, 'held', 'a connected board keeps the hook parked for its answer');
  await postJson(
    `${daemon.baseUrl}/api/questions/${answerable.id}/answer`,
    { behavior: 'allow' },
    { token: daemon.token },
  );
  assert.deepEqual((await answeredHold).json, {
    hookSpecificOutput: {
      hookEventName: 'PermissionRequest',
      decision: { behavior: 'allow' },
    },
  });

  // Two authorized tabs: closing one cannot steal the remaining tab's live
  // prompt. Closing the last one releases it immediately and does not re-arm.
  const boardB = await connectBoardClient(daemon.baseUrl, daemon.token);
  clients.add(boardB);
  const disconnectSid = randomUUID();
  await register(disconnectSid);
  const disconnectHold = permission(disconnectSid);
  const disconnectQuestion = await waitUntil(
    async () => {
      const next = await getState<QuestionsState>(daemon.baseUrl);
      return questionsFor(next, disconnectSid, 'permission').find((q) => q.held);
    },
    { label: 'two-client permission hold' },
  );

  await closeBoardClient(boardA);
  clients.delete(boardA);
  const afterOneClose = await Promise.race([
    disconnectHold.then(() => 'resolved'),
    delay(200).then(() => 'held'),
  ]);
  assert.equal(afterOneClose, 'held', 'one remaining board client preserves every live hold');

  await closeBoardClient(boardB);
  clients.delete(boardB);
  assert.deepEqual((await disconnectHold).json, {}, 'the last board close fails the hook open');
  const retired = await waitUntil(
    async () => {
      const next = await getState<QuestionsState>(daemon.baseUrl);
      return next.questions.find(
        (q) => q.id === disconnectQuestion.id && q.status === 'expired' && !q.held,
      );
    },
    { label: 'last-board-close retirement' },
  );
  assert.equal(retired.status, 'expired');

  await delay(rearmGraceMs + 250);
  state = await getState<QuestionsState>(daemon.baseUrl);
  assert.equal(
    questionsFor(state, disconnectSid).filter(
      (q) => q.status === 'pending' && q.payload?.rearmed === true,
    ).length,
    0,
    'disconnect fail-open hands ownership to the terminal and suppresses re-arm',
  );
});
