// store-owner.ts — the imperative owner of fleetd's SQLite handle (P8.3/P8.4).
//
// Sibling to http-server-owner.ts. The owner is published into the root Context
// (its `service`) and driven at two points, exactly like the listener owner:
//   - construction, at db.ts's existing openDb() site in program.ts (boot);
//   - `close`, by the P4 lifecycle coordinator via DaemonResources.setStore.
// `shutdownFallback` is the root-Scope LIFO second pass live-layer.ts registers.
//
// It wraps the existing lifetime, it does not rewrite it. The handle is opened
// by openDb() exactly as before (same PRAGMAs, migrate(), 0600 chmod, -wal/-shm
// sidecars, restart durability) and merely wrapped here. The ONE thing the owner
// adds over the legacy `{ close: () => db.close() }` callback is closing the
// P8.4 statement-closure gap on the release path (see `close`).

import type { SqliteHandle } from '../sqlite.ts';
import type { StoreService, StoreState } from './services/store.ts';

/**
 * The owner surface. `service` is what the root Context publishes; `close` is
 * the coordinator-driven retirement; `shutdownFallback` is the root-Scope LIFO
 * fallback; `state` reports the coarse open/closed lifecycle.
 */
export interface StoreOwner {
  readonly service: StoreService;
  // Coordinator-driven retirement, wired through DaemonResources.setStore and
  // gated by closeOnce's storeSafe. Finalizes every owned/cached prepared
  // statement, then closes the connection IMMEDIATELY (close(true) =
  // sqlite3_close). Memoized: safe to invoke more than once.
  readonly close: () => void;
  // Root-Scope LIFO fallback (see makeStoreOwner for the full mechanism). Only
  // ever COMPLETES a retirement the coordinator already authorized; it never
  // INITIATES one.
  readonly shutdownFallback: () => void;
  readonly state: () => StoreState;
}

export interface StoreOwnerOptions {
  readonly name: string;
  readonly handle: SqliteHandle;
}

/**
 * Wrap an already-open SQLite handle in a root-owned service value.
 *
 * The handle arrives from db.ts's openDb() unchanged; nothing here opens a
 * database, runs a migration, or chmods a file. The owner adds exactly one thing
 * over the legacy `{ close: () => db.close() }` callback: it closes the P8.4
 * statement-closure gap. Legacy close() is bun's sqlite3_close_v2, which can
 * leave the real close DEFERRED behind a still-open prepared statement — trailing
 * past the very finalizer that ran it. `close` here instead finalizes every
 * statement the handle owns and then calls close(true) (sqlite3_close), so the
 * connection's retirement COMPLETES synchronously inside the coordinator's
 * release rather than deferring into GC/exit.
 */
export function makeStoreOwner(options: StoreOwnerOptions): StoreOwner {
  const { handle } = options;
  let retired = false;

  const close = (): void => {
    if (retired) return;
    // Set BEFORE the driver calls: this is a memoized single attempt. A throw
    // from close(true) (an untracked live statement) then leaves the handle for
    // the OS to reclaim at exit rather than looping a failing retirement — and,
    // because `retired` is already true, the fallback below stays a no-op.
    retired = true;
    // P8.4: finalize the owned/cached statements FIRST, then require the
    // immediate close to complete. finalizeStatements() frees every prepared
    // statement the wrapper tracked; close(true) is sqlite3_close, which can no
    // longer be deferred behind — nor throw on — a live statement.
    handle.finalizeStatements();
    handle.close(true);
  };

  const shutdownFallback = (): void => {
    // Root-Scope fallback, run AFTER the coordinator's release by finalizer LIFO
    // (registered during acquire in live-layer.ts, so it is older than the
    // acquireRelease release and closes last). Its mechanism DIFFERS from the
    // HTTP listener's fallback in a way worth stating plainly:
    //
    // The listener's coordinator retires it through a DIVERGENT phased-stop
    // entry (beginGracefulStop/forceStop) and never calls its `lifecycle.close`,
    // so the listener's fallback genuinely STARTS closeHttpOnce as a safe second
    // pass. The store has a SINGLE retirement entry — this owner's `close` — and
    // the coordinator drives exactly that, via DaemonResources.setStore under
    // closeOnce's storeSafe gate. closeOnce runs before this finalizer on every
    // real path (coordinator.close on the success path, acquired.resources.close()
    // on the acquisition-failure path), so there is no divergent work left for
    // this fallback to do:
    //   - storeSafe=true  -> the coordinator already ran `close`; `retired` is
    //                        true, and this is a memoized no-op.
    //   - storeSafe=false -> closeOnce DELIBERATELY left the handle open (a
    //                        rejected upstream close cannot prove its DB-using
    //                        callbacks are gone; the OS closes SQLite at exit).
    //                        `retired` is false, so this stays a no-op and does
    //                        NOT override that gate by force-closing a
    //                        possibly-still-referenced handle.
    //
    // The fallback therefore only ever COMPLETES a retirement the coordinator
    // authorized; it never INITIATES one. `retired` is exactly "did the
    // coordinator's close run?", i.e. the storeSafe decision made visible.
    if (!retired) return;
    close();
  };

  const state = (): StoreState => (retired ? 'closed' : 'open');
  const service: StoreService = { handle, state };

  return { service, close, shutdownFallback, state };
}

/**
 * Truthful unbound owner for root Layer builds that inject no store (the P4
 * acquisition fixtures), mirroring makeUnboundHttpServer. It wraps no handle:
 * `state()` reports `closed` (there is no open connection to retire), and both
 * `close` and `shutdownFallback` are real no-ops, so the frozen finalizer
 * sequence is unperturbed. Reading `service.handle` is a fixture bug, not a
 * supported path — P8.3 publishes the service but has zero consumers of the
 * handle — so it throws rather than hand out a fake connection.
 */
export function makeUnboundStore(): StoreOwner {
  const state = (): StoreState => 'closed';
  const service: StoreService = {
    get handle(): SqliteHandle {
      throw new Error('unbound store owner has no SQLite handle');
    },
    state,
  };
  return {
    service,
    close: () => {},
    shutdownFallback: () => {},
    state,
  };
}
