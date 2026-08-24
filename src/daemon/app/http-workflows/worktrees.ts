// http-workflows/worktrees.ts — the WORKTREES READ route group (P9.2 Slice 1):
// GET /api/worktrees, the fleet-wide worktree inspector snapshot.
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
