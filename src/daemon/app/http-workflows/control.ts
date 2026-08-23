// http-workflows/control.ts — the CONTROL route group: the 11 mutating board
// actions (POST /api/spawn/:id/{kill,revive,rc}, /api/sessions/:sid/{adopt,
// dismiss,dismiss/retry,name}, /api/questions/:id/{answer,dismiss},
// /api/plans/:id/{mark,assign}). It follows the CONVENTION established by the
// pilot group (see app/http-workflows/health-state.ts) verbatim; the notes below
// only record the PER-GROUP decisions a MUTATING slice makes differently.
//
// EXPECTED FAILURES ARE DATA, NOT EFFECT ERRORS. Every control route already
// speaks a control-result contract: derive returns a concrete { status, body }
// (or an { ok, ... } object the transport maps to a status) that encodes the
// expected 400/404/409/410 outcomes. That result is the SUCCESS value of the
// workflow — we relay it verbatim. So every workflow here is E = never, exactly
// like the snapshot pilot: we do NOT lift an ok:false into a typed error, we do
// NOT widen the port's error channel, and mapEffectRouteExit needs no new case.
// The uniform success value is ControlWire — the (status, body) pair the
// transport hands straight to json(res, status, body).
//
// QUIESCE POLICY — INVERTED FROM THE SNAPSHOT PILOT (this is the whole point of a
// mutating group). A quiescing ingress resolves runRequest to
// Exit.fail(ApplicationQuiescingError) WITHOUT running the workflow. Because each
// workflow's core call lives INSIDE that (never-run) Effect, the write the
// ingress refused simply never happens — there is no half-applied mutation to
// reconcile. The transport settler (settleEffectControlRoute in http.ts) must
// therefore NOT fall back to the legacy synchronous handler the way a snapshot
// read does: replaying it would perform the very write the ingress just refused.
// Instead it answers the byte-identical refusal the request would receive one
// tick later once the transport's `quiescing` flag flips — 503
// {"ok":false,"reason":"shutting-down"} (http.ts fetchHandler quiescing gate).
// The mapper's interrupt policy is per-group for the same reason: an
// interrupts-only Exit (the shutdown fiber cancelling this in-flight request)
// classifies as quiesce, so a mid-flight mutation takes the same 503 refusal —
// we did not complete it, and we say so.
//
// DEFECT FAITHFULNESS — TWO LEGACY 500 SHAPES, PRESERVED EXACTLY. The legacy
// handlers emit two different 500 bodies, and each workflow reproduces its own:
//   * The six ASYNC routes wrap the core promise in `.then().catch()`. A promise
//     REJECTION logs a route-specific prefix and answers 500
//     {"ok":false,"reason":"internal"} — an EXPECTED-in-practice fault the legacy
//     code handled locally, so we reproduce it as a SUCCESS ControlWire (the
//     onError capability logs; the wire carries the 500 body). A SYNCHRONOUS
//     throw while CONSTRUCTING that promise escapes the .catch and lands in
//     routeRequest's outer catch, which answers 500 {"err":"internal"}. To split
//     the two truthfully the workflow runs the core call under Effect.sync (a
//     synchronous throw there becomes a die → the transport's defect arm →
//     {"err":"internal"}) and only then awaits the promise under Effect.promise
//     (a rejection is caught and mapped to the success wire above).
//   * The five SYNC routes have no local catch, so ANY throw lands in the outer
//     catch → 500 {"err":"internal"}. Running the core call under Effect.sync
//     turns that throw into a die, and the transport's defect arm replays the
//     exact outer-catch bytes.
//
// ROLLBACK SEAM: the legacy synchronous handler for each of the 11 routes stays
// reachable in http.ts. With effectRoutes unset (installEffectRoutes not called
// in program.ts) every route answers through its legacy handler again.
import * as Effect from 'effect/Effect';

/**
 * The uniform success value of every control workflow: the exact (status, body)
 * pair the transport passes to json(res, status, body). `body` is optional and
 * preserved verbatim (an undefined body is relayed as-is, byte-for-byte with the
 * legacy `json(res, out.status, out.body)`).
 */
