#!/usr/bin/env bun

// P10 SLICE 2 — EFFECT-PATH lifecycle fixture (re-review unblock U1).
//
// A production-faithful sibling of http-lifecycle-fixture.ts: the same real
// createHttp/createCore subprocess, but this one ALSO calls installEffectRoutes,
// so GET /api/watch parks through the Effect held primitive (heldSettleWorkflow +
// settleEffectWatchHold), exactly as the live daemon does (program.ts:884 wires
// installEffectRoutes unconditionally). The legacy fixture leaves effectRoutes
// null and answers through the rollback park; that one stays as the D2 barrier
// oracle for http-lifecycle.test.ts and MUST NOT change. This copy is the Effect
// oracle the slice-2 shutdown-closer (1E-3) and socket-abandon (1E-4) pins need.
//
// runHeld is the REAL production runner — runControlDetached
// (Effect.runPromiseWith(Context.empty())) — NOT the sibling suites'
// runPromiseExit stub, because a parked hold must SURVIVE shutdown on the
// untracked runner so its own closer leg can settle it while the transport can
// still write (D2). It is wrapped in a call counter (runHeldCalls) whose only
// effect is to record invocations: the wrapper delegates to runControlDetached
// unchanged. The counter is what lets the pins PROVE the effect park was taken —
// a parked watch that ran the legacy rollback park would leave runHeldCalls at 0.
//
// effectRoutesInstalled (reported at readiness) and runHeldCalls (reported with
// each `counts`/`closed`) are the two proof signals; ownedCounts keeps the exact
// shape the legacy fixture reports so the shared ZERO_OWNED_COUNTS assertion is
// untouched.

import readline from 'node:readline';
import path from 'node:path';
import * as Effect from 'effect/Effect';
import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createHttp } from '../../src/daemon/http.ts';
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import {
  armUnsupervisedWorkflow,
  controlAsyncWorkflow,
  controlSyncWorkflow,
  mailAckWorkflow,
  mailDrainWorkflow,
  nameControlWorkflow,
  questionsDismissWorkflow,
  spawnRouteWorkflow,
} from '../../src/daemon/app/http-workflows/control.ts';
import {
  healthWorkflow,
  settingsSnapshotWorkflow,
  stateWorkflow,
} from '../../src/daemon/app/http-workflows/health-state.ts';
import { hookDispatchWorkflow } from '../../src/daemon/app/http-workflows/hooks.ts';
import { pasteImageWorkflow } from '../../src/daemon/app/http-workflows/paste.ts';
import {
  cleanupWorkflow,
  commandWorkflow,
  mailWorkflow,
  settingsWorkflow,
} from '../../src/daemon/app/http-workflows/settings-command-mail-cleanup.ts';
import {
  worktreeRemoveWorkflow,
  worktreesSnapshotWorkflow,
} from '../../src/daemon/app/http-workflows/worktrees.ts';
import { repoPreflightWorkflow } from '../../src/daemon/app/http-workflows/repos.ts';
import { heldSettleWorkflow } from '../../src/daemon/app/http-workflows/held.ts';

interface ClosedMessage {
  type: 'closed';
  sharedClosePromise: boolean;
  runHeldCalls: number;
  ownedCounts: ReturnType<ReturnType<typeof createHttp>['lifecycle']['ownedCounts']>;
}

interface CountsMessage {
  type: 'counts';
  runHeldCalls: number;
  ownedCounts: ReturnType<ReturnType<typeof createHttp>['lifecycle']['ownedCounts']>;
}

function emit(
  message:
    | { type: 'ready'; port: number; pid: number; effectRoutesInstalled: boolean }
    | ClosedMessage
    | CountsMessage,
): void {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function listen(handle: ReturnType<typeof createHttp>, port: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    handle.server.once('error', reject);
    handle.server.listen(port, '127.0.0.1', resolve);
  });
}

