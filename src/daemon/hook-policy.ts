// hook-policy.ts — the pure Exit → hook-response mapper for the POST /hook/:name
// family, and the ONLY per-group mapper that fails OPEN.
//
// The hook fail-open contract (tests/p6-hook-failopen-contract.test.ts) is the
// daemon's single hardest invariant: EVERY /hook/* request answers HTTP 200 with
// the byte-exact body `{}` (plus the frozen header trio) unless a hook handler
// deliberately produced a hook-output payload — never a 401/500/503, never an
// Effect Cause, stack, token, path, or warning, because a hook reply is injected
// straight into a running Claude session. So this mapper is DELIBERATELY NOT
// mapEffectRouteExit (http-policy.ts): that shared classifier splits failures into
// quiesce (→ 503 or legacy replay) and defect (→ rethrow → 500) branches, which is
// exactly what a hook must never do. Here every non-success Exit — an
// ApplicationQuiescingError refusal, an interrupts-only interruption, a die
// (defect), a handler that threw, any unexpected fail — collapses to `{}`.
//
// TOTALITY / UNREPRESENTABLE STATES: Exit.isSuccess partitions every Exit into
// exactly two cases, so the mapping is exhaustive by construction. The result type
// carries ONLY a body and NO status field, so a non-200 hook response is
// structurally unrepresentable — the transport (settleEffectHookRoute) can only
// ever emit `json(res, 200, plan.body)`. The success body is whatever the E=never
// hook workflow assembled (`{}` or a hook-output object, already `?? {}`-shaped);
// every failure body is the canonical `{}`.
//
// A DOMAIN module may import bare effect/* purely as classifiers (see the
// import-boundaries tripwire and http-policy.ts's identical use); this module has
// no I/O, no timers, no core/store access, and no module-level mutable state.
import * as Exit from 'effect/Exit';

/**
 * A hook response PLAN. A body and NOTHING else: there is no status field because
 * a hook answer is always HTTP 200, so a non-200 is unrepresentable.
 */
export interface HookResponse {
  readonly body: unknown;
}

/**
 * Total Exit → hook-response mapper. Success carries the workflow's assembled hook
 * body verbatim; EVERY failure shape (quiesce, interrupt, die, any fail) collapses
 * to the canonical `{}`. This is the `catchAllCause → 200` boundary for hooks,
 * realized on the transport side of the P6.3 bridge rather than inside the
 * E=never workflow (folding a die into `{}` here is equivalent and keeps the
 * workflow catch-free). No Cause detail is ever read, so nothing leaks.
 */
export function mapHookExit(exit: Exit.Exit<unknown, unknown>): HookResponse {
  return Exit.isSuccess(exit) ? { body: exit.value } : { body: {} };
}