export interface ControlWire {
  readonly status: number;
  readonly body?: unknown;
}

/**
 * The six ASYNC control routes (kill / revive / rc / adopt / dismiss /
 * dismiss-retry). `run` starts the core control call and returns its promise;
 * `onError` reproduces the legacy route-specific `console.error(...)` a promise
 * rejection logs. R = never, E = never.
 */
export interface ControlAsyncCapabilities {
  readonly run: () => Promise<ControlWire>;
  readonly onError: (err: unknown) => void;
}

/**
 * POST /api/spawn/:id/kill · /revive · /rc, POST /api/sessions/:sid/adopt ·
 * /dismiss · /dismiss/retry. The core call runs under Effect.sync so a
 * synchronous throw becomes a die (→ 500 {"err":"internal"}, the outer catch);
 * its promise is awaited under Effect.promise, and a rejection is relayed as the
 * legacy `.catch` did: onError logs, and the success wire carries 500
 * {"ok":false,"reason":"internal"}.
 */
export const controlAsyncWorkflow = (
  caps: ControlAsyncCapabilities,
): Effect.Effect<ControlWire, never, never> =>
  Effect.sync(() => caps.run()).pipe(
    Effect.flatMap((pending) =>
      Effect.promise(() =>
        pending.then(
          (out): ControlWire => out,
          (err): ControlWire => {
            caps.onError(err);
            return { status: 500, body: { ok: false, reason: 'internal' } };
          },
        ),
      ),
    ),
  );

/**
 * The three SYNC pass-through routes (questions.answer / plans.mark /
 * plans.assign): `run` returns derive's { status, body } control result, relayed
 * verbatim. R = never, E = never; a throw inside `run` becomes a die (→ 500
 * {"err":"internal"}, the outer catch).
 */
export interface ControlSyncCapabilities {
  readonly run: () => ControlWire;
}

/** POST /api/questions/:id/answer, POST /api/plans/:id/mark · /assign. */
export const controlSyncWorkflow = (
  caps: ControlSyncCapabilities,
): Effect.Effect<ControlWire, never, never> => Effect.sync(() => caps.run());

/**
 * POST /api/questions/:id/dismiss. Its core result is an { ok } object the legacy
 * handler maps to json(res, out.ok ? 200 : 404, out). R = never, E = never.
 */
export interface QuestionsDismissCapabilities {
  readonly run: () => { readonly ok: boolean };
}

export const questionsDismissWorkflow = (
  caps: QuestionsDismissCapabilities,
): Effect.Effect<ControlWire, never, never> =>
  Effect.sync(() => {
    const out = caps.run();
    return { status: out.ok ? 200 : 404, body: out };
  });

/**
 * POST /api/sessions/:sid/name. The suffix-validation preceding the core write
 * (byte-exact 400 bodies) lives IN the workflow so it is pinned and isolation-
 * tested; `validateSuffix` (the pure helpers.validateNameSuffix) and `applyName`
 * (core.applyCustomName bound to the session id) arrive as capabilities so the
 * workflow stays pure and R = never. `clearing`/`suffix` are read off the parsed
 * body at the transport before the workflow runs.
 */
export interface NameControlCapabilities {
  readonly clearing: boolean;
  readonly suffix: unknown;
  readonly validateSuffix: (suffix: string) => string | null;
  readonly applyName: (suffix: string | null) => { readonly ok: boolean };
}

export const nameControlWorkflow = (
  caps: NameControlCapabilities,
): Effect.Effect<ControlWire, never, never> =>
  Effect.sync(() => {
    if (!caps.clearing && typeof caps.suffix !== 'string') {
      return {
        status: 400,
        body: { ok: false, reason: 'suffix must be a string (or pass {clear:true})' },
      };
    }
    if (!caps.clearing) {
      // suffix is a string here — the typeof guard above 400s otherwise.
      const bad = caps.validateSuffix(caps.suffix as string);
      if (bad) return { status: 400, body: { ok: false, reason: bad } };
    }
    const out = caps.applyName(caps.clearing ? null : (caps.suffix as string));
    return { status: out.ok ? 200 : 409, body: out };
  });
