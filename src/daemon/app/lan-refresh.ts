import type * as Duration from 'effect/Duration';
import * as Effect from 'effect/Effect';
import { fixedGridNoCatchUp } from './fixed-grid-schedule.ts';

export interface LanRefreshOptions<ReadError, RefreshError, Environment> {
  readonly enabled?: boolean;
  readonly interval: Duration.Input;
  /** Address discovery failures are intentionally silent, as in the P1 owner. */
  readonly readAddresses: () => Effect.Effect<readonly string[] | null, ReadError, Environment>;
  readonly previousAddresses: () => Effect.Effect<
    readonly string[] | null,
    RefreshError,
    Environment
  >;
  readonly onChange: (
    addresses: readonly string[],
    previous: readonly string[],
  ) => Effect.Effect<void, RefreshError, Environment>;
  /** Best-effort observer: its own failures and defects never load-bear discovery. */
  readonly onError?: (error: RefreshError) => Effect.Effect<void, unknown, Environment>;
}

export function sameLanAddressSet(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && [...a].sort().join(' ') === [...b].sort().join(' ');
}

function reportRefreshError<RefreshError, Environment>(
  error: RefreshError,
  onError: LanRefreshOptions<unknown, RefreshError, Environment>['onError'],
): Effect.Effect<void, never, Environment> {
  return onError ? onError(error).pipe(Effect.ignoreCause) : Effect.void;
}

function refreshOnce<ReadError, RefreshError, Environment>(
  options: LanRefreshOptions<ReadError, RefreshError, Environment>,
): Effect.Effect<void, never, Environment> {
  return Effect.gen(function* () {
    const addresses = yield* options
      .readAddresses()
      .pipe(Effect.catch(() => Effect.succeed<readonly string[] | null>(null)));
    if (addresses === null) return;

    yield* Effect.gen(function* () {
      const previous = yield* options.previousAddresses();
      if (previous === null || sameLanAddressSet(addresses, previous)) return;
      yield* options.onChange([...addresses], [...previous]);
    }).pipe(Effect.catch((error) => reportRefreshError(error, options.onError)));
  });
}

/**
 * Effect-owned LAN refresh loop. Its first read is delayed by one interval,
 * callbacks are strictly single-flight, missed interval ticks are skipped, and
 * interruption joins the active callback before the fiber completes.
 */
export function lanRefresh<ReadError, RefreshError, Environment>(
  options: LanRefreshOptions<ReadError, RefreshError, Environment>,
): Effect.Effect<never, never, Environment> {
  if (options.enabled === false) return Effect.never;

  return Effect.gen(function* () {
    const anchorMs = yield* Effect.clockWith((clock) => clock.currentTimeMillis);
    yield* Effect.sleep(options.interval);
    yield* Effect.repeat(refreshOnce(options), fixedGridNoCatchUp(options.interval, anchorMs));
    return yield* Effect.never;
  });
}
