// Shared lifecycle for integration tests that need one registered Claude
// session. Keep assertions in each test; this helper owns only repetitive,
// failure-prone resource setup and teardown.

import { randomUUID } from 'node:crypto';
import { rmSync } from 'node:fs';

import type { TestContext } from './harness-test.ts';
import { startDaemon, type DaemonHandle, type StartDaemonOptions } from './daemon.ts';
import { loadFixture } from './fixtures.ts';
import { postHook, type JsonResponse } from './http.ts';
import { scratchCwd } from './state.ts';

export interface RegisterTestSessionOptions {
  cwd?: string;
  cwdPrefix?: string;
  sessionId?: string;
  overrides?: Record<string, unknown>;
}

export interface RegisteredTestSession {
  cwd: string;
  sid: string;
  registration: JsonResponse;
}

/**
 * Register one SessionStart against an existing daemon. A generated cwd is
 * removed even when registration throws; caller-owned cwd paths are untouched.
 */
export async function registerTestSession(
  t: TestContext,
  daemon: DaemonHandle,
  {
    cwd: suppliedCwd,
    cwdPrefix,
    sessionId = randomUUID(),
    overrides = {},
  }: RegisterTestSessionOptions = {},
): Promise<RegisteredTestSession> {
  const ownsCwd = suppliedCwd === undefined;
  const cwd = suppliedCwd ?? scratchCwd(cwdPrefix);
  if (ownsCwd) {
    t.after(() => {
      rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
  }

  const registration = await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sessionId, cwd }, overrides),
    { token: daemon },
  );
  return { cwd, sid: sessionId, registration };
}

export interface StartRegisteredDaemonOptions {
  daemon?: StartDaemonOptions;
  session?: RegisterTestSessionOptions;
}

/** Start a daemon, register its teardown immediately, then add one session. */
export async function startRegisteredDaemon(
  t: TestContext,
  { daemon: daemonOptions, session }: StartRegisteredDaemonOptions = {},
): Promise<{ daemon: DaemonHandle } & RegisteredTestSession> {
  const daemon = await startDaemon(daemonOptions);
  t.after(() => daemon.stop());
  return { daemon, ...(await registerTestSession(t, daemon, session)) };
}
