// http-workflows/repos.ts — the REPOS PREFLIGHT route group (P9.2 Slice 2):
// POST /api/repos/preflight, the pre-spawn git-access probe.
//
// EXPECTED FAILURES ARE DATA, NOT EFFECT ERRORS (control.ts convention). The core
// preflight call already speaks a { status, body } contract: derive returns a
// concrete wire encoding every expected outcome — 400 (bad target), 200
// {ok:true,mode:'local'|'clone',…}, and the 409 git_access dialects — and that
// wire is the SUCCESS value of the workflow, relayed verbatim. E = never: no
// ok:false is lifted into a typed error, the port's error channel does not widen,
// and mapEffectRouteExit needs no new case.
//
// WHY NOT controlAsyncWorkflow. This route folds a promise rejection to a 500
// SUCCESS wire exactly as the six async control routes do — but to a DIFFERENT
// body. The legacy preflight route's .catch answers 500
// {ok:false,reason:'Git access check failed internally'} and logs
// 'fleetd repo preflight error:', whereas controlAsyncWorkflow hardcodes
// {ok:false,reason:'internal'}. Reusing it would collapse the two distinct 500
// dialects (DANGER §4.6), so preflight gets its own builder with its own fold
// body. onError reproduces the legacy 'fleetd repo preflight error:' log line.
//
// R = never (no Effect environment) and E = never (the rejection is DATA-folded,
// never an Effect failure), so it rides the P6.3 ingress bridge like every other
// converted route. ROLLBACK SEAM: the legacy async handler stays reachable in
// http.ts; with effectRoutes unset (installEffectRoutes not called in program.ts)
// the route answers through it again with no other edit.
import * as Effect from 'effect/Effect';

/**
 * The success value of the preflight workflow: the exact (status, body) pair the
 * transport passes to json(res, status, body). `body` is optional and preserved
 * verbatim (byte-for-byte with the legacy `json(res, out.status, out.body)`).
 */
export interface RepoPreflightWire {
  readonly status: number;
  readonly body?: unknown;
}

/**
 * POST /api/repos/preflight capabilities. `run` starts the core preflight call
 * (core.preflightRepo) and returns its promise; `onError` reproduces the legacy
 * `.catch`'s `console.error('fleetd repo preflight error:', err)`. R = never,
 * E = never.
 */
export interface RepoPreflightCapabilities {
  readonly run: () => Promise<RepoPreflightWire>;
  readonly onError: (err: unknown) => void;
}

/**
 * POST /api/repos/preflight. The core call runs under Effect.sync so a
 * (structurally unreachable — core.preflightRepo is async) synchronous throw
 * would die to the settler's defect arm; its promise is awaited under
 * Effect.promise, and a rejection is folded exactly as the legacy `.catch` did:
 * onError logs 'fleetd repo preflight error:' and the SUCCESS value carries the
 * 500 wire { ok: false, reason: 'Git access check failed internally' }. On resolve
 * the preflight wire is relayed verbatim.
 */
export const repoPreflightWorkflow = (
  caps: RepoPreflightCapabilities,
): Effect.Effect<RepoPreflightWire, never, never> =>
  Effect.sync(() => caps.run()).pipe(
    Effect.flatMap((pending) =>
      Effect.promise(() =>
        pending.then(
          (out): RepoPreflightWire => out,
          (err): RepoPreflightWire => {
            caps.onError(err);
            return {
              status: 500,
              body: { ok: false, reason: 'Git access check failed internally' },
            };
          },
        ),
      ),
    ),
  );
