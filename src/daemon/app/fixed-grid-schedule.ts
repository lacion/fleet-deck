import * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import * as Schedule from 'effect/Schedule';

function positiveFiniteMilliseconds(interval: Duration.Input): number {
  const milliseconds = Duration.toMillis(Duration.fromInputUnsafe(interval));
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) {
    throw new RangeError('fixed-grid interval must be a positive finite duration');
  }
  return milliseconds;
}

/**
 * Delay from `nowMs` to the next boundary on an interval grid anchored at
 * `anchorMs`. The current boundary is never selected: a callback which runs
 * past one or more ticks skips them and waits for the next future tick, which
 * matches a guarded `setInterval` without replay or immediate catch-up.
 */
export function nextFixedGridDelayMs(anchorMs: number, nowMs: number, intervalMs: number): number {
  if (!Number.isFinite(anchorMs) || !Number.isFinite(nowMs)) {
    throw new RangeError('fixed-grid clock readings must be finite');
  }
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) {
    throw new RangeError('fixed-grid interval must be a positive finite duration');
  }

  const elapsed = Math.max(0, nowMs - anchorMs);
  const completedWindows = Math.floor(elapsed / intervalMs);
  return Math.max(0, anchorMs + (completedWindows + 1) * intervalMs - nowMs);
}

/**
 * Infinite fixed-grid policy without `Schedule.fixed`'s immediate catch-up
 * when work overruns an interval. `anchorMs` must come from Effect's Clock so
 * TestClock and the live runtime observe the same policy.
 */
export function fixedGridNoCatchUp(
  interval: Duration.Input,
  anchorMs: number,
): Schedule.Schedule<number> {
  const intervalMs = positiveFiniteMilliseconds(interval);
  return Schedule.fromStepWithMetadata(
    Effect.succeed((metadata: Schedule.InputMetadata<unknown>) =>
      Effect.succeed([
        metadata.attempt - 1,
        Duration.millis(nextFixedGridDelayMs(anchorMs, metadata.now, intervalMs)),
      ] as const),
    ),
  );
}
