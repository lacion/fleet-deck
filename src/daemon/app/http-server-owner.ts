import * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import type { HttpBindResult, HttpBound } from '../http.ts';
import type { ApplicationQuiescingError } from './errors.ts';
import type {
  HttpServerAddress,
  HttpServerService,
  HttpServerState,
} from './services/http-server.ts';
import type { RootIngressSupervisorService } from './services/ingress-supervisor.ts';

/**
 * The narrow slice of the frozen `createHttp` return the owner drives.
 *
 * Acquisition reuses the existing `bind`/takeover path verbatim; retirement is
 * the coordinator's frozen phased quiesce/stop ordering, with `lifecycle.close`
 * kept only as the root-Scope LIFO fallback (see `shutdownFallback`). This owner
 * rewrites neither — it only decides *where* those already-frozen operations sit
 * in the root Scope, and exposes a blessed callback bridge for the routes P6.4
 * will convert.
 */
export interface HttpServerTransport {
  readonly bind: (port: number, host: string) => Promise<HttpBindResult>;
  readonly lifecycle: {
    readonly isQuiescing: () => boolean;
    readonly close: () => Promise<void>;
  };
}

/**
 * Imperative owner published into the root Context (its `service`) and driven by
 * boot (`bind`) and the root Scope (`shutdownFallback`).
 *
 * `readyExit` exposes the recorded outcome of the readiness bridge so a focused
 * test can prove the IngressSupervisor edge is installed and exercised without
 * a route conversion.
 */
export interface HttpServerOwner {
  readonly service: HttpServerService;
  readonly bind: (port: number, host: string) => Promise<HttpBindResult>;
  readonly shutdownFallback: () => void;
  readonly readyExit: () => Exit.Exit<void, ApplicationQuiescingError> | null;
}

export interface HttpServerOwnerOptions {
  readonly name: string;
  readonly ingress: RootIngressSupervisorService;
  readonly transport: HttpServerTransport;
}

/**
 * Wrap the frozen Bun listener transport in a root-owned service value.
 *
 * The first successful bind fires exactly one readiness Effect through
 * `IngressSupervisor.runPromiseExit` — the single blessed imperative edge into
 * the already-built root runtime. Nothing here builds another runtime, provides
 * a Layer, or calls `Effect.run*With`; the bridge is the ingress service method.
 */
export function makeHttpServerOwner(options: HttpServerOwnerOptions): HttpServerOwner {
  const { name, ingress, transport } = options;
  let bound: HttpBound | null = null;
  let readySettled = false;
  let recordedReadyExit: Exit.Exit<void, ApplicationQuiescingError> | null = null;

  const runRequest: HttpServerService['runRequest'] = (operation, effect, runOptions) =>
    ingress.runPromiseExit(operation, effect, runOptions);

  const state = (): HttpServerState =>
    transport.lifecycle.isQuiescing() ? 'quiescing' : bound ? 'listening' : 'unbound';

  const address = (): HttpServerAddress | null =>
    bound ? { hostname: bound.hostname, port: bound.port } : null;

  const service: HttpServerService = { state, address, runRequest };

  const bind = async (port: number, host: string): Promise<HttpBindResult> => {
    const result = await transport.bind(port, host);
    if (result._tag === 'Bound') {
      bound = result;
      // Exercise the callback bridge once, on the acquisition edge, so P6.4 route
      // conversions inherit a proven entry path. Idempotent: a repeated bind
      // returns the same frozen Bound value and must not re-run readiness.
      if (!readySettled) {
        readySettled = true;
        recordedReadyExit = await ingress.runPromiseExit(`${name}-ready`, Effect.void);
      }
    }
    return result;
  };

  // Root-Scope fallback only, run AFTER the coordinator's release by finalizer
  // LIFO. On the success path the coordinator retires the listener through the
  // phased beginGracefulStop/forceStop and never calls `lifecycle.close`, so this
  // fallback genuinely starts `closeHttpOnce` — a safe second pass only because
  // the transport's own latches have already run (quiesce latched, holds
  // released, memoized `closeClients`, `bunServer === null` so `forceStop`
  // no-ops). The memoized `closePromise` only makes this a true no-op on the
  // acquisition-failure path, where `resources.close()` already ran `http.close`.
  const shutdownFallback = (): void => {
    void transport.lifecycle.close();
  };

  return { service, bind, shutdownFallback, readyExit: () => recordedReadyExit };
}

/**
 * Truthful unbound owner for root Layer builds that inject no listener (the P4
 * acquisition fixtures). `state()` stays `unbound`, `bind` reports a
 * lifecycle-guard failure, and `shutdownFallback` is a real no-op, so the frozen
 * finalizer sequence and ingress-state assertions are unperturbed.
 */
export function makeUnboundHttpServer(ingress: RootIngressSupervisorService): HttpServerOwner {
  return makeHttpServerOwner({
    name: 'http-server-unbound',
    ingress,
    transport: {
      bind: () =>
        Promise.resolve<HttpBindResult>({
          _tag: 'BindFailed',
          reason: 'other',
          origin: 'lifecycle-guard',
          legacyDelivery: 'error-callback-microtask',
          error: new Error('unbound http server owner cannot bind'),
          code: null,
          errno: null,
          message: 'unbound http server owner cannot bind',
        }),
      lifecycle: { isQuiescing: () => false, close: () => Promise.resolve() },
    },
  });
}
