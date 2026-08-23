// http-workflows/hooks.ts — the HOOK route group: POST /hook/:name, the surface
// Claude's hook shims call on every session event. It follows the CONVENTION
// established by the pilot group (see app/http-workflows/health-state.ts)
// verbatim; the notes below record ONLY the per-group decisions the hook slice
// makes differently — and it makes the biggest departure of any group so far.
//
// WHAT THIS WORKFLOW OWNS. Exactly the three decisions the legacy dispatch block
// made after the transport walls (auth / CSRF / host / size / json / token) and
// the special-cased branches (silent refusal, the PermissionRequest /
// Elicitation / AskUserQuestion HOLD relay) had already run in http.ts:
//   1. unknown event name  → ingest telemetry best-effort, respond no-op {}
//   2. malformed payload    → respond no-op {} WITHOUT dispatch (the
//      validateHookEvent gate: a missing/blank session_id would otherwise key a
//      shared phantom card — see the http.ts dispatch comment it replaces)
//   3. a known, valid event → run its handler, respond handler() ?? {}
// The HOLD relay (PermissionRequest / Elicitation / AskUserQuestion) and the
// AskUserQuestion→PermissionRequest pairing stay in http.ts under their P1 owners
// (they park the response; converting them is P10 work), so they never reach this
// workflow. This module makes NO transport, auth, or hold decision.
//
// EVERY HOOK HANDLER IS SYNCHRONOUS. All ten entries in http.ts's hookHandlers
// map (and core.applyEvent) are synchronous core calls, so the whole decision is
// one Effect.sync — there is NO native Promise here and therefore NONE of the
// start-once / join-on-interrupt machinery the async control routes need. An
// interrupt can only land before the sync body runs (→ the settler fails open),
// never mid-write.
//
// FAIL-OPEN IS A PER-GROUP SETTLE POLICY — A FOURTH, UNIQUE SHAPE. The snapshot
// group falls back to the legacy read on quiesce; the mutating groups answer 503
// on a refused write. A hook does NEITHER: the fail-open contract
// (tests/p6-hook-failopen-contract.test.ts) requires that EVERY /hook/* request
// answers HTTP 200 with the byte-exact body `{}` (plus the frozen header trio)
// unless a handler deliberately produced hook output — never 401/500/503, and
// never any Effect Cause, stack, token, or path, because the reply is injected
// into a live Claude session. So the transport settler (settleEffectHookRoute in
// http.ts) uses a DEDICATED mapper — mapHookExit in hook-policy.ts, NOT the shared
// mapEffectRouteExit — that collapses EVERY non-success Exit (quiesce refusal,
// interrupt, die/defect, a handler that threw) to `{ body: {} }`. That is the
// `catchAllCause → 200` boundary, realized on the transport side of the P6.3
// bridge. Keeping it there (rather than a catchAllCause inside this workflow) is
// why the workflow stays E = never with NO catch: a handler that throws becomes a
// die on the Exit, which mapHookExit already folds to `{}` — byte-identical to the
// legacy inner-catch fail-open. Success is the ONLY path that carries a non-empty
// body, and its value is whatever the handler assembled (already `?? {}`-shaped).
//
// ROLLBACK SEAM: as with every group, the legacy synchronous dispatch stays
// reachable in http.ts behind the effectRoutes-unset (null) path; removing the
// installEffectRoutes call in program.ts routes hooks through it again.
import * as Effect from 'effect/Effect';

/**
 * Capabilities for POST /hook/:name. All three fields are thunks the workflow
 * calls INSIDE the Effect (never at build time), so constructing the capability
 * object in http.ts has no side effect — only running the workflow dispatches.
 *
 * - `handler` is the resolved hook handler bound over the already-parsed event
 *   body, or `null` when the event name is unknown. A non-null handler is the
 *   ONLY way a hook produces a non-empty body.
 * - `valid` re-runs the shared validateHookEvent gate at dispatch time (a
 *   malformed payload with no usable session_id → no dispatch).
 * - `ingestUnknown` records best-effort telemetry for an unknown event exactly as
 *   the legacy branch did (core.applyEvent with hook_event_name first); its return
 *   is ignored.
 */
export interface HookDispatchCapabilities {
  readonly handler: (() => unknown) | null;
  readonly valid: () => boolean;
  readonly ingestUnknown: () => void;
}

/**
 * POST /hook/:name — dispatch decision. R = never (no Effect environment),
 * E = never (a hook has no expected FAILURE — an unknown name and a malformed
 * payload are expected OUTCOMES carried as the DATA value `{}`, not errors). The
 * body is the exact value the legacy block passed to json():
 *   unknown name → ingest, then {}
 *   invalid      → {} (no dispatch)
 *   known+valid  → handler() ?? {}
 * A handler that throws is NOT caught here: it becomes a die on the settled Exit,
 * which mapHookExit folds to `{}` — the same fail-open the legacy inner catch
 * produced. Nothing in this workflow can emit a non-200 response.
 */
export const hookDispatchWorkflow = (
  caps: HookDispatchCapabilities,
): Effect.Effect<unknown, never, never> =>
  Effect.sync(() => {
    if (caps.handler === null) {
      caps.ingestUnknown();
      return {};
    }
    if (!caps.valid()) return {};
    return caps.handler() ?? {};
  });
