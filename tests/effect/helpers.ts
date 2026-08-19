import * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import { TestClock, TestConsole } from 'effect/testing';

export { TestClock, TestConsole };

/** Test services stay opt-in so a test cannot accidentally replace live time or console. */
export const TestServicesLayer = Layer.merge(TestClock.layer(), TestConsole.layer);

/**
 * Runs a fully provided Effect and returns its complete Exit. Typed failures, defects, and
 * interruption remain distinguishable in the Cause; this helper never squashes or throws them.
 */
export function runEffectExit<A, E>(effect: Effect.Effect<A, E>): Promise<Exit.Exit<A, E>> {
  return Effect.runPromiseExit(effect);
}
