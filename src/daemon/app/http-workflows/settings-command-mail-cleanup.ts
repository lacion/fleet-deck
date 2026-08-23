// http-workflows/settings-command-mail-cleanup.ts — P6.4 SETTINGS/COMMAND/MAIL/
// CLEANUP route group. Copies the conventions in health-state.ts; the marked
// PER-GROUP policies are this slice's decisions.
//
// ============================ CONVENTION ============================
// ONE MODULE PER ROUTE GROUP. A route group is a small set of related routes
// (here the four mutating control POSTs). Its module lives under
// src/daemon/app/http-workflows/ and owns three things and nothing else:
//   1. CAPABILITIES-AS-PARAMETERS. A workflow never looks a daemon service up
//      through Context — Core is not an Effect service until P8. Instead each
//      workflow is a plain function that takes a narrow, readonly capability
//      interface (defined in THIS module) and returns an Effect. The capability
//      fields are the only daemon state the workflow may touch: reads/writes are
//      thunks (() => T) the workflow calls inside the Effect. This keeps
//      R = never — the workflow requires NO Effect environment — so it runs
//      through the P6.3 ingress bridge, which captured the pre-daemon root
//      Context and can provide only AppConfig | ProcessRunner | ProcessRuntimeControl.
//   2. TYPED ERRORS ONLY FOR EXPECTED FAILURES. This group's 400/401/409/415/
//      422/429/503-from-core answers are DATA on a ControlPayload
//      `{status, body}` — they are successful workflow results, not Effect
//      failures. E = never. A defect stays a defect (it surfaces as the
//      byte-identical 500 the legacy .catch / inner catch already emits).
//      The gateway_* bearer gate is NOT a workflow error: it fires at the
//      transport BEFORE the workflow runs (http-policy.gatewaySettingsTouched),
//      exactly as today.
//   3. THE WIRE PAYLOAD, assembled in the Effect. The transport settler writes
//      `json(res, payload.status, payload.body)` — adapters below freeze the
//      legacy status/body mapping (mail `out.status ?? 200, out.body ?? out`;
//      cleanup `!out.ok ? 409 : 200`; settings relays `{status, body}`;
//      command is unconditional 200 of `core.command(text)`).
//
// HOW A ROUTE RUNS (transport side, in http.ts):
//   build the capability object from the createHttp closure (the POST body is
//   closed over in the thunks, not passed as a workflow argument), then
//   `runRequest('METHOD /path', workflow(caps))` — operation names are always
//   'METHOD /path' (e.g. 'POST /api/settings'). The returned Exit is turned
//   into a response PLAN by mapEffectRouteExit() in http-policy.ts (no new
//   E tags — this group does not grow the mapper). http.ts declares the
//   bridge's shape structurally (HttpEffectRoutes) and receives these builders
//   via installEffectRoutes because the domain zone may not import app/;
//   program.ts wires them and tsc checks the capability shapes match at that
//   injection site.
//
// QUIESCE POLICY (MUTATING-ROUTE — DO NOT COPY settleEffectSnapshotRoute):
//   if the ingress runtime is quiescing, runRequest resolves to
//   Exit.fail(ApplicationQuiescingError) WITHOUT running the workflow. The
//   mapper reports 'quiesce'. A snapshot group falls back to the legacy
//   handler so a READ still answers 200; THIS group must NOT. In the
//   intra-quiesce window (ingress has already refused the work, but the
//   transport is still admitting the request) the legacy fallback would
//   perform the very write the ingress just refused. The mutating settler
//   maps 'quiesce' to the frozen fetchHandler shutdown body
//   `503 {"ok":false,"reason":"shutting-down"}` and never replays legacy.
//   In-router json() adds content-length; fetchHandler's `new Response` does
//   not. That delta is the same as every other in-router JSON and is accepted.
//   POST /command is always-200 on SUCCESS but still MUTATES (logCommand,
//   mail, onMutate) — it uses this settler, not the snapshot one.
//   Core-mail's own 503 `{ok:false, reason:'mail lifecycle is quiescing'}` is
//   a successful ControlPayload from postMail, distinct from ingress quiesce.
//
// INTERRUPT POLICY (PER-GROUP): mapEffectRouteExit already classifies an
//   interrupts-only Exit as 'quiesce' (the shutdown fiber cancelling this
//   in-flight request). This group treats that the same as ingress quiesce:
//   503 shutting-down, no write. We do not grow the mapper.
//
// EXCLUSIONS (not converted; stay under their P1 owners until P10):
//   GET /mail — mutating drain+lease, CSRF-walled, broadcasts; not a simple
//     JSON relay (matrix G7).
//   GET /api/watch — async long-poll held-response (`watchHook`, hold_ms
//     0..25000); held responses stay under their P1 owners (matrix G8).
//   GET /api/settings — not in this group's brief (matrix G3).
//
// ROLLBACK SEAM (per route group): the legacy handler for each route stays
// reachable in http.ts. Removing the `http.installEffectRoutes(...)` call
// in program.ts leaves effectRoutes unset, and every route in the group
// answers through its legacy handler again — no other edit needed. (A FULL
// P6.3 revert additionally unwires the HttpServer owner.)
// ===================================================================
import * as Effect from 'effect/Effect';

