// http-workflows/health-state.ts — the FIRST route group converted to Effect
// (P6.4 pilot: GET /health and GET /state). Every later route-group worker
// copies the conventions below; the marked PER-GROUP policies are decisions each
// slice makes deliberately.
//
// ============================ CONVENTION ============================
// ONE MODULE PER ROUTE GROUP. A route group is a small set of related routes
// (here the two always-200 snapshot reads). Its module lives under
// src/daemon/app/http-workflows/ and owns three things and nothing else:
//   1. CAPABILITIES-AS-PARAMETERS. A workflow never looks a daemon service up
//      through Context — Core is not an Effect service until P8. Instead each
//      workflow is a plain function that takes a narrow, readonly capability
//      interface (defined in THIS module) and returns an Effect. The capability
//      fields are the only daemon state the workflow may touch: reads are thunks
//      (() => T) the workflow calls inside the Effect; genuine constants (pid,
//      version) are plain values. This keeps R = never — the workflow requires
//      NO Effect environment — so it runs through the P6.3 ingress bridge, which
//      captured the pre-daemon root Context and can provide only
//      AppConfig | ProcessRunner | ProcessRuntimeControl.
//   2. TYPED ERRORS ONLY FOR EXPECTED FAILURES. /health and /state are
//      always-200 snapshot reads with no expected failure, so their workflows
//      are success-only (E = never). A defect stays a defect (it surfaces as the
//      byte-identical 500 the legacy outer catch already emits). A route group
//      that CAN fail in an expected way defines a Data.TaggedError in its module
//      and widens E; the mapper in http-policy.ts then routes that tag.
//   3. THE WIRE PAYLOAD, assembled in the Effect in the exact frozen key order.
//
// HOW A ROUTE RUNS (transport side, in http.ts):
//   build the capability object from the createHttp closure, then
//   `runRequest('METHOD /path', workflow(caps))` — operation names are always
//   'METHOD /path' (e.g. 'GET /health'). The returned Exit is turned into
//   response bytes by mapEffectRouteExit() in http-policy.ts (Success → 200 JSON,
//   ApplicationQuiescingError → legacy fallback, Die → rethrow → byte-identical
//   500). http.ts declares the bridge's shape structurally (HttpEffectRoutes) and
//   receives these builders via installEffectRoutes because the domain zone may
//   not import app/ (tests/import-boundaries.ts); program.ts wires them and tsc
//   checks the capability shapes match at that injection site.
//
// QUIESCE FALLBACK (SNAPSHOT-ROUTE POLICY ONLY): if the ingress runtime is
// quiescing, runRequest resolves to Exit.fail(ApplicationQuiescingError) instead
// of running the workflow. The mapper reports 'quiesce' and the transport falls
// back to the legacy synchronous handler, so a snapshot READ answers 200 during
// shutdown exactly as it did before the conversion. We do NOT invent a new 503
// for these routes.
//   THIS IS A PER-GROUP DECISION, not a universal convention. A MUTATING route
//   group must NOT copy settleEffectSnapshotRoute: in the intra-quiesce window
//   (ingress has already refused the work, but the transport is still admitting
//   the request) the legacy fallback would perform the very write the ingress
//   just refused. A mutating group defines its OWN settler that maps 'quiesce'
//   to an explicit refusal response (e.g. 503), never to a legacy replay.
//   The mapper's interrupt policy (an interrupts-only Exit → 'quiesce', in
//   http-policy.ts) is per-group for the same reason: an always-200 snapshot
//   group reports a mid-flight interruption as quiesce so its contract holds;
//   another group decides what an interrupt means inside its own slice.
//
// ASYNC-MUTATING PATTERN (JOIN-ON-INTERRUPT — a THIRD settler shape). A mutating
// route whose core call is a native Promise (Effect.promise over core.postMail /
// core.cleanup / the async control calls) cannot treat every 'quiesce' Exit as a
// refusal: interrupt() cannot cancel an already-started native Promise, so the
// underlying write WILL commit. Collapsing that to 503 would answer "no write"
// while the DB mutates — a lie the transport must never tell, and the very defect
// this pattern exists to prevent (a settled response whose bytes contradict the
// operation's true result). Such a group's settler (settleEffectAsyncMutatingRoute
// in http.ts) therefore does START-ONCE + JOIN:
//   * START-ONCE: the capability's operation is invoked exactly once through a
//     memoizing recorder (startOnce) whose `invoke` thunk is what the workflow
//     calls; the recorder's `started()` witnesses whether the native Promise is in
//     flight. This is the ONLY way the settler can tell a true admission refusal
//     (workflow never ran → started() === null) from an interrupt-after-start
//     (native Promise captured → started() !== null), because mapEffectRouteExit
//     collapses both to 'quiesce'.
//   * SETTLE: success → emit the workflow's wire result; 'quiesce' with
//     started() === null → 503 {ok:false,reason:'shutting-down'} (the ONLY 503
//     case); 'quiesce' with started() !== null → JOIN the stored Promise and emit
//     the legacy fulfilled/ rejected bytes; a defect with a started Promise joins
//     first, a defect with none rides the group's defect arm. Joining before the
//     response resolves keeps res.done tied to the operation exactly as the legacy
//     .then(json) chain did, so closeClients still waits for the in-flight write.
//
// ROLLBACK SEAM (per route group): the legacy synchronous handler for each route
// stays reachable in http.ts. Removing the `http.installEffectRoutes(...)` call
// in program.ts leaves effectRoutes unset, and every route in the group answers
// through its legacy handler again — no other edit needed. (A FULL P6.3 revert
// additionally unwires the HttpServer owner; see effect-migration-status.md.)
// ===================================================================
import * as Effect from 'effect/Effect';