const [home, portText, token] = process.argv.slice(2);
const port = Number(portText);
if (!home || !token || !Number.isSafeInteger(port) || port < 1 || port > 65_535) {
  throw new Error('usage: http-lifecycle-effect-fixture.ts HOME PORT TOKEN');
}

const db = openDb(path.join(home, 'fleetd.db'));
const core = createCore(db, { port, home, holdMs: 30_000, version: '0.0.0-test' });
const http = createHttp(core, { port, token, version: '0.0.0-test' });

// The one signal that proves the watch route took the Effect park: runHeld is
// only reachable from settleEffectWatchHold (the `if (effectRoutes)` branch of
// watchHook). A legacy-park watch never touches it.
let runHeldCalls = 0;

// FULL installEffectRoutes port — the live daemon's shape (program.ts:884-925).
// runRequest uses the sanctioned in-process discharge (Effect.runPromiseExit,
// mirroring the sibling suites' installSuccess); the watch route never routes
// through runRequest — it uses runHeld — so the SessionStart hook is runRequest's
// only consumer in these pins.
http.installEffectRoutes({
  runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
  health: healthWorkflow,
  state: stateWorkflow,
  settingsSnapshot: settingsSnapshotWorkflow,
  settings: settingsWorkflow,
  command: commandWorkflow,
  mail: mailWorkflow,
  cleanup: cleanupWorkflow,
  pasteImage: pasteImageWorkflow,
  controlAsync: controlAsyncWorkflow,
  controlSync: controlSyncWorkflow,
  questionsDismiss: questionsDismissWorkflow,
  nameControl: nameControlWorkflow,
  armUnsupervised: armUnsupervisedWorkflow,
  mailAck: mailAckWorkflow,
  mailDrain: mailDrainWorkflow,
  spawnRoute: spawnRouteWorkflow,
  hookDispatch: hookDispatchWorkflow,
  worktreesSnapshot: worktreesSnapshotWorkflow,
  repoPreflight: repoPreflightWorkflow,
  worktreeRemove: worktreeRemoveWorkflow,
  // The production-faithful held runner, wrapped ONLY to count invocations. The
  // discharge itself is runControlDetached verbatim — the untracked
  // Effect.runPromiseWith(Context.empty()) that lets a parked hold outlive
  // shutdown so its closer leg settles it (D2).
  runHeld: (effect) => {
    runHeldCalls += 1;
    return runControlDetached(effect);
  },
  watchHold: heldSettleWorkflow,
});

let shutdownPromise: Promise<ClosedMessage> | null = null;

function shutdown(): Promise<ClosedMessage> {
  shutdownPromise ??= (async () => {
    const first = http.lifecycle.close();
    const second = http.lifecycle.close();
    const sharedClosePromise = first === second;
    await first;
    const afterSettlement = http.lifecycle.close();
    await afterSettlement;
    await core.lifecycle.close();
    db.close();
    return {
      type: 'closed',
      sharedClosePromise: sharedClosePromise && afterSettlement === first,
      runHeldCalls,
      ownedCounts: http.lifecycle.ownedCounts(),
    };
  })();
  return shutdownPromise;
}

try {
  await listen(http, port);
  emit({ type: 'ready', port, pid: process.pid, effectRoutesInstalled: true });

  const input = readline.createInterface({ input: process.stdin });
  for await (const line of input) {
    const command = line.trim();
    if (command === 'counts') {
      emit({ type: 'counts', runHeldCalls, ownedCounts: http.lifecycle.ownedCounts() });
    } else if (command === 'close') {
      emit(await shutdown());
    }
  }

  // EOF is also a cleanup command, which keeps failed parent assertions from
  // stranding a listener if the fixture is allowed to exit naturally.
  await shutdown();
} catch (error) {
  try {
    await shutdown();
  } catch {
    /* preserve the original fixture failure */
  }
  const detail = error instanceof Error ? (error.stack ?? error.message) : String(error);
  process.stderr.write(`${detail}\n`);
  process.exitCode = 1;
}
