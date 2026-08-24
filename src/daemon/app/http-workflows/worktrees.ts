// http-workflows/worktrees.ts — the WORKTREES route group: GET /api/worktrees,
// the fleet-wide worktree inspector snapshot (P9.2 Slice 1), and
// POST /api/worktrees/remove, the allow-listed destruction (P9.2 Slice 3).
//
// FAIL-SOFT (DANGER §4.7 — the one read whose EVERY arm renders 200): the core
// dispatcher core.worktrees() is a bounded git fan-out over every remembered
// worktree, and one broken repository must never turn the fleet-wide view into a
// 500 or hide the other worktrees from the human. So a core rejection is folded
// INSIDE this workflow to the soft body { ok: true, worktrees: [] } (byte-identical
// to the legacy handler in http.ts) while onError logs the frozen
// 'fleetd worktree inspector error:' line. The workflow's success value is
// therefore ALWAYS a 200 body — the real snapshot on resolve, the soft wire on
// reject — never a wire carrying a status. This mirrors controlAsyncWorkflow's
// two-phase sync→promise fold (control.ts), but folds to a fail-soft READ body
// instead of a mutating 500 wire.
//
// R = never (no Effect environment) and E = never (the rejection is DATA-folded,
// never an Effect failure), so it rides the P6.3 ingress bridge like every other
// converted route. ROLLBACK SEAM: the legacy synchronous handler stays reachable
// in http.ts; with effectRoutes unset (installEffectRoutes not called in
// program.ts) the route answers through it again with no other edit.
import * as Effect from 'effect/Effect';

/**
 * GET /api/worktrees capabilities. `run` starts the core inspector snapshot and
 * returns its promise; `onError` reproduces the legacy `.catch`'s
 * `console.error('fleetd worktree inspector error:', err)`. R = never, E = never.
 */
export interface WorktreesSnapshotCapabilities {
  readonly run: () => Promise<unknown>;
  readonly onError: (err: unknown) => void;
}

/**
 * GET /api/worktrees — fail-soft read. The core call runs under Effect.sync so a
 * (structurally unreachable, core.worktrees is async) synchronous throw would die
 * to the settler's never-500 defect arm; its promise is awaited under
 * Effect.promise, and a rejection is folded exactly as the legacy `.catch` did:
 * onError logs and the SUCCESS value carries the soft wire { ok: true,
 * worktrees: [] }. On resolve the snapshot is relayed verbatim. NEVER a 500.
 */
export const worktreesSnapshotWorkflow = (
  caps: WorktreesSnapshotCapabilities,
): Effect.Effect<unknown, never, never> =>
  Effect.sync(() => caps.run()).pipe(
    Effect.flatMap((pending) =>
      Effect.promise(() =>
        pending.then(
          (out): unknown => out,
          (err): unknown => {
            caps.onError(err);
            return { ok: true, worktrees: [] };
          },
        ),
      ),
    ),
  );

// ---------------------------------------------------------------------------
// POST /api/worktrees/remove (P9.2 Slice 3)
//
// EXPECTED FAILURES ARE DATA, NOT EFFECT ERRORS (control.ts convention). The core
// removeWorktree call already speaks a { status, body } contract: derive returns a
// concrete wire encoding every expected outcome — the 400 (not a fleet worktree),
// the 409 refusals, the 200 success, AND a RESOLVED purge-path 500
// {ok:false,reason:`could not purge worktree rows: …`} — and that wire is the
// SUCCESS value of the workflow, relayed verbatim (GAP-3b: the purge-500 passes
// through with NO fold and NO log, byte-distinct from the rejection fold). E =
// never: no ok:false is lifted into a typed error.
//
// WHY A DISTINCT BUILDER (not controlAsyncWorkflow, not repoPreflightWorkflow).
// The fold BODY is byte-equal to controlAsyncWorkflow's
// `{ status: 500, body: { ok: false, reason: 'internal' } }` (control.ts:106 ≡
// the return below) — reusing controlAsync would NOT move GAP-3a bytes. What
// must stay distinct is the builder/port (`routes.worktreeRemove`) and the log
// prefix (`fleetd worktree removal error:` vs controlAsync's line vs preflight's
// `Git access check failed internally`). Reusing controlAsyncWorkflow would only
// be wrong if it stole CONTROL_DEFECT's log/`{err:internal}` settler; reusing
// repoPreflightWorkflow WOULD collapse GAP-3a to preflight's distinct 500
// (DANGER §4.6 / slice-0 GAP-3a). onError reproduces the legacy
// 'fleetd worktree removal error:' log.

/**
 * The success value of the remove workflow: the exact (status, body) pair the
 * transport passes to json(res, status, body). `body` is optional and preserved
 * verbatim (byte-for-byte with the legacy `json(res, out.status, out.body)`).
 */
export interface WorktreeRemoveWire {
  readonly status: number;
  readonly body?: unknown;
}

/**
 * POST /api/worktrees/remove capabilities. `run` starts the core removal
 * (core.removeWorktree) and returns its promise; `onError` reproduces the legacy
 * `.catch`'s `console.error('fleetd worktree removal error:', err)`. R = never,
 * E = never.
 */
export interface WorktreeRemoveCapabilities {
  readonly run: () => Promise<WorktreeRemoveWire>;
  readonly onError: (err: unknown) => void;
}

/**
 * POST /api/worktrees/remove. The core call runs under Effect.sync so a
 * (structurally unreachable — removeWorktree is a Promise-returning dispatcher)
 * synchronous throw would die to the settler's defect arm; its promise is awaited
 * under Effect.promise, and a rejection is folded exactly as the legacy `.catch`
 * did: onError logs 'fleetd worktree removal error:' and the SUCCESS value carries
 * the generic 500 wire { ok: false, reason: 'internal' } (GAP-3a). On resolve the
 * removal wire — including a RESOLVED purge-500 — is relayed verbatim (GAP-3b).
 */
export const worktreeRemoveWorkflow = (
  caps: WorktreeRemoveCapabilities,
): Effect.Effect<WorktreeRemoveWire, never, never> =>
  Effect.sync(() => caps.run()).pipe(
    Effect.flatMap((pending) =>
      Effect.promise(() =>
        pending.then(
          (out): WorktreeRemoveWire => out,
          (err): WorktreeRemoveWire => {
            caps.onError(err);
            return { status: 500, body: { ok: false, reason: 'internal' } };
          },
        ),
      ),
    ),
  );
