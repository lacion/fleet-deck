import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { makeProcessRunnerServiceFromDriver } from '../../app/services/process-driver.ts';
import { ProcessRunner } from '../../app/services/process-runner.ts';
import { ProcessRuntimeControl } from '../../app/services/process-runtime-control.ts';
import { makeBunProcessDriver } from './process-driver.ts';

/**
 * Reusable, definition-only native process Layer. Each Layer scope acquires its
 * own driver. ProcessRunner and root control share that exact instance, while
 * its acquireRelease finalizer idempotently joins the lifecycle-owned close.
 */
export const ProcessRunnerLive: Layer.Layer<ProcessRunner | ProcessRuntimeControl> =
  Layer.effectContext(
    Effect.acquireRelease(Effect.sync(makeBunProcessDriver), (driver) =>
      Effect.promise(() => driver.close()),
    ).pipe(
      Effect.map((driver) =>
        Context.make(ProcessRunner, makeProcessRunnerServiceFromDriver(driver)).pipe(
          Context.add(ProcessRuntimeControl, {
            force: () => driver.forceClose(),
            close: () => driver.close(),
          }),
        ),
      ),
    ),
  );
