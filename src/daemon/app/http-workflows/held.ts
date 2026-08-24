// http-workflows/held.ts — the HELD-RESPONSE settle primitive (P10 Slice 2).
//
// A held long-poll parks an open HTTP response and settles it later when one of
// several racing legs wins. The natural Effect shape is a Deferred the winning
// leg completes; because a Deferred is idempotent, FIRST-SETTLEMENT-WINS falls
// out for free — no `settled` bool guarding the write. heldSettleWorkflow wraps
// exactly that and nothing more:
//
//   make a Deferred<HeldOutcome, never>  ->  arm the legs  ->  await the Deferred
//
// §6-Q1 BINDING: the Deferred wraps SETTLEMENT ONLY. The imperative machinery a
// held surface already owns (its timers, its waiter/closer registries) STAYS
// imperative in the transport — `caps.arm(settle, abandon)` is where the surface
// registers its legs and returns their teardown. This workflow never learns what
// a leg is; it only hands each leg a `settle`/`abandon` it may call at most once.
//
// PARAMETERIZATION (design §2B/§2C): the terminal fold is NOT here. Which value a
// winning leg settles with — GET /api/watch's idle-info body, or (Slice 3) a
// hook's fail-open {} — and how a HeldOutcome renders to the wire both live on
// the TRANSPORT side (the arm closure + the route's settler). That keeps this one
// primitive reusable across the forgiving watch surface (wired this slice) and
// the hook hold surface (Slice 3) without the workflow knowing either fold.
//
// EXECUTION MODEL: this rides the UNTRACKED runControlDetached runner, not the
// supervised ingress bridge — a parked hold must SURVIVE shutdown so its own
// closer leg can settle it while the transport can still write (D2), rather than
// being interrupted. runControlDetached (Effect.runPromiseWith(Context.empty()))
// runs the fiber synchronously until the first async boundary, so `Effect.sync
// (() => caps.arm(...))` executes — arming every leg — BEFORE `Deferred.await`
// suspends. That preserves the legacy synchronous leg-arming: the timer, the
// waiter and the shutdown closer are all registered on the admitting turn.
//
// R = never, E = never (settle/abandon are the only completions, both total), so
// it discharges to a native Promise<HeldOutcome> like every other converted
// route. ROLLBACK SEAM: the legacy imperative park stays reachable in http.ts;
// with effectRoutes unset the watch route answers through it again with no other
// edit.
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';

/**
 * The result of a held response. A winning leg either SETTLES the hold with a
 * body the transport writes, or ABANDONS it (the peer is gone — write nothing).
 * The union is deliberately opaque about `value`: the terminal fold that decides
 * what `value` means for a given surface lives on the transport, not here.
 */
export type HeldOutcome =
  | { readonly _tag: 'settle'; readonly value: unknown }
  | { readonly _tag: 'abandon' };

/**
 * The imperative half §6-Q1 keeps in the transport. `arm` registers the surface's
 * racing legs, wiring each to the `settle`/`abandon` it receives (each completes
 * the Deferred AT MOST once), and returns a teardown thunk run EXACTLY once when
 * the hold resolves — whichever leg won — BEFORE the transport writes.
 */
export interface HeldHoldCapabilities {
  readonly arm: (settle: (value: unknown) => void, abandon: () => void) => () => void;
}

export const heldSettleWorkflow = (
  caps: HeldHoldCapabilities,
): Effect.Effect<HeldOutcome, never, never> =>
  Effect.scoped(
    Effect.gen(function* () {
      const deferred = yield* Deferred.make<HeldOutcome>();
      // settle/abandon complete the Deferred from OUTSIDE the Effect (an
      // imperative leg's callback). doneUnsafe is idempotent, so the second and
      // later legs to fire are silent no-ops — first-settlement-wins.
      const settle = (value: unknown): void => {
        Deferred.doneUnsafe(deferred, Exit.succeed<HeldOutcome>({ _tag: 'settle', value }));
      };
      const abandon = (): void => {
        Deferred.doneUnsafe(deferred, Exit.succeed<HeldOutcome>({ _tag: 'abandon' }));
      };
      // acquireRelease arms the legs synchronously (runControlDetached runs it
      // before the await below suspends) and registers their teardown as a scope
      // finalizer, so teardown runs when the Deferred resolves — after the
      // winning leg fires, BEFORE this workflow's value reaches the settler.
      yield* Effect.acquireRelease(
        Effect.sync(() => caps.arm(settle, abandon)),
        (teardown) => Effect.sync(teardown),
      );
      return yield* Deferred.await(deferred);
    }),
  );
