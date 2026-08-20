import * as Data from 'effect/Data';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';

import type { BackgroundController } from './background-owner.ts';
import {
  makeBootReconciliationProgram,
  type BootReconciliationOptions,
} from './boot-reconciliation.ts';
import type { RetentionSchedule } from './retention-schedule.ts';

export class BackgroundProgramUnexpectedExitError extends Data.TaggedError(
  'BackgroundProgramUnexpectedExitError',
)<{
  readonly program: 'agents-poll' | 'lan-refresh' | 'retention';
  readonly message: string;
}> {}

type BootWithoutFirstRetention<Environment> = Omit<
  BootReconciliationOptions<Environment>,
  'firstRetention'
>;

export interface DaemonBackgroundProgramOptions<Environment> {
  readonly agentsPoll: Effect.Effect<never, never, Environment>;
  readonly lanRefresh: Effect.Effect<never, never, Environment>;
  readonly retention: RetentionSchedule<Environment>;
  /** Boot owns the first-retention gate; callers cannot accidentally start a duplicate sweep. */
  readonly boot: BootWithoutFirstRetention<Environment>;
}

function daemonLong<Environment>(
  program: BackgroundProgramUnexpectedExitError['program'],
  effect: Effect.Effect<never, never, Environment>,
): Effect.Effect<never, never, Environment> {
  return effect.pipe(
    Effect.andThen(
      Effect.die(
        new BackgroundProgramUnexpectedExitError({
          program,
          message: `background program ${program} returned unexpectedly`,
        }),
      ),
    ),
  );
}

/**
 * Compose every P5 daemon-long scheduler and the one-shot boot workflow under one parent fiber.
 *
 * Retention waits behind a private gate. The boot workflow opens that gate only after clear-fork
 * healing, then awaits the exact schedule's first run concurrently with spawn reconciliation. This
 * preserves one boot sweep, broadcast-last readiness, and structured interruption of every child.
 */
export function makeDaemonBackgroundProgram<Environment>(
  controller: BackgroundController,
  options: DaemonBackgroundProgramOptions<Environment>,
): Effect.Effect<never, never, Environment> {
  const resolved = {
    agentsPoll: options.agentsPoll,
    lanRefresh: options.lanRefresh,
    retention: options.retention,
    boot: options.boot,
  };

  return Effect.gen(function* () {
    const startRetention = yield* Deferred.make<void>();
    const retentionProgram = Deferred.await(startRetention).pipe(
      Effect.andThen(resolved.retention.program),
    );
    const firstRetention = Deferred.succeed(startRetention, undefined).pipe(
      Effect.andThen(resolved.retention.awaitFirstRun),
    );
    const bootProgram = makeBootReconciliationProgram(controller, {
      ...resolved.boot,
      firstRetention,
    });

    yield* Effect.all(
      [
        daemonLong('agents-poll', resolved.agentsPoll),
        daemonLong('lan-refresh', resolved.lanRefresh),
        daemonLong('retention', retentionProgram),
        bootProgram,
      ],
      { concurrency: 'unbounded', discard: true },
    );
    return yield* Effect.never;
  });
}
