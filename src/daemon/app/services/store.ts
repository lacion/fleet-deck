// store.ts — the root-published Store service (P8.3).
//
// This is the SQLite counterpart to services/http-server.ts: a definition-only
// value service the root integration supplies exactly one live instance of. It
// mirrors the P6.3 HttpServer ownership shape — the Layer owns the handle's
// lifetime under the root Scope (store-owner.ts + live-layer.ts), and this
// service is the read surface published into the root Context.

import * as Context from 'effect/Context';
import type { SqliteHandle } from '../../sqlite.ts';

/**
 * Coarse store state, derived from the owner's single retirement rather than a
 * new state machine: `open` until the coordinator (or its root-Scope fallback)
 * retires the handle, `closed` after. Reported, not authoritative — the P4
 * lifecycle coordinator drives the actual finalize-then-close(true) under
 * daemon-resources' storeSafe gate; this only says what that owner has done.
 */
export type StoreState = 'open' | 'closed';

/**
 * The root-published surface over the single SQLite handle fleetd opens.
 *
 * `handle` is the EXISTING synchronous bun:sqlite wrapper (sqlite.ts) verbatim —
 * the same object statements.ts keys its prepared-statement WeakMap on and the
 * ~300 synchronous `q.<stmt>` call sites thread through `ctx`. Publishing it as
 * a value service changes none of those sites; it only gives the later (P8.6)
 * workflow conversions one blessed handle to resolve from the root Context
 * instead of a threaded parameter. The handle's open/close lifetime is owned by
 * the root Scope (see store-owner.ts and live-layer.ts); this service never
 * opens or closes it — the centralized query surface stays synchronous and
 * plain, the Layer owns lifetime, not every SQL call's return type.
 */
export interface StoreService {
  readonly handle: SqliteHandle;
  readonly state: () => StoreState;
}

/** Definition-only value service; the root integration supplies its one live instance. */
export class Store extends Context.Service<Store, StoreService>()('fleetdeck/daemon/app/Store') {}
