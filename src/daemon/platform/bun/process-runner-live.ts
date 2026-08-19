import * as Effect from 'effect/Effect';
import type * as Layer from 'effect/Layer';
import { makeProcessRunnerLayerFromDriver } from '../../app/services/process-driver.ts';
import type { ProcessRunner } from '../../app/services/process-runner.ts';
import { makeBunProcessDriver } from './process-driver.ts';

/**
 * Reusable, definition-only native process Layer. Each Layer scope acquires its
 * own driver and its acquireRelease finalizer quiesces and joins every child.
 */
export const ProcessRunnerLive: Layer.Layer<ProcessRunner> = makeProcessRunnerLayerFromDriver(
  Effect.sync(makeBunProcessDriver),
);
