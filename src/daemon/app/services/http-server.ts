import * as Context from 'effect/Context';
import type * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import type { ApplicationQuiescingError } from '../errors.ts';
import type { IngressRunOptions } from './ingress-supervisor.ts';

/**
 * Coarse listener state derived from the frozen Bun lifecycle, not a new state
 * machine. `unbound` before a successful bind, `listening` once bound, and
 * `quiescing` once the frozen shutdown has begun retiring the listener. The
 * authoritative ordering still lives in the P4 lifecycle coordinator driving
 * `http.lifecycle`; this only reports what that owner has already done.
 */
export type HttpServerState = 'unbound' | 'listening' | 'quiescing';

export interface HttpServerAddress {
  readonly hostname: string;
  readonly port: number;
}

/**
 * The root-published surface over the single scoped Bun HTTP listener.
 *
 * The listener's bind/takeover acquisition and its quiesce/stop release are
 * owned by the root Scope (see `http-server-owner.ts` and `live-layer.ts`); this
 * value service only exposes read-only status plus the one blessed bridge a
 * request callback may use to reach the already-built root runtime. `runRequest`
 * forwards to `IngressSupervisor.runPromiseExit`, so no callback ever constructs
 * another runtime, provides a Layer, or calls `Effect.run*With` directly.
 */
export interface HttpServerService {
  readonly state: () => HttpServerState;
  readonly address: () => HttpServerAddress | null;
  readonly runRequest: <A, E>(
    operation: string,
    effect: Effect.Effect<A, E, unknown>,
    options?: IngressRunOptions,
  ) => Promise<Exit.Exit<A, E | ApplicationQuiescingError>>;
}

/** Definition-only value service; the root integration supplies its one live instance. */
export class HttpServer extends Context.Service<HttpServer, HttpServerService>()(
  'fleetdeck/daemon/app/HttpServer',
) {}