/**
 * GET /health capabilities. Reads are thunks resolved inside the Effect; pid,
 * version, managed and auth are boot-constant values. The board reads `auth` and
 * `spawn` off /health to shape its UI (see the createHttp header), so their
 * shapes are load-bearing.
 */
export interface HealthCapabilities {
  readonly fleet: () => number;
  readonly pid: number;
  readonly version: string;
  readonly managed: boolean;
  readonly spawn: () => unknown;
  readonly auth: { readonly term_token: boolean };
  readonly startup: () => unknown;
}

/** The exact GET /health body, in frozen key order. */
export interface HealthPayload {
  readonly ok: true;
  readonly fleet: number;
  readonly pid: number;
  readonly version: string;
  readonly managed: boolean;
  readonly spawn: unknown;
  readonly auth: { readonly term_token: boolean };
  readonly startup: unknown;
}

/**
 * GET /health — always-200 snapshot. R = never (no Effect environment), E = never
 * (no expected failure). Key order matches the frozen wire contract exactly:
 * ok, fleet, pid, version, managed, spawn, auth, startup.
 */
export const healthWorkflow = (
  caps: HealthCapabilities,
): Effect.Effect<HealthPayload, never, never> =>
  Effect.sync(() => ({
    ok: true,
    fleet: caps.fleet(),
    pid: caps.pid,
    version: caps.version,
    managed: caps.managed,
    spawn: caps.spawn(),
    auth: caps.auth,
    startup: caps.startup(),
  }));

/**
 * GET /state capabilities. `snapshotWithLan` resolves the token-bearing LAN block
 * and the core snapshot together; the workflow must return its result verbatim so
 * the wire body — including lan.urls/lan.mdns — stays byte-for-byte.
 */
export interface StateCapabilities {
  readonly snapshotWithLan: () => unknown;
}

/**
 * GET /state — always-200 snapshot. R = never, E = never. Returns the exact
 * object the legacy handler passed to json(): { ...core.snapshot(), lan, legacy_upgrade }.
 */
export const stateWorkflow = (caps: StateCapabilities): Effect.Effect<unknown, never, never> =>
  Effect.sync(() => caps.snapshotWithLan());

/**
 * GET /api/settings capabilities (P9.5 Slice 1). `resolve` is a thunk the
 * workflow calls inside the Effect — core.resolveSettings, a SYNC FROZEN LEAF
 * shared three ways (GET /api/settings, the POST /api/settings response body, and
 * the /state broadcast). Its gateway masking (settings.ts resolveGateway:
 * token_set boolean, no raw token key) is a credential-safety invariant the
 * workflow passes through untouched — it never reshapes what resolve() returns.
 */
export interface SettingsSnapshotCapabilities {
  readonly resolve: () => unknown;
}

/**
 * GET /api/settings — always-200 snapshot read, the THIRD route on this model
 * after /health and /state. R = never, E = never: it rides settleEffectSnapshotRoute
 * exactly like /state, so a quiescing ingress replays the legacy 200 read and a
 * defect (unreachable for a synchronous, non-throwing leaf) surfaces as the
 * byte-identical 500 {} the outer catch already emits. The WHOLE body is the
 * success value, in the frozen key order (ok, settings) with resolve()'s masked
 * gateway relayed verbatim.
 */
export const settingsSnapshotWorkflow = (
  caps: SettingsSnapshotCapabilities,
): Effect.Effect<{ readonly ok: true; readonly settings: unknown }, never, never> =>
  Effect.sync(() => ({ ok: true, settings: caps.resolve() }));