/** Internal success value the mutating settler unpacks to `json(res, status, body)`. */
export interface ControlPayload {
  readonly status: number;
  readonly body: unknown;
}

/**
 * POST /api/settings capabilities. `setSettings` is a thunk so the Effect is
 * lazy: building it must not write. The gateway_* bearer gate is NOT a
 * capability — it stays at the transport.
 */
export interface SettingsCapabilities {
  readonly setSettings: () => { readonly status: number; readonly body: unknown };
}

/**
 * POST /api/settings — sync core.setSettings relay. R = never, E = never.
 * 400/500-shaped `{status, body}` from setSettings is DATA, not an Effect error.
 */
export const settingsWorkflow = (
  caps: SettingsCapabilities,
): Effect.Effect<ControlPayload, never, never> =>
  Effect.sync(() => {
    const out = caps.setSettings();
    return { status: out.status, body: out.body };
  });

/**
 * POST /command capabilities. `command` is a thunk closed over the POST body's
 * `text` at the transport, matching `core.command((ev as {text?: unknown}).text)`.
 */
export interface CommandCapabilities {
  readonly command: () => unknown;
}

/**
 * POST /command — unconditional 200 of core.command(text) on success. R = never,
 * E = never. Still a MUTATING route (logCommand / mail / onMutate); quiesce
 * must refuse, not replay.
 */
export const commandWorkflow = (
  caps: CommandCapabilities,
): Effect.Effect<ControlPayload, never, never> =>
  Effect.sync(() => ({ status: 200, body: caps.command() }));

/**
 * POST /mail capabilities. `postMail` returns the Promise core.postMail
 * already returns; rejections become defects via Effect.promise (the frozen
 * `.catch` dialect).
 */
export interface MailCapabilities {
  readonly postMail: () => Promise<unknown>;
}

/**
 * Legacy http.ts adapter: postMail returns `{status, body}` on a refusal and
 * the historical bare delivery object on success. `??` matches the handler
 * byte-for-byte (`out.status ?? 200`, `out.body ?? out`).
 */
function adaptMailResult(out: unknown): ControlPayload {
  const rec = out as { status?: unknown; body?: unknown };
  return { status: (rec.status ?? 200) as number, body: rec.body ?? out };
}

/**
 * POST /mail — async core.postMail relay. R = never, E = never. 422/409/429/503
 * (mail-lifecycle) refusals are ControlPayload DATA. A Promise rejection is a
 * defect (legacy `fleetd mail error:` + `500 {ok:false, err:'internal'}`).
 */
export const mailWorkflow = (caps: MailCapabilities): Effect.Effect<ControlPayload, never, never> =>
  Effect.map(
    Effect.promise(() => caps.postMail()),
    adaptMailResult,
  );

/**
 * POST /api/cleanup capabilities. `cleanup` returns the Promise core.cleanup
 * already returns; `ok` is the only field the transport status mapping reads.
 */
export interface CleanupCapabilities {
  readonly cleanup: () => Promise<{ readonly ok: boolean }>;
}

/**
 * POST /api/cleanup — async core.cleanup relay. R = never, E = never.
 * `{ok:true, ...}` → 200; `{ok:false, reason}` → 409 (BUG-145). The body is
 * `out` verbatim. A Promise rejection is a defect (legacy
 * `fleetd cleanup error:` + `500 {ok:false, err:'internal'}`).
 */
export const cleanupWorkflow = (
  caps: CleanupCapabilities,
): Effect.Effect<ControlPayload, never, never> =>
  Effect.map(
    Effect.promise(() => caps.cleanup()),
    (out) => ({
      status: !out.ok ? 409 : 200,
      body: out,
    }),
  );
