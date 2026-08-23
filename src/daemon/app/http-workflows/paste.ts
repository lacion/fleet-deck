// http-workflows/paste.ts — P6.4 route group: POST /api/paste-image.
// Copies the convention in health-state.ts (capabilities-as-params, R=never,
// typed errors only for expected failures, wire payload in frozen key order,
// operation name 'METHOD /path', Exit→Response mapping only in http-policy.ts).
// The PER-GROUP policies below are this slice's deliberate decisions.
//
// ============================ PER-GROUP POLICIES ============================
// MUTATING QUIESCE: do NOT copy settleEffectSnapshotRoute. paste-image writes a
// kept paste (paste.ts pasteImage → FLEETDECK_HOME/pastes). In the intra-quiesce
// window the ingress has already refused the work (ApplicationQuiescingError)
// but the transport still admitted the request (fetchHandler's `quiescing` latch
// is a different, later flag). Falling back to the legacy handler would perform
// the write ingress just refused. Frozen shutdown bytes for a non-hook request
// that loses the admission race are the transport 503
// `{"ok":false,"reason":"shutting-down"}` (http.ts fetchHandler + forceEnd). The
// settler emits that same status+body through json() (the in-router JSON header
// trio: content-type, content-length, nosniff) and never calls pasteImage.
//
// INTERRUPT: mapEffectRouteExit already classifies an interrupts-only Exit as
// 'quiesce'. This group settles that the same way as an explicit quiesce — 503,
// no write — so a mid-flight cancellation is a refusal, not a defect and not a
// legacy replay. A mixed defect+interrupt Cause still falls through to defect.
//
// E = never. Validation / decoded-size / dir / write failures are DATA
// responses today (400/413/500 `{ok:false,reason}` from paste.ts, or 201
// `{ok:true,path,bytes}`). They stay success values of the Effect; the settler
// writes `json(res, value.status, value.body)`. A defect (die / unexpected
// throw) is the POST inner-catch `500 {err:'internal'}` — paste-image lives
// inside the POST `req.on('end')` try, whose catch is `fleetd handler error:` +
// `500 {err:'internal'}`, not the GET outer-catch `500 {}`.
//
// STATIC SHELL INTENTIONALLY LEGACY UNTIL P13. GET `/`, `/index.html`,
// `/assets/*` (and the in-router `/favicon.ico` 204) stay `serveBoardAsset`.
// Anchors: matrix §4 / G9–G10; resolveBoardAssetPath + boardAssetHeaders are
// already pure in http-policy.ts (P6.2); the remaining work is `readFileSync` +
// `res.end(data)`. Wrapping that I/O in an Effect would add a fiber hop and a
// binary-response mapper for no behavioral gain, and would invent an
// intra-quiesce window the current sync path does not have (fetchHandler's
// quiescing latch is the only gate today). P6.4's exit gate is route WORKFLOWS
// for application handlers; static assets are borderline and the freeze bytes
// (MIME/CSP/cache-control/nosniff/referrer-policy/404 `{err:'nope'}`) are best
// preserved by leaving the sync serve in place until P13 removes scaffolding.
// Base64 handling stays in paste.ts; the capability is a thunk over that method.
// ===========================================================================
import * as Effect from 'effect/Effect';

/**
 * POST /api/paste-image capabilities. The thunk closes over the already-parsed
 * JSON body at dispatch time and is the only daemon state the workflow may
 * touch — it is `core.pasteImage` (paste.ts), which owns sniff/base64/dir/write.
 * Resolved inside the Effect so building the workflow is side-effect-free.
 */
export interface PasteImageCapabilities {
  readonly pasteImage: () => PasteImageResult;
}

/**
 * The exact `{status, body}` envelope pasteImage returns, in frozen key order
 * inside `body`: success `ok, path, bytes`; failure `ok, reason`.
 */
export interface PasteImageResult {
  readonly status: number;
  readonly body: unknown;
}

/**
 * POST /api/paste-image — mutating ingest. R = never (no Effect environment),
 * E = never (validation failures are data responses). Returns the paste-store
 * envelope verbatim so the settler can write `json(res, status, body)` with
 * the frozen bytes (201 `.png`/`.jpg`/`.gif`/`.webp` path shape, 400/413/500
 * reasons). Operation name at the bridge: 'POST /api/paste-image'.
 */
export const pasteImageWorkflow = (
  caps: PasteImageCapabilities,
): Effect.Effect<PasteImageResult, never, never> => Effect.sync(() => caps.pasteImage());
