import { writeSync } from 'node:fs';
import * as BunRuntime from '@effect/platform-bun/BunRuntime';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import type * as Runtime from 'effect/Runtime';
import * as Scope from 'effect/Scope';
import {
  LifecycleCoordinator,
  type LifecycleOwner,
} from '../../../src/daemon/app/lifecycle-coordinator.ts';
import { makeIngressSupervisor } from '../../../src/daemon/platform/bun/ingress-supervisor-live.ts';

interface Observation {
  readonly event: string;
  readonly [key: string]: string | number | boolean;
}

function observe(event: string, fields: Record<string, string | number | boolean> = {}): void {
  const observation: Observation = { event, ...fields };
  // The nonzero teardown intentionally calls process.exit through BunRuntime's
  // Node-shared runner. Keep the fixture output deterministic under that exit.
  writeSync(process.stdout.fd, `${JSON.stringify(observation)}\n`);
}

let coordinatedExitCode: 0 | 1 = 1;

const program = Effect.gen(function* () {
  const rootScope = yield* Effect.scope;
  yield* Scope.addFinalizer(
    rootScope,
    Effect.sync(() => observe('root-scope-closed')),
  );

  const ingress = yield* makeIngressSupervisor(Context.empty(), rootScope);
  const stuck = ingress.runPromise(
    'stuck deadline fixture',
    Effect.callback<void>(() => {
      // This ref'ed handle proves natural host idleness cannot mask a stranded
      // ingress fiber. Only the coordinated nonzero teardown may end the child.
      setInterval(() => undefined, 2_147_483_647);
      observe('ingress-started');
      return Effect.never;
    }),
  );
  void stuck.catch(() => undefined);

  let storeCloses = 0;
  const noOp = { run: () => undefined } as const;
  const owner: LifecycleOwner = {
    quiescing: noOp,
    'stopping-producers': noOp,
    withdrawing: noOp,
    'releasing-holds': noOp,
    'closing-clients': { run: () => ingress.close() },
    'closing-http': noOp,
    'closing-store': {
      run: () => {
        storeCloses++;
      },
    },
    'releasing-process': noOp,
  };
  const coordinator = new LifecycleCoordinator(owner, { timeoutMs: 30 });
  const outcome = yield* coordinator.close({ _tag: 'Requested', reason: 'fixture' });
  const clients = outcome.phases.find((phase) => phase.phase === 'closing-clients');
  const store = outcome.phases.find((phase) => phase.phase === 'closing-store');
  coordinatedExitCode = outcome.deadlineExpired ? 1 : 0;
  observe('deadline-outcome', {
    clients: clients?._tag ?? 'missing',
    store: store?._tag ?? 'missing',
    storeCloses,
    active: ingress.activeCount,
  });
}).pipe(Effect.scoped);

const teardown: Runtime.Teardown = <E, A>(
  _exit: Exit.Exit<E, A>,
  onExit: (code: number) => void,
): void => {
  observe('teardown', { code: coordinatedExitCode });
  onExit(coordinatedExitCode);
};

BunRuntime.runMain(program, {
  disableErrorReporting: true,
  teardown,
});
