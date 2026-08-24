// http.mjs — fleetd HTTP + WebSocket surface.
//
// Hook endpoints answer with hook-output JSON directly; every
// handler fails open — an internal error still returns 200 {} so a hook can
// never break a session. Board/control API: /health /state /mail /command,
// /api/cleanup,
// static board at / + /assets/* (built React app from board-dist), and WS /ws
// (snapshot on connect and on every mutation; a ping/pong keepalive — not a
// periodic snapshot — reaps dead peers).

// Type-only: the audited router body was written against node:http's (req, res)
// objects. Bun.serve replaces node:http as the transport (single-runtime, no `ws`
// dependency), but the ~700-line hostile-boundary router stays byte-for-byte by
// running over the HttpReqShim/HttpResShim adapters below. Only the types survive
// the import; no node:http server is ever constructed.
import type * as http from 'node:http';
import type { Server, ServerWebSocket, WebSocketHandler } from 'bun';
// P6.4: type-only Effect imports. http.ts is the DOMAIN zone — it may reference
// bare `effect/*` at type level (import-boundaries allows it) but must NOT import
// the app-zone workflow module (src/daemon/app/http-workflows). The Effect values
// are injected at runtime via installEffectRoutes (see HttpEffectRoutes below);
// only the shapes are needed here.
import type * as Effect from 'effect/Effect';
import type * as Exit from 'effect/Exit';
import os from 'node:os';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
// 0.7.1: one validator for the custom-name suffix, shared with the `name`
// orchestrator command so the REST route and the text command can never drift.
import { validateNameSuffix } from './helpers.ts';
import { networkInterfaces } from './os-net.ts';
import { spawnFailureReason } from './spawns.ts';
import { createTermBridge } from './termbridge.ts';
// Type-only: `core` is typed as the exact object createCore returns (no runtime
// import, no cycle — derive.ts does not import http). ReturnType<typeof …> gives
// every method's precise signature for free, so the whole `core.*` surface below
// typechecks against derive without a hand-maintained interface.
import type { createCore } from './derive.ts';
// F1a HOSTILE-boundary validators, imported from the shared wire contracts by
// explicit `.ts` specifier (the TS source of truth). Bun strips the types on
// load (dev/test); the esbuild bundle inlines them as plain JS for ship, which
// is what reaches end users. See docs/v1/ts-migration.md.
import { validateHookEvent, validateSpawnRequest } from '../../contracts/index.ts';
// P6.2: pure parsing/security/response-shaping policy split out of the transport
// callback. The dependency points one way (http.ts → http-policy.ts); the router
// below stays byte-for-byte, calling these leaves and owning every side effect.
import {
  asRecord,
  assembleSnapshotFrame,
  authorityTrusted as policyAuthorityTrusted,
  boardAssetHeaders,
  gatewaySettingsTouched,
  hostIsOwn,
  isJsonContentType,
  isLoopbackAddress,
  isPublicShell,
  isUnsupervisedRequest,
  mapEffectRouteExit,
  originTrusted as policyOriginTrusted,
  parseBearer,
  repoPreflightBodyError,
  resolveBoardAssetPath,
  tokenGatedRoute,
  tokenMatches as policyTokenMatches,
  wsBufferEviction,
  wsKeepaliveAction,
} from './http-policy.ts';
import type { TrustedOrigin } from './http-policy.ts';
// P6.4 hook route group: the DEDICATED Exit→hook-response mapper. Deliberately
// NOT mapEffectRouteExit — hooks fail OPEN (every non-success Exit → 200 {}),
// never 503/500/legacy-replay. See hook-policy.ts and settleEffectHookRoute.
import { mapHookExit } from './hook-policy.ts';
// program.ts and the auth/origin suites import these two from the HTTP module's
// public surface; keep re-exporting them now that they live in http-policy.ts.
export { isLoopbackAddress, parseTrustedOrigins } from './http-policy.ts';

const MAX_BODY = 1e6;
// /api/paste-image only: a screenshot is megabytes, and base64-in-JSON (kept —
// the json content-type wall forces a CORS preflight that raw image/png would
// dodge) inflates it another third. paste.mjs caps the DECODED image at 10 MB;
// 10 MB base64 is ~13.4 MB, plus the small JSON/data-URL envelope — 14 MB
// carries it with headroom and nothing more. Every other POST keeps MAX_BODY.
const MAX_PASTE_BODY = 14e6;
// Bun.serve reuses keep-alive sockets and gives us no per-connection close, so an
// early response that abandons the rest of the request body (the oversized-refuse
// path, and any 4xx that replies before 'end') would leave the unread bytes in the
// pipe and desync the NEXT request on that socket. The fetch handler therefore
// DRAINS the body before handing Bun the response — but a client that DECLARES a
// large body then withholds it would park that drain forever, so the wait is capped
// here. A body that is actually present drains in ~ms (measured ~10 ms for 1.2 MB
// over loopback); this grace is ~100x that yet well under the ~4s FIN that
// shouldKeepAlive=false arms, so a refused-then-stalled socket still gets its 413 on
// the wire before it closes. See bun-serve-runtime-limits and the M-B3 body cap.
const BODY_DRAIN_GRACE_MS = 1000;
// A1: when the body-drain grace above expires with the body STILL un-drained
// (a stalled/withheld request body), arm a per-request idle FIN so the
// immortal socket is reaped — idleTimeout:0 otherwise lets it live forever.
// server.timeout is an idle clock that uSockets resets on inbound data, so a
// legitimately flowing upload keeps pushing this bound forward and never trips
// it — the value only has to span the gap between two chunks, not the whole
// transfer. Even read as an ABSOLUTE deadline it still clears the worst single
// upload end-to-end: a 14 MB /api/paste-image over a DERP-relayed Tailscale
// link at ~1 Mbps ≈ 113s, and the FIN (armed ~1s into the request) fires ~121s
// in. clearStalledFin retracts it the instant the body drains, so it only ever
// bounds a body that is genuinely stuck. A 0/negative override would mean
// "never" (server.timeout(_,0) — the opposite of a bound), so those fall back
// to 120s. FLEETDECK_STALL_FIN_S overrides for tests. See boundStalledDrain,
// clearStalledFin, and bun-serve-runtime-limits.
const BODY_STALL_FIN_S = (() => {
  const n = Number(process.env['FLEETDECK_STALL_FIN_S']);
  return Number.isFinite(n) && n > 0 ? n : 120;
})();
// C: the stalled-drain FIN above bounds a request WHILE it is in flight, but
// Bun.serve's idleTimeout:0 leaves the BETWEEN-requests keep-alive-idle phase
// (a completed request, keep-alive response, then a silent socket) immortal —
// no request object exists there for boundStalledDrain to bound, so probing
// idle0 keeps such a socket alive indefinitely. HttpResShim.end arms THIS
// per-request idle FIN just as the response completes, so a client that made
// one request then vanished (a dropped phone, a 401'd keep-alive probe) is
// reaped instead of pinning an fd until restart. Like the stall FIN it is an
// idle clock uSockets resets on inbound data, so a client that keeps issuing
// requests never trips it, and the fetchHandler entry-clear drops it for the
// next in-flight request. 120s mirrors the stall FIN. The 255s clamp on an
// override is defensive only: 255 is the GLOBAL idleTimeout's documented u8 cap,
// NOT proven to bound this per-request lever (probed bun 1.3.14: timeout(_,260)
// did NOT wrap to 4s), so BODY_STALL_FIN_S feeding the same sink unclamped is
// equally moot. A 0/negative override would mean "never" (server.timeout(_,0) —
// the opposite of a bound) and falls back to 120s. FLEETDECK_KEEPALIVE_FIN_S
// overrides for tests. See boundStalledDrain, HttpResShim.end, and
// bun-serve-runtime-limits.
const KEEPALIVE_FIN_S = (() => {
  const n = Number(process.env['FLEETDECK_KEEPALIVE_FIN_S']);
  return Number.isFinite(n) && n > 0 ? Math.min(n, 255) : 120;
})();
// H-R3/R1-2 backpressure: a /ws peer this far behind (dropped wifi, a frozen
// tab) has stopped draining. We do NOT keep buffering snapshots into its dead
// socket — but nor do we merely SKIP the send and clear `dirty`, which stranded
// a recovering client on the one mutation it missed forever (the board halts
// /state polling while its socket is live, so nothing re-delivers it). Instead
// broadcast() TERMINATES the peer past this cap; it reconnects and the connect
// handler hands it a fresh, complete snapshot. FLEETDECK_WS_BUFFER_MAX overrides
// the cap for tests (e.g. -1 forces the eviction path deterministically, since
// bufferedAmount is never negative); unset in production, the 1 MiB default stands.
const MAX_WS_BUFFER = (() => {
  const n = Number(process.env['FLEETDECK_WS_BUFFER_MAX']);
  return Number.isFinite(n) ? n : 1 << 20; // 1 MiB
})();
// H-R3 keepalive cadence: ping every peer and terminate any that missed the
// previous pong. This also RETIRES the old "broadcast a full snapshot every
// 5 s" — a phone that vanished without a FIN never fires 'close', so without a
// real ping/pong its /ws socket leaked and its /ws/term viewer pinned the
// shared tmux client forever.
const WS_PING_MS = 30_000;
// M-P1: coalesce a burst of mutations into ONE snapshot. A single hook can
// drive several updateSession() calls; unbatched, each one rebuilt, stringified
// and broadcast the whole snapshot to every client.
const BROADCAST_COALESCE_MS = 60;
// M-R4/M-P6 terminal-WS bounds. One input frame is a keystroke or a paste,
// never a megabyte; a viewer sitting on this many un-drained output bytes has
// stopped reading and is evicted rather than buffered into oblivion.
const MAX_TERM_FRAME_BYTES = 1 << 20; // 1 MiB
const MAX_TERM_WS_BUFFER = 4 << 20; // 4 MiB

// ---------------------------------------------------------------------------
// node:http shims over Bun.serve
// ---------------------------------------------------------------------------
// The audited router body (routeRequest) and its ~dozen helpers were written
// against node:http's (req, res) objects. Rather than rewrite ~700 lines of
// hostile-boundary logic for a new transport, we feed them these two adapters so
// the body stays byte-for-byte — only the transport underneath changed from
// node:http to Bun.serve. See memory bun-serve-runtime-limits for the one Node
// affordance Bun can't match exactly (per-socket close → ~4s FIN via timeout()).

type ResCloseListener = () => void;
type ReqDataListener = (chunk: Buffer) => void;
type ReqEndListener = () => void;

// A minimal node ServerResponse shim over a single Bun fetch Response. writeHead
// records status+headers; end() builds the Response and resolves `done`, which the
// fetch handler returns. Idempotent: a second writeHead/end (an error thrown after
// a response already went out) is a no-op, matching the router's defensive
// `try { json(res, …) } catch {}` "headers already sent" tolerance.
class HttpResShim {
  readonly done: Promise<Response>;
  private _resolve!: (r: Response) => void;
  private _status = 200;
  private _headers: Record<string, string> = {};
  private _ended = false;
  private _destroyed = false;
  private _closeEmitted = false;
  // A1/C: which per-request idle FIN (if any) is armed. 'refuse' = the ~4s FIN
  // set by shouldKeepAlive on the oversized-refuse path; 'stall' = the
  // BODY_STALL_FIN_S bound armed by boundStalledDrain when the body-drain grace
  // expires un-drained; 'keepalive' = the KEEPALIVE_FIN_S bound armed by end()
  // as the response completes, to reap the between-requests idle socket.
  // boundStalledDrain and end() both refuse to overwrite an already-armed FIN
  // (so grace-expiry can't EXTEND the shorter refuse FIN to 120s, and end()
  // can't lengthen a 'refuse'/'stall' FIN), and clearStalledFin retracts ONLY a
  // 'stall' FIN when the body later drains.
  private _finKind: 'none' | 'refuse' | 'stall' | 'keepalive' = 'none';
  private _closeListeners: ResCloseListener[] = [];
  // Declared as fields + assigned in the body, NOT constructor parameter properties:
  // Bun (like any strip-only type loader) erases types but cannot LOWER a parameter
  // property to a constructor assignment, so these are declared as fields + assigned
  // in the body; `tsconfig`'s `erasableSyntaxOnly` now enforces this at typecheck.
  // See the header note at the top of this file and HttpReqShim below.
  private readonly _request: Request;
  private readonly _server: Server<WsData>;
  constructor(request: Request, server: Server<WsData>) {
    this._request = request;
    this._server = server;
    this.done = new Promise<Response>((resolve) => {
      this._resolve = resolve;
    });
    // Client-disconnect → 'close'. Held responses (hold-hook, watch long-poll) wire
    // their cleanup here; the request signal aborts when the peer drops, never on a
    // normal end — exactly node's res 'close'-on-disconnect semantics those callers
    // rely on (release the question / unregister the waiter).
    this._request.signal.addEventListener(
      'abort',
      () => {
        this._destroyed = true;
        this._emitClose();
      },
      { once: true },
    );
  }
  writeHead(status: number, headers?: http.OutgoingHttpHeaders): this {
    if (this._ended) return this;
    this._status = status;
    if (headers) {
      for (const [k, v] of Object.entries(headers)) {
        if (v == null) continue;
        this._headers[k] = Array.isArray(v) ? v.join(', ') : String(v);
      }
    }
    return this;
  }
  end(body?: string | Buffer): this {
    if (this._ended) return this;
    this._ended = true;
    // C: this response completes the request; the socket now enters the
    // between-requests keep-alive-idle phase, which idleTimeout:0 leaves immortal
    // (boundStalledDrain only bounds an IN-FLIGHT request body, not the gap AFTER
    // one — see bun-serve-runtime-limits). Arm a bounded idle FIN so a client that
    // made one request then went silent (a dropped phone, a 401'd keep-alive
    // probe) is reaped instead of pinning an fd until restart. Guarded by _finKind
    // so a 'refuse' (~4s) or 'stall' FIN already armed for this request is never
    // overwritten/extended; a reused socket's NEXT request clears this and runs
    // unbounded again via the fetchHandler entry-clear. uSockets resets this idle
    // clock on inbound data, so a client that keeps issuing requests never trips it.
    if (this._finKind === 'none') {
      this._finKind = 'keepalive';
      try {
        this._server.timeout(this._request, KEEPALIVE_FIN_S);
      } catch {
        /* server torn down (stop(true)) — benign, same as boundStalledDrain */
      }
    }
    const payload: string | Uint8Array | null =
      body == null ? null : typeof body === 'string' ? body : new Uint8Array(body);
    this._resolve(new Response(payload, { status: this._status, headers: this._headers }));
    return this;
  }
  on(event: 'close', cb: ResCloseListener): this {
    if (event === 'close') this._closeListeners.push(cb);
    return this;
  }
  // P1 lifecycle escape hatch for a request whose body never finishes. Such a
  // request has not reached any application operation yet, so shutdown may
  // cancel its reader and publish the admission-latch response without racing
  // a store user. Completed-body requests are deliberately not forced: their
  // route promise remains owned until it settles.
  forceEnd(status: number, body: string): void {
    if (this._ended) return;
    this._destroyed = true;
    this._emitClose();
    this.writeHead(status, {
      'content-type': 'application/json',
      'x-content-type-options': 'nosniff',
    });
    this.end(body);
  }
  private _emitClose(): void {
    if (this._closeEmitted) return;
    this._closeEmitted = true;
    for (const cb of this._closeListeners) {
      try {
        cb();
      } catch {
        /* listener hygiene only */
      }
    }
  }
  // node's "close this keep-alive socket after the response". Bun has no per-socket
  // close, so force the shortest per-request idle timeout — a ~4s FIN (uSockets 4s
  // granularity, see bun-serve-runtime-limits). The 413 body itself still goes out
  // immediately; only the socket FIN is delayed. Set-only (the router only writes).
  set shouldKeepAlive(keep: boolean) {
    if (!keep) {
      try {
        this._server.timeout(this._request, 1);
        this._finKind = 'refuse';
      } catch {
        /* server torn down */
      }
    }
  }
  // A1: the body-drain grace expired with the body still un-drained (a stalled
  // or withheld request body). Arm a bounded per-request idle FIN so the socket
  // idleTimeout:0 would otherwise keep immortal is reaped. Guarded by _finKind
  // so it never overwrites the shorter ~4s refuse FIN (the oversized-refuse path
  // ALSO stalls its drain — its grace fires before its ~4s FIN — and extending
  // that to 120s would defeat BUG-125's prompt close).
  boundStalledDrain(): void {
    if (this._finKind !== 'none') return; // never overwrite an already-armed FIN
    this._finKind = 'stall';
    try {
      this._server.timeout(this._request, BODY_STALL_FIN_S);
    } catch {
      /* server torn down (stop(true)) — same benign case as shouldKeepAlive */
    }
  }
  // A1 follow-up: the body finished draining AFTER the grace already armed the
  // stalled-drain FIN — a slow-but-real upload, or a held long-poll whose body
  // drained just past the 1s grace (a future remote hook sender, or a ≥1s
  // event-loop stall straddling the drain). Retract the bound so the now-live
  // request runs unbounded again, exactly as if the grace had never fired. Only
  // ever clears a 'stall' FIN; a 'refuse' FIN is left intact so BUG-125's prompt
  // close is never lengthened. This is what makes the held-long-poll exemption
  // true BY CONSTRUCTION rather than merely true-for-loopback-fast-drains.
  clearStalledFin(): void {
    if (this._finKind !== 'stall') return;
    this._finKind = 'none';
    try {
      this._server.timeout(this._request, 0); // back to idleTimeout:0 for this request
    } catch {
      /* server torn down — benign, same as boundStalledDrain */
    }
  }
  get writableEnded(): boolean {
    return this._ended;
  }
  get destroyed(): boolean {
    return this._destroyed;
  }
}

// A minimal node IncomingMessage shim over a Bun Request. `url` is path+search
// (node's req.url shape, NOT Bun's absolute request.url) so the router's
// `new URL(req.url, base)` stays byte-identical; `headers` is the lowercased,
// comma-joined node-style record the walls read; `socket.remoteAddress` is the
// peer IP the loopback/trusted-proxy checks key on.
class HttpReqShim {
  readonly method: string;
  readonly url: string;
  // Typed as node's IncomingHttpHeaders (not Record<string,string>) so the audited
  // router body keeps dot-access on the known keys it reads (req.headers.host /
  // .origin / .authorization); arbitrary keys still take bracket access, exactly as
  // under node:http. Object.fromEntries(headers.entries()) is a {[k]:string}, which
  // is assignable to IncomingHttpHeaders.
  readonly headers: http.IncomingHttpHeaders;
  readonly socket: { remoteAddress: string | undefined };
  private readonly _request: Request;
  private _dataListeners: ReqDataListener[] = [];
  private _endListeners: ReqEndListener[] = [];
  private _destroyed = false;
  private _reader: { cancel: (reason?: unknown) => Promise<void> } | null = null;
  constructor(request: Request, server: Server<WsData>) {
    this._request = request;
    this.method = request.method;
    const u = new URL(request.url);
    this.url = u.pathname + u.search;
    this.headers = Object.fromEntries(request.headers.entries());
    this.socket = { remoteAddress: server.requestIP(request)?.address };
  }
  on(event: 'data', cb: ReqDataListener): this;
  on(event: 'end', cb: ReqEndListener): this;
  on(event: 'data' | 'end', cb: ReqDataListener | ReqEndListener): this {
    if (event === 'data') this._dataListeners.push(cb);
    else this._endListeners.push(cb as ReqEndListener);
    return this;
  }
  destroy(): void {
    this._destroyed = true;
    const reader = this._reader;
    if (reader) void reader.cancel().catch(() => {});
  }
  // Mirrors HttpResShim.destroyed. True once destroy() ran OR _pump's catch
  // fired — a mid-stream body read error, or a 'data'/'end' listener that threw
  // (both run inside _pump's try) — i.e. the request FAULTED rather than draining
  // cleanly. drainThenRespond reads this to decide whether a settled drain earned
  // its stalled-FIN retraction (clean end) or must keep the FIN so the stuck
  // socket is still reaped (fault).
  get destroyed(): boolean {
    return this._destroyed;
  }
  // Driven by the fetch handler AFTER routeRequest has synchronously registered the
  // POST body listeners. Replays the Bun request body stream as node-style
  // 'data'/'end' events. The fetch handler gates the response on this drain via
  // drainThenRespond: Bun reuses keep-alive sockets, so an un-drained body — the
  // oversized-refuse path replies without consuming the rest — would leave the NEXT
  // request's bytes appended to the abandoned stream and desync the peer (it reads a
  // bodyless 400, or nothing). Draining to 'end' keeps the socket in sync; a client
  // that withholds the rest of a declared body parks the drain here, so drainThenRespond
  // caps the wait at BODY_DRAIN_GRACE_MS and shouldKeepAlive=false's ~4s timeout FINs
  // the connection (BUG-125). destroy() remains node's teardown primitive and
  // short-circuits the pump if ever called.
  async _pump(): Promise<void> {
    if (this._destroyed) return;
    const body = this._request.body;
    if (!body) {
      this._emitEnd();
      return;
    }
    const reader = body.getReader();
    this._reader = reader;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (this._destroyed) {
          await reader.cancel().catch(() => {});
          return;
        }
        if (value?.byteLength) {
          const buf = Buffer.from(value); // copy: the router buffers chunks then concats
          for (const cb of this._dataListeners) cb(buf);
        }
        if (this._destroyed) {
          await reader.cancel().catch(() => {});
          return;
        }
      }
      this._emitEnd();
    } catch {
      // A mid-stream read error tears the request down without a spurious 'end'.
      this._destroyed = true;
    } finally {
      if (this._reader === reader) this._reader = null;
      try {
        reader.releaseLock();
      } catch {
        /* reader cancellation may already have released it */
      }
    }
  }
  private _emitEnd(): void {
    if (this._destroyed) return;
    for (const cb of this._endListeners) cb();
  }
}

// The LAN share source handed in by the daemon (a plain object or a thunk that
// re-resolves it per snapshot). All fields optional — currentLan() reads them
// defensively so a half-populated source still renders "local only".
interface LanSource {
  enabled?: boolean;
  urls?: string[];
  mdns?: string | null;
}

// createHttp options — the daemon's exact wiring surface.
interface CreateHttpOptions {
  port: number;
  version?: string;
  capture?: (name: string, ev: unknown) => void;
  token?: string;
  lan?: LanSource | (() => LanSource) | null;
  trustedOrigins?: TrustedOrigin[];
  proxyAuth?: string;
  managed?: boolean;
  requireToken?: boolean;
  trustLoopback?: boolean;
  startup?: { reconciliationStatus?: () => unknown } | null;
}

/**
 * Awaitable bind result for the Effect-owned application root.
 *
 * Bun reports bind failures by throwing synchronously from `Bun.serve`. The
 * legacy node-shaped facade below deliberately replays that same error through
 * an asynchronous `error` callback, but the new root must not have to recover
 * structured startup data from callback timing. Keep the original thrown value
 * and copy its errno fields so startup can map EADDRINUSE to exit 3 without
 * parsing Bun's human-readable message.
 */
export interface HttpBound {
  readonly _tag: 'Bound';
  readonly hostname: string;
  readonly port: number;
}

export interface HttpBindFailed {
  readonly _tag: 'BindFailed';
  readonly reason: 'address-in-use' | 'closed' | 'other';
  readonly origin: 'bun-serve-throw' | 'lifecycle-guard';
  readonly legacyDelivery: 'error-callback-microtask';
  readonly error: unknown;
  readonly code: string | null;
  readonly errno: string | number | null;
  readonly message: string;
}

export type HttpBindResult = HttpBound | HttpBindFailed;

// P6.4 EFFECT-ROUTES PORT. Route groups run as Effect workflows through the
// P6.3 ingress bridge. The workflows and their canonical capability interfaces
// live in the app zone (src/daemon/app/http-workflows/), which this domain
// module may not import — so the shapes below are STRUCTURAL MIRRORS.
// program.ts injects the live builders + runRequest via installEffectRoutes(),
// and tsc checks the mirror against the real interfaces at that injection site
// (a drift there is a compile error, not a silent skew). Until injected,
// effectRoutes stays null and every route answers through its legacy handler —
// that null is the per-route-group rollback seam (remove the
// installEffectRoutes call in program.ts). See health-state.ts for the
// snapshot convention and paste.ts / settings-command-mail-cleanup.ts for the
// mutating convention (do NOT copy settleEffectSnapshotRoute onto a write).
// Every workflow this port carries is E = never: snapshot reads have no
// expected failure, and the mutating groups' 400/409/413/422/429/503-from-core
// answers are DATA (a {status, body} payload), not Effect errors. Pinning
// E = never here (not `unknown`) makes a later route group that widens its
// error channel a COMPILE ERROR at installEffectRoutes in program.ts until this
// port AND mapEffectRouteExit are deliberately grown to route the new tag.
export type HttpWorkflowEffect = Effect.Effect<unknown, never, never>;
// The ONE error the settled Exit can still carry: the ingress bridge resolves a
// quiescing request to Exit.fail(ApplicationQuiescingError) WITHOUT running the
// workflow. This domain module may not import app/errors.ts, so its error is a
// STRUCTURAL MIRROR keyed on the _tag mapEffectRouteExit already detects; the
// covariant Exit error channel makes the concrete bridge return assignable here.
export interface HttpQuiescingFailure {
  readonly _tag: 'ApplicationQuiescingError';
}
export interface HealthRouteCapabilities {
  readonly fleet: () => number;
  readonly pid: number;
  readonly version: string;
  readonly managed: boolean;
  readonly spawn: () => unknown;
  readonly auth: { readonly term_token: boolean };
  readonly startup: () => unknown;
}
export interface StateRouteCapabilities {
  readonly snapshotWithLan: () => unknown;
}
// settings/command/mail/cleanup group (P6.4). Structural mirrors of the
// capability interfaces in http-workflows/settings-command-mail-cleanup.ts.
export interface SettingsRouteCapabilities {
  readonly setSettings: () => { readonly status: number; readonly body: unknown };
}
export interface CommandRouteCapabilities {
  readonly command: () => unknown;
}
export interface MailRouteCapabilities {
  readonly postMail: () => Promise<unknown>;
}
export interface CleanupRouteCapabilities {
  readonly cleanup: () => Promise<{ readonly ok: boolean }>;
}
// paste-image group (P6.4). Structural mirror of PasteImageCapabilities /
// PasteImageResult in http-workflows/paste.ts. The thunk closes over the
// already-parsed JSON body at dispatch; base64/sniff/write stay in paste.ts.
export interface PasteImageRouteResult {
  readonly status: number;
  readonly body: unknown;
}
export interface PasteImageRouteCapabilities {
  readonly pasteImage: () => PasteImageRouteResult;
}
// P6.4 CONTROL ROUTE GROUP (11 mutating POSTs) — STRUCTURAL MIRRORS of the
// capability interfaces in app/http-workflows/control.ts, one per route pattern.
// Every control workflow's success value is the (status, body) pair the transport
// hands to json() — its expected 400/404/409/410 outcomes are DATA carried in
// that body, not Effect errors, so the whole group stays E = never and neither
// this port's error channel nor mapEffectRouteExit grows. tsc checks each mirror
// against the real interface at the program.ts installEffectRoutes() site.
export interface ControlAsyncRouteCapabilities {
  readonly run: () => Promise<{ status: number; body?: unknown }>;
  readonly onError: (err: unknown) => void;
}
export interface ControlSyncRouteCapabilities {
  readonly run: () => { status: number; body?: unknown };
}
export interface QuestionsDismissRouteCapabilities {
  readonly run: () => { readonly ok: boolean };
}
export interface NameControlRouteCapabilities {
  readonly clearing: boolean;
  readonly suffix: unknown;
  readonly validateSuffix: (suffix: string) => string | null;
  readonly applyName: (suffix: string | null) => { readonly ok: boolean };
}
// P9.1 Slice 0 — POST /api/spawn/arm-unsupervised. STRUCTURAL MIRROR of
// ArmUnsupervisedCapabilities in app/http-workflows/control.ts: `run` is the raw
// token mint (core.armUnsupervised), and the workflow assembles the frozen 200
// { ok, arm_token } wire. Sync + E = never like the other pass-through control
// routes; it rides settleEffectMutatingRoute with CONTROL_DEFECT.
export interface ArmUnsupervisedRouteCapabilities {
  readonly run: () => string;
}
// P9.1 Slice 6a — POST /api/spawn. STRUCTURAL MIRROR of SpawnRouteCapabilities in
// app/http-workflows/control.ts: `run` is the raw core.spawn call and its
// { status, body } control result is relayed VERBATIM as the workflow's success
// value (202 provisioning, every early 4xx, the maintenance-gate 503 — all
// success DATA). E stays `never`: a rejection is NOT folded into a wire (unlike
// the six controlAsync routes) — it dies, and settleEffectSpawnRoute renders the
// redacted spawnFailureReason on its defect/joined-rejection arms (D6). tsc checks
// this mirror against the real interface at program.ts's installEffectRoutes site.
export interface SpawnRouteCapabilities {
  readonly run: () => Promise<{ status: number; body?: unknown }>;
}
// P6.4 HOOK ROUTE GROUP (POST /hook/:name) — STRUCTURAL MIRROR of
// HookDispatchCapabilities in app/http-workflows/hooks.ts. All three fields are
// thunks the workflow calls inside the Effect, so building the capability object
// has no side effect. E stays `never`: an unknown name and a malformed payload
// are expected OUTCOMES carried as the DATA value `{}`, not Effect errors. tsc
// checks this mirror against the real interface at program.ts's
// installEffectRoutes() site.
export interface HookDispatchRouteCapabilities {
  readonly handler: (() => unknown) | null;
  readonly valid: () => boolean;
  readonly ingestUnknown: () => void;
}
// P9.2 Slice 1 — GET /api/worktrees (fail-soft READ). STRUCTURAL MIRROR of
// WorktreesSnapshotCapabilities in app/http-workflows/worktrees.ts: `run` starts
// the core inspector snapshot (core.worktrees()) and `onError` reproduces the
// legacy `.catch` inspector log. The workflow folds a core rejection to the soft
// body { ok: true, worktrees: [] } INSIDE itself, so its success value is ALWAYS a
// 200 body and E stays `never`; the never-500 soft-read settler emits no status of
// its own. tsc checks this mirror against the real interface at program.ts's
// installEffectRoutes() site.
export interface WorktreesSnapshotRouteCapabilities {
  readonly run: () => Promise<unknown>;
  readonly onError: (err: unknown) => void;
}
// P9.2 Slice 2 — POST /api/repos/preflight (async pre-spawn probe). STRUCTURAL
// MIRROR of RepoPreflightCapabilities in app/http-workflows/repos.ts: `run` starts
// the core preflight call (core.preflightRepo) and `onError` reproduces the legacy
// `.catch` preflight log. The workflow folds a core rejection to the 500 body
// { ok: false, reason: 'Git access check failed internally' } INSIDE itself — its
// own dialect, NOT controlAsync's {reason:'internal'} — so its success value is
// always a { status, body } wire and E stays `never`. tsc checks this mirror
// against the real interface at program.ts's installEffectRoutes() site.
export interface RepoPreflightRouteCapabilities {
  readonly run: () => Promise<{ status: number; body?: unknown }>;
  readonly onError: (err: unknown) => void;
}
export interface HttpEffectRoutes {
  // runRequest routes the workflow Effect through the ingress bridge and settles
  // to an Exit whose error channel is exactly HttpQuiescingFailure: a quiescing
  // ingress resolves to Exit.fail(ApplicationQuiescingError) WITHOUT running the
  // workflow (mapEffectRouteExit reports 'quiesce'), and the E=never workflow
  // contributes no other failure.
  readonly runRequest: (
    operation: string,
    effect: HttpWorkflowEffect,
  ) => Promise<Exit.Exit<unknown, HttpQuiescingFailure>>;
  readonly health: (caps: HealthRouteCapabilities) => HttpWorkflowEffect;
  readonly state: (caps: StateRouteCapabilities) => HttpWorkflowEffect;
  // settings/command/mail/cleanup group
  readonly settings: (caps: SettingsRouteCapabilities) => HttpWorkflowEffect;
  readonly command: (caps: CommandRouteCapabilities) => HttpWorkflowEffect;
  readonly mail: (caps: MailRouteCapabilities) => HttpWorkflowEffect;
  readonly cleanup: (caps: CleanupRouteCapabilities) => HttpWorkflowEffect;
  // paste-image group
  readonly pasteImage: (caps: PasteImageRouteCapabilities) => HttpWorkflowEffect;
  // P6.4 CONTROL ROUTE GROUP builders (see the mirror interfaces above).
  readonly controlAsync: (caps: ControlAsyncRouteCapabilities) => HttpWorkflowEffect;
  readonly controlSync: (caps: ControlSyncRouteCapabilities) => HttpWorkflowEffect;
  readonly questionsDismiss: (caps: QuestionsDismissRouteCapabilities) => HttpWorkflowEffect;
  readonly nameControl: (caps: NameControlRouteCapabilities) => HttpWorkflowEffect;
  // P9.1 Slice 0 CONTROL ROUTE: POST /api/spawn/arm-unsupervised.
  readonly armUnsupervised: (caps: ArmUnsupervisedRouteCapabilities) => HttpWorkflowEffect;
  // P9.1 Slice 6a CONTROL ROUTE: POST /api/spawn (transport only; core unchanged).
  readonly spawnRoute: (caps: SpawnRouteCapabilities) => HttpWorkflowEffect;
  // P6.4 HOOK ROUTE GROUP builder (see the mirror interface above).
  readonly hookDispatch: (caps: HookDispatchRouteCapabilities) => HttpWorkflowEffect;
  // P9.2 Slice 1 READ ROUTE: GET /api/worktrees (fail-soft; see the mirror above).
  readonly worktreesSnapshot: (caps: WorktreesSnapshotRouteCapabilities) => HttpWorkflowEffect;
  // P9.2 Slice 2 ASYNC ROUTE: POST /api/repos/preflight (see the mirror above).
  readonly repoPreflight: (caps: RepoPreflightRouteCapabilities) => HttpWorkflowEffect;
}

// CONTROL-API SEAM: the board-spawn lifecycle methods (spawn / revive /
// spawnKill / enableRemote) are declared loosely on derive's hand-written ctx
// surface as (...args) => unknown — only adoptSession is spelled out there,
// because events.ts / retention.ts constrain its exact shape. http is the first
// typed consumer that needs their runtime result, so it re-asserts the
// {status, body} control-result contract these all resolve to at this seam.
// See ts-migration-bugs (NOISE): the assertion re-states the runtime shape,
// it does not change it.
type ControlResult = Promise<{ status: number; body?: unknown }>;

// KEEPALIVE SEAM: Bun's ServerWebSocket carries no liveness bit; the heartbeat
// below stamps `isAlive` on each socket (via ws.data) and reaps any that missed
// the previous pong. The two logical WS servers — snapshot (/ws) and terminal
// (/ws/term) — are ONE Bun websocket handler dispatched on `data.kind`, so each
// socket's per-connection state lives here on the discriminated `data` payload.
interface SnapshotSocketData {
  kind: 'snapshot';
  isAlive: boolean;
}
// A terminal socket also carries the query it was opened with (parsed at upgrade,
// before the socket exists), the opened viewer bridge handle, and an abort latch
// the close handler flips mid-open (the M-R5 open/close race).
type TermHandle = Awaited<ReturnType<ReturnType<typeof createTermBridge>['openViewer']>>;
interface TermSocketData {
  kind: 'term';
  isAlive: boolean;
  spawn_id: string;
  cols: number;
  rows: number;
  abort: { closed: boolean };
  handle: TermHandle | null;
}
type WsData = SnapshotSocketData | TermSocketData;
type LiveSocket = ServerWebSocket<WsData>;

// ------------------------------------------------------------ board static
// GET / and /assets/* serve the built React board from board-dist, resolved
// relative to THIS file's directory at runtime — the esbuild bundle keeps
// import.meta.url pointing at scripts/fleetd/, so both the source run
// (fleetd.mjs) and the bundle run (fleetd.bundle.mjs) find the same dist.
const BOARD_DIST = path.join(path.dirname(fileURLToPath(import.meta.url)), 'board-dist');

// Serve one file from board-dist. Path resolution + traversal safety and the
// header policy are pure (http-policy: resolveBoardAssetPath, boardAssetHeaders);
// only the filesystem read and the response write stay here.
function serveBoardAsset(
  res: HttpResShim,
  pathname: string,
  notFound: () => HttpResShim,
): HttpResShim {
  const abs = resolveBoardAssetPath(pathname, BOARD_DIST);
  if (abs === null) return notFound();
  let data;
  try {
    data = fs.readFileSync(abs);
  } catch {
    return notFound();
  }
  const ext = path.extname(abs).toLowerCase();
  res.writeHead(200, boardAssetHeaders(ext, data.length));
  return res.end(data);
}

export function createHttp(
  core: ReturnType<typeof createCore>,
  {
    port,
    version = '0.0.0',
    capture = () => {
      /* no-op unless the daemon wires telemetry */
    },
    token,
    lan = null,
    trustedOrigins = [],
    proxyAuth = 'token',
    managed = false,
    requireToken = false,
    trustLoopback = false,
    startup = null,
  }: CreateHttpOptions,
) {
  // The parsed hook/POST body flows into many typed core.* methods; each cast
  // pulls that method's own param type via Parameters<> rather than re-declaring
  // it here (single source of truth in derive.ts). See asRecord() above for the
  // defensive-read view. `core` never reads `this`, so destructured references
  // are fine.
  type HookBody = Parameters<typeof core.applyEvent>[0];
  // CAPABILITY: may a tokenless caller upgrade /ws/term? The board reads this
  // off /health to diagnose a pre-frame terminal close (see board/src/
  // termDiag.js): a refusal under a mode that WAIVES the key is a transport
  // fault, not a missing credential, and the UI must say so. The decision must
  // mirror authorized() for the one caller the board cannot distinguish — a
  // loopback peer — and authorized() itself cannot answer it: no daemon
  // endpoint can tell whether the BROWSER's upgrade will travel the trusted
  // proxy (waived under PROXY_AUTH=trust) or a direct socket (gated). So this
  // is the union of every tokenless path that exists:
  //   PROXY_AUTH=trust → the proxied browser needs no key;
  //   TRUST_LOOPBACK=on → the plain-loopback power gates are waived;
  //   otherwise the 0.16.0 gate stands: /ws/term demands the bearer on
  //   loopback too (LAN/REQUIRE_TOKEN only ever make it stricter).
  const termAuth = { term_token: !(proxyAuth === 'trust' || trustLoopback) };

  // P1 RESOURCE OWNER: lifecycle state is installed before any callback can be
  // admitted. The returned owner flips this latch first, so every native Bun
  // callback has one cheap, synchronous admission check while close drains the
  // work that was already accepted.
  let quiescing = false;
  interface ActiveResponse {
    request: HttpReqShim;
    response: HttpResShim;
    drain: Promise<void>;
    promise: Promise<Response>;
    hook: boolean;
    drained: boolean;
    drainFaulted: boolean;
  }
  const activeResponses = new Set<ActiveResponse>();
  const activeWatchClosers = new Set<() => void>();
  const openTermTasks = new Set<Promise<void>>();

  function forceFaultedResponseDuringShutdown(active: ActiveResponse): void {
    if (!quiescing || active.response.writableEnded) return;
    // A cleanly drained request may already be running an asynchronous route
    // operation that still owns SQLite/process state, so it must be joined.
    // A body that has not drained, a pump fault, or a disconnected peer cannot
    // produce a response on its own and must be settled explicitly.
    if (
      active.drained &&
      !active.drainFaulted &&
      !active.request.destroyed &&
      !active.response.destroyed
    ) {
      return;
    }
    active.request.destroy();
    active.response.forceEnd(
      active.hook ? 200 : 503,
      active.hook ? '{}' : '{"ok":false,"reason":"shutting-down"}',
    );
  }

  // The board renders its share panel from this: the exact URLs a peer can
  // open, token included (a browser cannot send an Authorization header on its
  // first navigation). Absent/disabled ⇒ the panel says "local only" rather
  // than inventing a URL. Only ever handed to an ALREADY-AUTHORIZED caller —
  // snapshot() is behind the same gate as everything else. `lan` may be a
  // function (a thunk resolved per snapshot so a dead mDNS responder drops its
  // .local URL — BUG-122/051) or a plain object; refreshLan() reassigns it when
  // the host's interfaces change so the panel shows the address the host has NOW
  // without a restart (BUG-118/129). currentLan() therefore resolves it every
  // snapshot rather than freezing a boot-time value.
  function currentLan() {
    const source = typeof lan === 'function' ? lan() : lan;
    return source?.enabled
      ? { enabled: true, urls: source.urls ?? [], mdns: source.mdns ?? null }
      : { enabled: false, urls: [] };
  }

  function snapshotWithLan() {
    return { ...core.snapshot(), lan: currentLan(), legacy_upgrade: legacyBanner() };
  }

  // Returns the response object (Express-style) so `return json(...)` in a
  // void-returning request handler is a real value, not a confusing void
  // expression. Every caller ignores the return — behaviour is unchanged.
  function json(res: HttpResShim, code: number, obj: unknown): HttpResShim {
    const body = JSON.stringify(obj);
    // nosniff on every JSON response too: the one central place that emits our
    // API + hook bodies, so no route can forget it (matches serveBoardAsset).
    res.writeHead(code, {
      'content-type': 'application/json',
      'content-length': Buffer.byteLength(body),
      'x-content-type-options': 'nosniff',
    });
    return res.end(body);
  }

  function settleFilesystemOperation(
    res: HttpResShim,
    scope: 'session' | 'home',
    operation: Promise<{ status: number; body: unknown }>,
  ): void {
    operation
      .then(({ status, body }) => {
        json(res, status, body);
      })
      .catch((err: unknown) => {
        console.error(`fleetd ${scope} filesystem error:`, err);
        json(res, 500, { ok: false, reason: 'internal' });
      });
  }

  // ------------------------------------------------------------ P6.4 effect routes
  // GET /health, GET /state, and POST /api/paste-image run as Effect workflows
  // through the P6.3 ingress bridge, injected post-construction via
  // installEffectRoutes() — the domain zone cannot import the app-zone workflow
  // modules (tests/import-boundaries.ts), so program.ts hands the live builders
  // + runRequest in here.
  //
  // ROUTE-GROUP ROLLBACK: while effectRoutes is null — i.e. program.ts never
  // called installEffectRoutes (or its call is removed) — every wired route
  // answers through its legacy synchronous handler below, byte-for-byte as
  // before the conversion. Snapshot routes also use those handlers as the
  // quiesce fallback; paste-image does NOT (mutating: intra-quiesce is a 503
  // refusal). This null is the documented per-route-group rollback seam; a FULL
  // P6.3 revert additionally unwires the HttpServer owner (see
  // docs/v1/effect-migration-status.md).
  let effectRoutes: HttpEffectRoutes | null = null;

  // The pre-P6.4 GET /health body, verbatim — the rollback + quiesce path and
  // what the focused freeze tests pin. Key order is the frozen wire contract:
  // ok, fleet, pid, version, managed, spawn, auth, startup.
  function legacyHealthResponse(res: HttpResShim): void {
    json(res, 200, {
      ok: true,
      fleet: core.fleetSize(),
      pid: process.pid,
      version,
      managed,
      spawn: core.spawnCapability(),
      auth: termAuth,
      startup: startup?.reconciliationStatus?.() ?? null,
    });
  }

  // The pre-P6.4 GET /state body, verbatim (core.snapshot() + lan + banner).
  function legacyStateResponse(res: HttpResShim): void {
    json(res, 200, snapshotWithLan());
  }

  // Capability objects handed to the workflows. Reads are thunks the workflow
  // resolves inside its Effect; pid/version/managed/auth are boot constants.
  // Shapes mirror HealthCapabilities/StateCapabilities in health-state.ts; tsc
  // checks the mirror at the program.ts injection site.
  function healthCapabilities(): HealthRouteCapabilities {
    return {
      fleet: () => core.fleetSize(),
      pid: process.pid,
      version,
      managed,
      spawn: () => core.spawnCapability(),
      auth: termAuth,
      startup: () => startup?.reconciliationStatus?.() ?? null,
    };
  }

  function stateCapabilities(): StateRouteCapabilities {
    return { snapshotWithLan: () => snapshotWithLan() };
  }

  function settingsCapabilities(ev: unknown): SettingsRouteCapabilities {
    return { setSettings: () => core.setSettings(ev) };
  }

  function commandCapabilities(ev: unknown): CommandRouteCapabilities {
    return { command: () => core.command((ev as { text?: unknown }).text) };
  }

  function mailCapabilities(ev: unknown): MailRouteCapabilities {
    return { postMail: () => core.postMail(ev as Parameters<typeof core.postMail>[0]) };
  }

  function cleanupCapabilities(): CleanupRouteCapabilities {
    return { cleanup: () => core.cleanup() };
  }

  // Run a workflow Effect through the ingress bridge and settle it to the exact
  // legacy response bytes. Mirrors the async-dispatch idiom of /api/worktrees:
  // routeRequest stays synchronous and returns at once; the response is written
  // when the Exit resolves a microtask later (fetchHandler's drainThenRespond
  // awaits res.done). Exit → Response via mapEffectRouteExit():
  //   success → json(res, 200, value) — the workflow assembled the frozen body;
  //   quiesce → legacy(res) — the ingress refused (ApplicationQuiescingError);
  //             a snapshot read still answers 200 exactly as before shutdown;
  //   defect  → reproduce routeRequest's outer catch byte-for-byte: log
  //             'fleetd request error:' and answer 500 {} (a non-hook route).
  function settleEffectSnapshotRoute(
    routes: HttpEffectRoutes,
    operation: string,
    effect: HttpWorkflowEffect,
    res: HttpResShim,
    legacy: (res: HttpResShim) => void,
  ): void {
    routes
      .runRequest(operation, effect)
      .then((exit) => {
        const outcome = mapEffectRouteExit(exit);
        if (outcome.kind === 'success') {
          json(res, 200, outcome.value);
          return;
        }
        if (outcome.kind === 'quiesce') {
          legacy(res);
          return;
        }
        // A defect is the byte-identical 500 the outer catch emits; rethrow so a
        // single sink (the .catch below) writes it.
        throw outcome.defect;
      })
      .catch((err: unknown) => {
        // Mirrors routeRequest's outer catch for a non-hook path exactly:
        // /health and /state are never /hook/ routes, so 500 {} — never the
        // fail-open 200 branch.
        console.error('fleetd request error:', err);
        try {
          json(res, 500, {});
        } catch {
          /* socket gone */
        }
      });
  }

  // Fail-soft READ settler (GET /api/worktrees, DANGER §4.7): the ONE read whose
  // EVERY Exit arm renders 200. Unlike settleEffectSnapshotRoute, the defect arm
  // does NOT reproduce routeRequest's 500 — a broken inspector must never surface
  // as an error status. The workflow already folded a core rejection to the soft
  // body { ok: true, worktrees: [] } (its onError logged the inspector line), so:
  //   success → json(res, 200, value) — the real snapshot OR the folded soft body;
  //   quiesce → legacy(res) — a snapshot read answers 200 as before shutdown;
  //   defect  → the (structurally unreachable, core.worktrees is async) sync-throw
  //             path STILL answers the 200 soft body, never 500; the defect is
  //             logged with the frozen inspector line so a real regression is not
  //             swallowed silently.
  function settleEffectSoftReadRoute(
    routes: HttpEffectRoutes,
    operation: string,
    effect: HttpWorkflowEffect,
    res: HttpResShim,
    legacy: (res: HttpResShim) => void,
  ): void {
    routes
      .runRequest(operation, effect)
      .then((exit) => {
        const outcome = mapEffectRouteExit(exit);
        if (outcome.kind === 'success') {
          json(res, 200, outcome.value);
          return;
        }
        if (outcome.kind === 'quiesce') {
          legacy(res);
          return;
        }
        // Never-500: an (unreachable) defect still renders the fail-soft wire.
        console.error('fleetd worktree inspector error:', outcome.defect);
        json(res, 200, { ok: true, worktrees: [] });
      })
      .catch(() => {
        // The only way here is json() throwing on a dead socket (the defect arm
        // above is handled inline, never rethrown). Nothing left to write.
      });
  }

  // GET /health dispatch: legacy when the bridge is unwired, else the workflow.
  function dispatchHealth(res: HttpResShim): void {
    if (!effectRoutes) {
      legacyHealthResponse(res);
      return;
    }
    settleEffectSnapshotRoute(
      effectRoutes,
      'GET /health',
      effectRoutes.health(healthCapabilities()),
      res,
      legacyHealthResponse,
    );
  }

  // GET /state dispatch: legacy when the bridge is unwired, else the workflow.
  function dispatchState(res: HttpResShim): void {
    if (!effectRoutes) {
      legacyStateResponse(res);
      return;
    }
    settleEffectSnapshotRoute(
      effectRoutes,
      'GET /state',
      effectRoutes.state(stateCapabilities()),
      res,
      legacyStateResponse,
    );
  }

  // GET /api/worktrees legacy handler, verbatim — the rollback path (effectRoutes
  // unwired) and the fail-soft settler's quiesce replay. Fail-SOFT: a rejection
  // folds to 200 { ok: true, worktrees: [] } + the inspector log; NEVER a 500.
  function legacyWorktreesResponse(res: HttpResShim): void {
    core
      .worktrees()
      .then((out) => {
        json(res, 200, out);
      })
      .catch((err: unknown) => {
        console.error('fleetd worktree inspector error:', err);
        json(res, 200, { ok: true, worktrees: [] });
      });
  }

  function worktreesSnapshotCapabilities(): WorktreesSnapshotRouteCapabilities {
    return {
      run: () => core.worktrees(),
      onError: (err) => {
        console.error('fleetd worktree inspector error:', err);
      },
    };
  }

  // GET /api/worktrees dispatch: legacy when the bridge is unwired, else the
  // fail-soft workflow settled through the never-500 soft-read settler.
  function dispatchWorktrees(res: HttpResShim): void {
    if (!effectRoutes) {
      legacyWorktreesResponse(res);
      return;
    }
    settleEffectSoftReadRoute(
      effectRoutes,
      'GET /api/worktrees',
      effectRoutes.worktreesSnapshot(worktreesSnapshotCapabilities()),
      res,
      legacyWorktreesResponse,
    );
  }

  // The pre-P6.4 POST /api/paste-image handler, verbatim — rollback path only.
  // MUTATING: the quiesce settler must NEVER call this (it would write).
  function legacyPasteImageResponse(res: HttpResShim, ev: unknown): void {
    const out = core.pasteImage(ev as Parameters<typeof core.pasteImage>[0]);
    json(res, out.status, out.body);
  }

  function pasteImageCapabilities(ev: unknown): PasteImageRouteCapabilities {
    return {
      pasteImage: () => core.pasteImage(ev as Parameters<typeof core.pasteImage>[0]),
    };
  }

  // Frozen non-hook shutdown body (fetchHandler + forceEnd). Emitted through
  // json() so the in-router JSON header trio (content-type, content-length,
  // nosniff) applies; status+body bytes match the transport 503.
  const PASTE_IMAGE_QUIESCE_BODY = { ok: false, reason: 'shutting-down' } as const;

  // MUTATING settler: success writes the paste envelope's own status (201/400/
  // 413/500 data responses); quiesce/interrupt → frozen 503, no legacy replay;
  // defect → POST inner-catch bytes (`fleetd handler error:` + 500 {err:'internal'}).
  function settleEffectPasteImageRoute(
    routes: HttpEffectRoutes,
    operation: string,
    effect: HttpWorkflowEffect,
    res: HttpResShim,
  ): void {
    routes
      .runRequest(operation, effect)
      .then((exit) => {
        const outcome = mapEffectRouteExit(exit);
        if (outcome.kind === 'success') {
          const value = outcome.value as PasteImageRouteResult;
          json(res, value.status, value.body);
          return;
        }
        if (outcome.kind === 'quiesce') {
          json(res, 503, PASTE_IMAGE_QUIESCE_BODY);
          return;
        }
        throw outcome.defect;
      })
      .catch((err: unknown) => {
        console.error('fleetd handler error:', err);
        try {
          json(res, 500, { err: 'internal' });
        } catch {
          /* socket gone */
        }
      });
  }

  // POST /api/paste-image dispatch: legacy when the bridge is unwired, else the
  // workflow. Quiesce is a 503 refusal, not a legacy fallback.
  function dispatchPasteImage(res: HttpResShim, ev: unknown): void {
    if (!effectRoutes) {
      legacyPasteImageResponse(res, ev);
      return;
    }
    settleEffectPasteImageRoute(
      effectRoutes,
      'POST /api/paste-image',
      effectRoutes.pasteImage(pasteImageCapabilities(ev)),
      res,
    );
  }

  // MUTATING-ROUTE SETTLER (settings/command/mail/cleanup quiesce policy).
  // Unlike settleEffectSnapshotRoute, 'quiesce' MUST NOT replay the legacy
  // handler: in the intra-quiesce window the ingress has already refused the
  // write, but the transport is still admitting the request. Replaying would
  // perform the write. Map 'quiesce' (ApplicationQuiescingError OR an
  // interrupts-only Exit — mapEffectRouteExit already classifies both as
  // 'quiesce') to the frozen fetchHandler shutdown body. In-router json()
  // adds content-length; fetchHandler's new Response does not. That delta is
  // the same as every other in-router JSON and is accepted.
  // Defect bytes are PER ROUTE (the frozen .catch / inner-catch dialect).
  function settleEffectMutatingRoute(
    routes: HttpEffectRoutes,
    operation: string,
    effect: HttpWorkflowEffect,
    res: HttpResShim,
    defect: { readonly log: string; readonly body: unknown },
  ): void {
    routes
      .runRequest(operation, effect)
      .then((exit) => {
        const outcome = mapEffectRouteExit(exit);
        if (outcome.kind === 'success') {
          const payload = outcome.value as { status: number; body: unknown };
          json(res, payload.status, payload.body);
          return;
        }
        if (outcome.kind === 'quiesce') {
          json(res, 503, { ok: false, reason: 'shutting-down' });
          return;
        }
        throw outcome.defect;
      })
      .catch((err: unknown) => {
        console.error(defect.log, err);
        try {
          json(res, 500, defect.body);
        } catch {
          /* socket gone */
        }
      });
  }

  const SETTINGS_COMMAND_DEFECT = { log: 'fleetd handler error:', body: { err: 'internal' } };
  const MAIL_DEFECT = { log: 'fleetd mail error:', body: { ok: false, err: 'internal' } };
  const CLEANUP_DEFECT = { log: 'fleetd cleanup error:', body: { ok: false, err: 'internal' } };
  // P6.4 CONTROL ROUTE GROUP defect dialect — shared by all 11 mutating control
  // POSTs. A defect reproduces routeRequest's outer catch for a non-hook /api/
  // path byte-for-byte — log 'fleetd handler error:' + 500 {"err":"internal"}.
  // The group splits by whether its core write is synchronous or in-flight when
  // shutdown interrupts the request fiber:
  //   FIVE SYNC POSTs (name / questions.answer / questions.dismiss / plans.mark /
  //     plans.assign) complete on the admitting turn, so their fiber is already
  //     done before closing-clients. They ride settleEffectMutatingRoute with this
  //     defect: an interrupt-after-completion is a no-op, and a true quiesce
  //     refusal (workflow never ran) is the only 503 case.
  //   SIX ASYNC POSTs (kill / revive / adopt / dismiss / dismiss-retry / rc) start
  //     a native Promise that interrupt() cannot cancel. They ride
  //     settleControlAsyncRoute (start-once + JOIN-on-interrupt) so closeClients
  //     waits for the in-flight write exactly as res.done joined the legacy
  //     .then(json) chain — a 503 is emitted ONLY when the write provably never
  //     started. The per-route rejection dialect ('fleetd <route> error:' +
  //     500 {ok:false,reason:'internal'}) still lives in the route's onError.
  const CONTROL_DEFECT = { log: 'fleetd handler error:', body: { err: 'internal' } };

  // START-ONCE recorder for an async mutating core call. The workflow invokes
  // `invoke` (memoized): the native Promise starts EXACTLY ONCE, from inside the
  // Effect, and is captured on that first call. The settler reads `started()` to
  // learn, on an interrupts-only Exit, whether that native write is in flight.
  interface StartedOnce<T> {
    readonly invoke: () => Promise<T>;
    readonly started: () => Promise<T> | null;
  }
  function startOnce<T>(operation: () => Promise<T>): StartedOnce<T> {
    let native: Promise<T> | null = null;
    return {
      invoke: () => (native ??= operation()),
      started: () => native,
    };
  }

  // ASYNC-MUTATING SETTLER (POST /mail, POST /api/cleanup, the six async control
  // POSTs). Unlike settleEffectMutatingRoute, an interrupts-only Exit here does
  // NOT blindly become a 503: the workflow's Effect.promise thunk already STARTED
  // a native Promise that interrupt() cannot cancel (an arity-0 thunk carries no
  // AbortController). The frozen invariant is that closeClients — which joins
  // res.done AFTER supervisor.interrupt() — waits for that in-flight mutation
  // exactly as the legacy .then(json) chain on the same Promise did. So the
  // settler JOINS the recorded native Promise and emits the legacy .then/.catch
  // bytes. Only a mutation that PROVABLY never started (recorder.started() === null
  // — an ApplicationQuiescingError admission refusal, or an interrupt before the
  // thunk fired) takes the frozen shutdown 503.
  //   success  → the workflow's assembled {status, body}, same as the sync settler.
  //   quiesce  → started? JOIN + legacy bytes : 503 {ok:false,reason:'shutting-down'}.
  //   defect   → started? JOIN first (never abandon a started write) : per-route
  //              defect arm (log + 500 defect.body), same as the sync settler.
  // onRejected defaults to the defect arm: mail/cleanup's legacy .catch bytes ARE
  // their defect bytes. Control overrides it — its .catch logs the route prefix
  // and answers a 500 SUCCESS wire {ok:false,reason:'internal'}, a different
  // dialect from CONTROL_DEFECT's {err:'internal'}.
  function settleEffectAsyncMutatingRoute<T>(
    routes: HttpEffectRoutes,
    operation: string,
    effect: HttpWorkflowEffect,
    res: HttpResShim,
    recorder: StartedOnce<T>,
    relay: {
      readonly defect: { readonly log: string; readonly body: unknown };
      readonly onFulfilled: (res: HttpResShim, out: T) => void;
      readonly onRejected?: (res: HttpResShim, err: unknown) => void;
    },
  ): void {
    const emitDefect = (err: unknown): void => {
      console.error(relay.defect.log, err);
      try {
        json(res, 500, relay.defect.body);
      } catch {
        /* socket gone */
      }
    };
    const joinNative = (native: Promise<T>): Promise<void> =>
      native.then(
        (out) => relay.onFulfilled(res, out),
        (err) => (relay.onRejected ? relay.onRejected(res, err) : emitDefect(err)),
      );
    routes
      .runRequest(operation, effect)
      .then((exit) => {
        const outcome = mapEffectRouteExit(exit);
        if (outcome.kind === 'success') {
          const payload = outcome.value as { status: number; body: unknown };
          json(res, payload.status, payload.body);
          return;
        }
        // mapEffectRouteExit collapses BOTH an explicit ApplicationQuiescingError
        // refusal AND an interrupts-only Exit to 'quiesce'; the recorder is the
        // only witness that tells them apart. Never started → true refusal → 503.
        const native = recorder.started();
        if (outcome.kind === 'quiesce') {
          if (native === null) {
            json(res, 503, { ok: false, reason: 'shutting-down' });
            return;
          }
          return joinNative(native);
        }
        // Defect (die). A started native op should not coincide with a defect,
        // but if it did, join it first so a started write is never abandoned.
        if (native !== null) return joinNative(native);
        throw outcome.defect;
      })
      .catch((err: unknown) => {
        emitDefect(err);
      });
  }

  // Wire one async control POST through the start-once/join seam. `run` is the raw
  // core control call (spawnKill / revive / adoptSession / dismissSession /
  // dismissRetry / enableRemote); the recorder starts it EXACTLY ONCE and the
  // settler JOINS its Promise on an interrupts-only Exit. On a JOINED rejection
  // onRejected emits the legacy .catch bytes (log `errorPrefix` + 500
  // {ok:false,reason:'internal'}). NOTE: controlAsyncWorkflow ALSO folds a native
  // rejection through the SAME onError (control.ts), and that fold's continuation
  // still runs after the fiber is interrupted; so an interrupt racing a native
  // REJECTION logs `errorPrefix` twice. The response bytes stay single and
  // correct — this is a harmless duplicate log line during shutdown only.
  function settleControlAsyncRoute(
    routes: HttpEffectRoutes,
    operation: string,
    res: HttpResShim,
    run: () => ControlResult,
    errorPrefix: string,
  ): void {
    const onError = (err: unknown): void => {
      console.error(errorPrefix, err);
    };
    const recorder = startOnce(run);
    settleEffectAsyncMutatingRoute(
      routes,
      operation,
      routes.controlAsync({ run: recorder.invoke, onError }),
      res,
      recorder,
      {
        defect: CONTROL_DEFECT,
        onFulfilled: (target, out) => {
          json(target, out.status, out.body);
        },
        onRejected: (target, err) => {
          onError(err);
          json(target, 500, { ok: false, reason: 'internal' });
        },
      },
    );
  }

  // P9.2 Slice 2 PREFLIGHT SETTLER (POST /api/repos/preflight). Modeled on
  // settleControlAsyncRoute (start-once witness + JOIN-on-interrupt + quiesce 503)
  // over the SAME generic settleEffectAsyncMutatingRoute — the generic settler is
  // parameterized with no defaults, so it is left byte-identical; preflight only
  // supplies its own config ALONGSIDE (§6-OQ-3 fallback: default-preserving
  // parameterization of settleControlAsyncRoute is NOT cleanly provable because it
  // also hardcodes routes.controlAsync). The ONE divergence from control is the 500
  // dialect: preflight's defect body AND its joined-rejection body are BOTH
  // { ok: false, reason: 'Git access check failed internally' } (the legacy route's
  // single `.catch`), NOT CONTROL_DEFECT's {err:'internal'} nor controlAsync's
  // {reason:'internal'}. `run` is the raw core.preflightRepo call; onError logs the
  // frozen 'fleetd repo preflight error:' prefix. NOTE (mirrors settleControlAsyncRoute):
  // repoPreflightWorkflow ALSO folds a native rejection through the SAME onError,
  // so an interrupt racing a native REJECTION logs the prefix twice — a harmless
  // duplicate log line during shutdown only; the response bytes stay single.
  const PREFLIGHT_DEFECT = {
    log: 'fleetd repo preflight error:',
    body: { ok: false, reason: 'Git access check failed internally' },
  };
  function settleEffectPreflightRoute(
    routes: HttpEffectRoutes,
    operation: string,
    res: HttpResShim,
    run: () => ControlResult,
  ): void {
    const onError = (err: unknown): void => {
      console.error(PREFLIGHT_DEFECT.log, err);
    };
    const recorder = startOnce(run);
    settleEffectAsyncMutatingRoute(
      routes,
      operation,
      routes.repoPreflight({ run: recorder.invoke, onError }),
      res,
      recorder,
      {
        defect: PREFLIGHT_DEFECT,
        onFulfilled: (target, out) => {
          json(target, out.status, out.body);
        },
        onRejected: (target, err) => {
          onError(err);
          json(target, 500, { ok: false, reason: 'Git access check failed internally' });
        },
      },
    );
  }

  // SPAWN SETTLER (POST /api/spawn — P9.1 Slice 6a). Modeled on
  // settleControlAsyncRoute (start-once witness + JOIN-on-interrupt + quiesce 503)
  // with ONE contractual divergence in the defect arm. spawn is the single control
  // POST whose 500 body is COMPUTED from the escaped error, not static: the legacy
  // route's .catch was
  //   console.error('fleetd spawn error:', err);
  //   json(res, 500, { ok: false, reason: spawnFailureReason(err) });
  // and spawnFailureReason applies redactGitText/scrubUrlCredentials + one-line +
  // truncation (spawns.ts) so a token-bearing clone URL never reaches the wire.
  // Design D6 makes that redaction contractual — a defect MUST reproduce these
  // bytes, NEVER CONTROL_DEFECT's {"err":"internal"}. settleEffectAsyncMutatingRoute
  // cannot serve this: its relay.defect.body is a STATIC value it cannot vary per
  // error. So spawn owns this settler and funnels EVERY failure arm — a joined
  // native rejection, a never-started defect, and the outer catch — through ONE
  // emitSpawnFailure(err).
  //   success  → spawn's assembled wire verbatim (202 provisioning, early 4xx, AND
  //              the maintenance-gate 503 {ok:false,reason:'daemon is shutting
  //              down; spawn maintenance is quiescing'} — a NORMAL success wire,
  //              distinct from the transport quiesce 503 below).
  //   quiesce  → started? JOIN the in-flight native write (closeClients waits for
  //              it exactly as the legacy .then(json) chain did) : 503
  //              {ok:false,reason:'shutting-down'} — the transport refusal, emitted
  //              ONLY when the native write PROVABLY never started (an
  //              ApplicationQuiescingError admission refusal or an interrupt before
  //              the Effect.promise thunk fired).
  //   defect   → the die comes FROM the rejected native (Effect.promise raises a
  //              rejection as a die and the workflow does NOT fold it), so it
  //              coincides with a started write — JOIN it, rendering
  //              spawnFailureReason. The never-started-defect edge (a synchronous
  //              throw constructing the promise; ownedSpawn never does —
  //              runMaintenance turns a sync throw into a rejected Promise) still
  //              emits spawnFailureReason(defect) via the outer catch, NOT
  //              {"err":"internal"} (D6).
  function settleEffectSpawnRoute(
    routes: HttpEffectRoutes,
    operation: string,
    res: HttpResShim,
    run: () => ControlResult,
  ): void {
    const emitSpawnFailure = (err: unknown): void => {
      console.error('fleetd spawn error:', err);
      try {
        json(res, 500, { ok: false, reason: spawnFailureReason(err) });
      } catch {
        /* socket gone */
      }
    };
    const recorder = startOnce(run);
    const joinNative = (native: ControlResult): Promise<void> =>
      native.then(
        (out) => {
          json(res, out.status, out.body);
        },
        (err) => emitSpawnFailure(err),
      );
    routes
      .runRequest(operation, routes.spawnRoute({ run: recorder.invoke }))
      .then((exit) => {
        const outcome = mapEffectRouteExit(exit);
        if (outcome.kind === 'success') {
          const payload = outcome.value as { status: number; body: unknown };
          json(res, payload.status, payload.body);
          return;
        }
        const native = recorder.started();
        if (outcome.kind === 'quiesce') {
          if (native === null) {
            json(res, 503, { ok: false, reason: 'shutting-down' });
            return;
          }
          return joinNative(native);
        }
        // Defect. A started native op renders spawnFailureReason via the join; the
        // never-started edge falls through to the outer catch → emitSpawnFailure.
        if (native !== null) return joinNative(native);
        throw outcome.defect;
      })
      .catch((err: unknown) => {
        emitSpawnFailure(err);
      });
  }

  function legacySettingsResponse(res: HttpResShim, ev: unknown): void {
    const out = core.setSettings(ev);
    json(res, out.status, out.body);
  }

  function legacyCommandResponse(res: HttpResShim, ev: unknown): void {
    json(res, 200, core.command((ev as { text?: unknown }).text));
  }

  function legacyMailResponse(res: HttpResShim, ev: unknown): void {
    core
      .postMail(ev as Parameters<typeof core.postMail>[0])
      // 0.16.0: postMail returns {status, body} on a refusal and the
      // historical bare delivery object on success (in-process callers
      // consume the bare shape — the adapter lives in mailWorkflow too).
      .then((out) => {
        json(res, out.status ?? 200, out.body ?? out);
      })
      .catch((err: unknown) => {
        console.error('fleetd mail error:', err);
        json(res, 500, { ok: false, err: 'internal' });
      });
  }

  function legacyCleanupResponse(res: HttpResShim): void {
    // BUG-145: an incomplete Clear (tmux unreachable / a dead window
    // that would not die) comes back {ok:false, reason} with NOTHING
    // touched — speak a real code so the board can fail loud.
    core
      .cleanup()
      .then((out) => {
        json(res, !out.ok ? 409 : 200, out);
      })
      .catch((err: unknown) => {
        console.error('fleetd cleanup error:', err);
        json(res, 500, { ok: false, err: 'internal' });
      });
  }

  function dispatchSettings(res: HttpResShim, ev: unknown): void {
    if (!effectRoutes) {
      legacySettingsResponse(res, ev);
      return;
    }
    settleEffectMutatingRoute(
      effectRoutes,
      'POST /api/settings',
      effectRoutes.settings(settingsCapabilities(ev)),
      res,
      SETTINGS_COMMAND_DEFECT,
    );
  }

  function dispatchCommand(res: HttpResShim, ev: unknown): void {
    if (!effectRoutes) {
      legacyCommandResponse(res, ev);
      return;
    }
    settleEffectMutatingRoute(
      effectRoutes,
      'POST /command',
      effectRoutes.command(commandCapabilities(ev)),
      res,
      SETTINGS_COMMAND_DEFECT,
    );
  }

  function dispatchMail(res: HttpResShim, ev: unknown): void {
    if (!effectRoutes) {
      legacyMailResponse(res, ev);
      return;
    }
    // start-once around the SAME core thunk mailCapabilities names, so the
    // settler can JOIN the in-flight postMail() on an interrupts-only Exit.
    const recorder = startOnce(mailCapabilities(ev).postMail);
    settleEffectAsyncMutatingRoute(
      effectRoutes,
      'POST /mail',
      effectRoutes.mail({ postMail: recorder.invoke }),
      res,
      recorder,
      {
        // legacy .catch bytes == MAIL_DEFECT bytes, so onRejected defaults to it.
        defect: MAIL_DEFECT,
        // legacy .then bytes: postMail returns {status, body} on a refusal and
        // the bare delivery object on success — the `?? out` fallback matches
        // legacyMailResponse and adaptMailResult.
        onFulfilled: (target, out) => {
          const rec = out as { status?: number; body?: unknown };
          json(target, rec.status ?? 200, rec.body ?? out);
        },
      },
    );
  }

  function dispatchCleanup(res: HttpResShim): void {
    if (!effectRoutes) {
      legacyCleanupResponse(res);
      return;
    }
    const recorder = startOnce(cleanupCapabilities().cleanup);
    settleEffectAsyncMutatingRoute(
      effectRoutes,
      'POST /api/cleanup',
      effectRoutes.cleanup({ cleanup: recorder.invoke }),
      res,
      recorder,
      {
        // legacy .catch bytes == CLEANUP_DEFECT bytes, so onRejected defaults.
        defect: CLEANUP_DEFECT,
        // BUG-145 legacy .then bytes: an incomplete Clear comes back
        // {ok:false, reason} untouched — a real 409 so the board fails loud.
        onFulfilled: (target, out) => {
          json(target, !out.ok ? 409 : 200, out);
        },
      },
    );
  }

  // AUTH CONTRACT (http-policy.tokenMatches): every non-loopback HTTP route and
  // WebSocket upgrade shares this exact gate. This binds our own token; the
  // constant-time comparison lives in the policy leaf.
  const tokenMatches = (candidate: unknown) => policyTokenMatches(token, candidate);

  // PROXY AUTH CONTRACT. A reverse proxy connects to us over loopback, so the
  // loopback exemption below would hand the entire fleet — spawn included — to
  // anyone who can reach the proxy. Which of the two is correct is not something
  // the daemon can infer, so the operator states it:
  //
  //   'token' (default) — a browser arriving through a trusted external origin
  //     must still present the bearer token. Defence in depth, and the only safe
  //     default: it is the behaviour an operator gets if they configure a proxy
  //     and think no further about auth.
  //   'trust' — the proxy is the authenticator (Coder authenticates before it
  //     ever forwards, and coder_app defaults to share = "owner"). A trusted
  //     origin is then sufficient and the board needs no token at all.
  //
  // A LEGITIMATE local CLI hook sends our OWN loopback Host and no Origin, so
  // arrivedViaTrustedProxy is false; the proxy is caught by its EXTERNAL Host
  // even with no Origin (see arrivedViaTrustedProxy for why Origin alone was not
  // enough). But a hostile local process can DELIBERATELY forge the trusted
  // external Host/Origin, so hook authentication must never key off those
  // headers — see the unconditional /hook/* guard immediately below.
  function authorized(req: HttpReqShim, url: URL) {
    // /hook/* is authenticated UNCONDITIONALLY: no loopback or proxy-trust path
    // may waive it. Every hook arrives through a command shim
    // (scripts/fleet-hook.mjs / fleet-sessionstart.mjs / fleet-watch.mjs) that
    // reads $FLEETDECK_HOME/token and attaches the bearer, because Claude Code
    // http hooks cannot. A tokenless /hook/* call — a legacy pre-0.16.0 CLI or a
    // local forgery — must fail the bearer check below. The router still answers
    // it with the canonical hook no-op (HTTP 200 `{}`), because an auth/home/token
    // mismatch must never add context, show a warning, or interrupt Claude. This
    // guard LEADS the loopback block on purpose: the PROXY_AUTH=trust exemption
    // (return true, below) would otherwise authorize a FORGED hook, because a
    // direct loopback process can forge the trusted Host/Origin to make
    // arrivedViaTrustedProxy(req) true and short-circuit before the plain-
    // loopback hook exclusion is ever evaluated. Keeping the gate here means it
    // holds under every mode: token proxy, trust proxy, plain loopback, LAN,
    // REQUIRE_TOKEN, and TRUST_LOOPBACK alike.
    const isHook = url.pathname.startsWith('/hook/');
    if (isLoopbackAddress(req.socket.remoteAddress) && !isHook) {
      // Use arrivedViaTrustedProxy, NOT viaTrustedProxy: the latter keys off
      // Origin alone and so waived the token for a proxied request that carried
      // no Origin — the no-Origin bypass fixed here.
      const viaProxy = arrivedViaTrustedProxy(req);
      if (proxyAuth === 'token' && viaProxy) {
        // NO-ORIGIN PROXY HOLE: a browser that genuinely reached us THROUGH the
        // proxy must still present the bearer even over loopback (see
        // arrivedViaTrustedProxy). Do NOT auto-exempt — fall through to the token
        // check below. REQUIRE_TOKEN never loosens this gate.
      } else if (proxyAuth === 'trust' && viaProxy) {
        // PROXY_AUTH=trust: the operator has explicitly made the reverse proxy
        // the authenticator (Coder et al. authenticate before forwarding), so a
        // request that genuinely arrived through the trusted proxy needs no token
        // at all. REQUIRE_TOKEN exists to close the LOOPBACK trust zone against
        // other local OS users; it must NOT override this deliberate operator
        // decision to trust the proxy, so this exemption survives the flag.
        return true;
      } else {
        // PLAIN LOOPBACK (not via the proxy). /hook/* never reaches here — the
        // unconditional guard at the top of authorized() already excluded it.
        // What remains is the ordinary loopback exemption: (a) /health and the
        // data-free public shell, open for everyone; (b) since 0.16.0 the daemon
        // always mints a token, so when REQUIRE_TOKEN is off (the default) every
        // other loopback route keeps the historical exemption EXCEPT the
        // specific powers named in REQUIRE_TOKEN_GATED_ROUTES (/ws/term, POST
        // /mail, gateway settings writes, the unsupervised-spawn arm) — the
        // powers a malicious same-UID process or a fleet agent itself must not
        // wield anonymously. REQUIRE_TOKEN=on keeps its stronger meaning:
        // everything except /health and the shell requires the bearer.
        // TRUST_LOOPBACK=on restores the historical exemption for the named
        // power routes too (the single-user opt-out); it does NOT touch hooks,
        // which stay gated at the top regardless.
        if (url.pathname === '/health' || isPublicShell(req.method, url.pathname)) return true;
        if (!requireToken && (trustLoopback || !tokenGatedRoute(req.method, url.pathname)))
          return true;
      }
    }
    const bearer = parseBearer(req.headers.authorization);
    return tokenMatches(bearer) || tokenMatches(url.searchParams.get('t'));
  }

  // SILENT AUTH FAILURE. Sessions started before authenticated command shims
  // (and modern shims reading a missing/stale token or the wrong FLEETDECK_HOME)
  // arrive exactly like a local forgery: the daemon cannot distinguish them, so
  // none may be ingested. They also must not leak an infrastructure problem into
  // the developer's conversation. Every rejected hook therefore receives the
  // one safe response for every event: HTTP 200 with canonical `{}`. The board
  // still learns the affected session id through legacySessions below and can
  // show an operator diagnostic without steering or interrupting Claude.
  // The board banner reads these: which sessions are running pre-0.16.0 hooks
  // (still to restart) and which have already proven they're on the new shims
  // (an authenticated hook arrived). A session moves from the first set to
  // the second exactly once — its first authenticated hook — so the banner
  // self-heals as the human restarts things. In-memory: a daemon restart
  // simply re-learns both from the next hooks each session emits.
  const legacySessions = new Set();
  const upgradedSessions = new Set();
  function noteLegacySession(sid: unknown) {
    if (typeof sid !== 'string' || !sid || sid === 'unknown') return;
    if (upgradedSessions.has(sid)) return;
    if (legacySessions.has(sid)) return;
    legacySessions.add(sid);
    // The board learns legacy_upgrade from the /ws frame now — a tokenless
    // hook changes no session state (nothing else would broadcast), so push
    // one ourselves or a live board never sees the restart banner appear.
    scheduleBroadcast();
  }
  function noteUpgradedSession(sid: unknown) {
    if (typeof sid !== 'string' || !sid || sid === 'unknown') return;
    if (upgradedSessions.has(sid)) return;
    upgradedSessions.add(sid);
    const wasLegacy = legacySessions.delete(sid);
    // Same push when a legacy session restarts (its banner entry must shrink)
    // — unless the authenticated hook mutates session state anyway and will
    // broadcast on its own (the common SessionStart path).
    if (wasLegacy) scheduleBroadcast();
  }
  function legacyBanner() {
    return { sessions: [...legacySessions], upgraded: upgradedSessions.size };
  }
  function silentHookRefusal(res: HttpResShim, ev: unknown) {
    const sidRaw = asRecord(ev)['session_id'];
    const sid = typeof sidRaw === 'string' ? sidRaw : null;
    noteLegacySession(sid);
    json(res, 200, {});
  }

  // SAME-ORIGIN CONTRACT (C1/H-S3). Loopback auto-authorizes, and a browser is a
  // loopback peer — so a page on ANY site the user visits could otherwise open
  // ws://127.0.0.1/ws (read the whole snapshot, drive a live pane) or blind-POST
  // /api/spawn (RCE). The token alone does not stop this: the local board carries
  // none. The wall is instead "is this request same-origin with us?", enforced
  // for every state-changing POST, both WS upgrades, and (for DNS rebinding) the
  // Host of every data route. Loopback CLI hooks send no Origin and a loopback
  // Host, so they sail straight through.
  const daemonPort = String(port);
  // Hostnames that count as "us": loopback (localhost, 127/8, ::1 — via
  // isLoopbackAddress), every address this host actually answers on, and the
  // advertised mDNS .local name. The address set is REFRESHED from the interface
  // list on every checked request (cheaply): Wi-Fi roaming, DHCP renewal and VPN
  // changes DO move the LAN address under a long-lived daemon, and a snapshot
  // taken at startup would otherwise reject the board's own new address as a
  // DNS-rebinding attempt for the daemon's whole lifetime (BUG-118/129).
  const lanHosts = new Set<string>();
  // os.getAddresses is a non-standard method: it is absent from @types/node and
  // from Node itself, so this probe is always false on the supported runtimes and
  // we fall through to networkInterfaces() (see ts-migration-bugs). Kept as a
  // defensive branch for a host runtime that might provide it; the optional-typed
  // view keeps the probe honest without asserting the method exists.
  const nativeGetAddresses = (os as typeof os & { getAddresses?: () => { address?: string }[] })
    .getAddresses;
  const osGetAddresses: () => ({ address?: string } | undefined)[] =
    typeof nativeGetAddresses === 'function'
      ? () => nativeGetAddresses()
      : () => Object.values(networkInterfaces()).flat();
  // The advertised .local name is a STANDING member of the allowlist, not
  // interface data: the per-request refresh clears and rebuilds the address set,
  // so it must re-add this name every time or the very first checked request via
  // the mDNS URL would evict it and 403 as a DNS-rebinding attempt. `lan` may be
  // a thunk (BUG-122/051), so resolve it once here to seed the standing name.
  let mdnsHost: string | null = null;
  try {
    const lanSeed = typeof lan === 'function' ? lan() : lan;
    if (lanSeed?.mdns) mdnsHost = new URL(lanSeed.mdns).hostname.toLowerCase();
  } catch {
    /* malformed mDNS URL — skip it; the IP URLs still work */
  }
  function refreshLanHosts() {
    try {
      lanHosts.clear();
      for (const entry of osGetAddresses()) {
        if (entry?.address) lanHosts.add(entry.address.toLowerCase());
      }
      // Re-resolve the advertised .local name from the LIVE lan source each
      // refresh, and keep it sticky once seen. The share URL is rendered from
      // the same live thunk (currentLan), so the two must never diverge
      // (BUG-119). Seeding mdnsHost ONLY at construction missed a responder that
      // finished binding AFTER the HTTP layer was built — the exact name we then
      // advertised got 403'd as a DNS-rebinding attempt. Sticky, because a
      // transient responder blip must not evict a name we already published
      // (BUG-122/051), which is also why we never clear it back to null here.
      try {
        const live = typeof lan === 'function' ? lan() : lan;
        if (live?.mdns) mdnsHost = new URL(live.mdns).hostname.toLowerCase();
      } catch {
        /* malformed/absent live mDNS URL — keep the last known name */
      }
      if (mdnsHost) lanHosts.add(mdnsHost);
    } catch {
      /* restricted sandbox: loopback stays allowed regardless */
    }
  }
  refreshLanHosts();

  // The DNS-rebinding / same-origin decisions are pure (http-policy: hostIsOwn,
  // authorityTrusted, originTrusted). hostAllowed keeps the per-request LAN
  // refresh (I/O + mutable lanHosts) here and delegates the verdict; the trusted
  // wrappers bind the operator's trustedOrigins list to the policy leaves.
  function hostAllowed(u: URL) {
    refreshLanHosts();
    return hostIsOwn(u, lanHosts, daemonPort);
  }
  const authorityTrusted = (u: URL) => policyAuthorityTrusted(trustedOrigins, u);
  const originTrusted = (u: URL) => policyOriginTrusted(trustedOrigins, u);
  // Host header check — the DNS-rebinding wall. A browser always sends Host, so a
  // domain that re-resolves to this box arrives as Host: evil.example and is
  // refused. A missing Host is a non-browser caller and is left alone. A proxied
  // request arrives with the PROXY's Host, which only passes once an operator has
  // named it in FLEETDECK_TRUSTED_ORIGINS.
  function hostHeaderOk(req: HttpReqShim) {
    const host = req.headers.host;
    if (typeof host !== 'string' || !host) return true;
    let u;
    try {
      u = new URL('http://' + host);
    } catch {
      return false;
    }
    return hostAllowed(u) || authorityTrusted(u);
  }
  // Sec-Fetch-Site + Origin verdict for a STATE-CHANGING request. Returns null
  // when it may proceed. Sec-Fetch-Site, when the browser sends it, is
  // authoritative for the cross-site call; an Origin, when present, must resolve
  // to one of our own hosts; no Origin at all is a non-browser CLI hook and is
  // allowed. The reason drives our control flow only — it is never echoed back.
  function crossSiteReason(req: HttpReqShim) {
    const site = req.headers['sec-fetch-site'];
    if (site === 'cross-site' || site === 'cross-origin') return 'cross-site';
    const origin = req.headers.origin;
    if (typeof origin === 'string' && origin) {
      let u;
      try {
        u = new URL(origin);
      } catch {
        return 'bad-origin';
      } // 'null', junk
      if (!hostAllowed(u) && !originTrusted(u)) return 'cross-origin';
    }
    return null;
  }

  // Is this a browser arriving through a reverse proxy — i.e. an Origin that is
  // trusted but is NOT one of our own hosts? Such a request has already cleared
  // the walls above; this only decides whether it must ALSO carry the token.
  function viaTrustedProxy(req: HttpReqShim) {
    const origin = req.headers.origin;
    if (typeof origin !== 'string' || !origin) return false; // a CLI hook
    let u;
    try {
      u = new URL(origin);
    } catch {
      return false;
    }
    return !hostAllowed(u) && originTrusted(u);
  }
  // C1/H-S3 NO-ORIGIN PROXY HOLE. viaTrustedProxy keys off Origin alone — but a
  // reverse proxy connects to us over loopback, so an attacker who reaches the
  // public proxy and sends a request with a trusted Host, NO Origin and NO token
  // looked exactly like a local CLI hook: isLoopbackAddress true, viaTrustedProxy
  // false ⇒ the loopback exemption returned authorized. Under the default
  // PROXY_AUTH=token that waived the bearer token entirely (spawn/state/mail/
  // cleanup exposed tokenless), defeating the standalone auth model.
  //
  // The Host header carries the signal Origin does not: a genuine loopback hook
  // sends our OWN authority (127.0.0.1:port, localhost) — hostAllowed — while a
  // proxied request sends the proxy's EXTERNAL authority (board.example.com),
  // which is authorityTrusted but NOT hostAllowed. Treat EITHER the Origin-based
  // signal or that Host-based signal as "arrived through the proxy", so such a
  // request must still clear the token check below.
  //
  // RESIDUAL (out of scope): a proxy that REWRITES Host to loopback still reads
  // as local. Coder and the documented proxies preserve req.Host (see ~line 116),
  // so this does not arise in the supported deployments.
  function arrivedViaTrustedProxy(req: HttpReqShim) {
    if (viaTrustedProxy(req)) return true;
    const host = req.headers.host;
    if (typeof host !== 'string' || !host) return false; // a CLI hook may omit Host
    let u;
    try {
      u = new URL('http://' + host);
    } catch {
      return false;
    }
    return authorityTrusted(u) && !hostAllowed(u);
  }

  // PROVENANCE LOG (exec-class control routes). spawn/kill/revive/adopt/rc each
  // start a process or move a live pane, so one audit line records WHERE the
  // call came from: the socket peer, and whether it arrived through a trusted
  // reverse proxy (a browser at the proxy) rather than a direct loopback/LAN
  // caller. Deliberately NEVER the token, headers or body — provenance, not
  // payload. Matches the daemon's `fleetd …:` stderr dialect so it lands in
  // fleetd.log alongside the other operational lines.
  function logExec(route: string, req: HttpReqShim, extra = '') {
    const from = req.socket.remoteAddress ?? 'unknown';
    console.error(
      `fleetd exec ${route} from ${from} proxied=${arrivedViaTrustedProxy(req)}${extra}`,
    );
  }

  // PermissionRequest / Elicitation / AskUserQuestion are handled OUT of this
  // table (Phase 3/4 hold-open relay — the response is parked, see the hook
  // branch below).
  // Payloads WITHOUT a session_id still ingest best-effort telemetry (an
  // unknown-name hook, a telemetry-only Notification, and the
  // AskUserQuestion→PermissionRequest pairing all stay visible), but they are
  // never DISPATCHED to a hook handler — the dispatch gate below refuses them.
  // FileChanged is the exception since the v0.22.5 hotfix: its handler acks
  // WITHOUT ingesting, so it never reaches the state machine or conflict ledger
  // (see the hookHandlers entry below and tests/filechanged-watch.test.ts).
  const hookHandlers: Record<string, (ev: HookBody) => unknown> = {
    // 0.16.0: the hook that may have just performed the version takeover gets
    // the upgrade lines appended — the human who started THAT session hears
    // about every other session still needing a restart (see fleet-sessionstart).
    SessionStart: (ev) => {
      const out = core.hookSessionStart(ev);
      // fleet_takeover is a real SessionStart field set by the fleet-sessionstart
      // shim, but it is absent from the HookEvent interface in events.ts (see
      // ts-migration-bugs) — read it defensively off the wire body.
      const takeover = asRecord(ev)['fleet_takeover'];
      if (takeover && typeof out === 'object') {
        (out as Record<string, unknown>)['upgrade_lines'] = core.takeoverBriefLines(
          takeover as Parameters<typeof core.takeoverBriefLines>[0],
          legacyBanner(),
        );
      }
      return out;
    },
    UserPromptSubmit: (ev) => core.hookUserPromptSubmit(ev),
    PostToolUse: (ev) => core.hookPostToolUse(ev),
    PreToolUse: (ev) => core.hookPostToolUse(ev), // same derivation branch as the spike
    // BUG-102: a FAILED tool call is still a completed tool call — route it
    // through the same correlated expiry so its permission hold retires now
    // instead of after the full hold window. hookPostToolUse keeps the event's
    // own name (PostToolUseFailure) in applyEvent and any whisper.
    PostToolUseFailure: (ev) => core.hookPostToolUse(ev),
    Stop: (ev) => core.hookStop(ev),
    SessionEnd: (ev) => core.hookSessionEnd(ev),
    Notification: (ev) => (core.applyEvent({ ...ev, hook_event_name: 'Notification' }), {}),
    // Older cached plugin hooks can keep emitting FileChanged after an upgrade.
    // Acknowledge them without touching session state or the conflict ledger.
    FileChanged: () => ({}),
    // CwdChanged remains pure telemetry for the session event log.
    CwdChanged: (ev) => (core.applyEvent({ ...ev, hook_event_name: 'CwdChanged' }), {}),
  };

  // F3a/F3b/F3c hold-open relay: create the durable question row, then park
  // the HTTP response until the board answers, the hold window lapses
  // (respond {} — normal flow resumes in the terminal), or the client
  // disconnects. questions.mjs owns the arbitration; this only wires the
  // socket to it. Fail open like every hook path: intake errors still 200 {}.
  function holdHook(res: HttpResShim, ev: unknown, name: string) {
    // A request that crossed the router immediately before quiesce must not
    // create a fresh durable question after shutdown began. Hooks always fail
    // open with their canonical response.
    if (quiescing) {
      json(res, 200, {});
      return;
    }
    let row: ReturnType<typeof core.hookHoldQuestion> | null = null;
    try {
      row = core.hookHoldQuestion(ev as Parameters<typeof core.hookHoldQuestion>[0], name);
    } catch (err) {
      console.error('fleetd hold intake error:', err);
    }
    if (!row) {
      json(res, 200, {});
      return;
    }
    const held = row;
    // seam cast: events.ts deliberately narrows questions.create to { id: number }
    // in its ctx contract, so hookHoldQuestion is typed { id: number } | null; the
    // runtime row is a full QuestionRow, which is what attachHold/socketClosed read
    // (row.session_id, row.id). Cast at this seam rather than perturb the contract.
    core.questions.attachHold(
      held as Parameters<typeof core.questions.attachHold>[0],
      (obj: unknown) => {
        json(res, 200, obj);
      },
    );
    res.on('close', () => {
      try {
        core.questions.socketClosed(held.id);
      } catch {
        /* hold hygiene only */
      }
    });
    // response intentionally left open
  }

  // P6.4 HOOK ROUTE GROUP transport. The hook dispatch decision (unknown name /
  // malformed payload / known handler) is now the E=never workflow in
  // app/http-workflows/hooks.ts; this block wires it through the P6.3 bridge and
  // settles it FAIL-OPEN. It sits next to hookHandlers/holdHook rather than with
  // the other settlers because hookDispatchCapabilities closes over hookHandlers.

  // B2 reply floor. A wedged Effect runtime whose bridge Promise never settles
  // would otherwise strand a hook forever: an active request runs with idleTimeout
  // 0 (immortal), the keep-alive FINs close the socket without writing a body, and
  // boundStalledDrain never arms (the body is already drained). So a single
  // unref'd, idempotent timer synthesizes the canonical 200 {} after this many ms.
  // Read at createHttp construction time (default 5000ms) so a test can shorten it
  // via FLEETDECK_HOOK_REPLY_FLOOR_MS before the server binds. It never fires under
  // load — the sync hook workflow settles on a microtask — and never truncates a
  // HOLD (holds answer through legacy holdHook and never reach this settler).
  const HOOK_REPLY_FLOOR_MS = (() => {
    const raw = Number(process.env['FLEETDECK_HOOK_REPLY_FLOOR_MS']);
    return Number.isFinite(raw) && raw > 0 ? raw : 5000;
  })();

  // HOOK-ROUTE SETTLER — the FOURTH settle shape, and the only one that fails
  // OPEN. No Exit shape can produce a non-200: mapHookExit (hook-policy.ts, NOT
  // mapEffectRouteExit) folds success→its body and EVERY failure — an
  // ApplicationQuiescingError refusal, an interrupts-only interruption, a die, a
  // handler that threw — to {}. There is no quiesce branch, no defect rethrow, no
  // 503/500: the fail-open contract (tests/p6-hook-failopen-contract.test.ts)
  // forbids every one of them, and no Cause/stack/token/path is ever read.
  //   B1 (the reply survives interruption): the reply is emitted from the bridge
  //   Promise's terminal arms, not an in-Effect finalizer. The P6.3 bridge ALWAYS
  //   resolves — success, a quiesce refusal, and an interrupts-only Exit all
  //   resolve (only a synchronous submission throw rejects) — so .then runs
  //   mapHookExit on every settled Exit and .catch covers the submission throw. An
  //   interruption cannot skip the reply; it resolves an Exit that becomes {}.
  //   B2 (the floor above) covers the wedged-runtime case where the Promise never
  //   settles at all.
  // One idempotent emitter (failOpen) is shared by all three arms; whichever fires
  // first wins and the rest no-op (res.end is itself idempotent — belt and braces).
  function settleEffectHookRoute(
    routes: HttpEffectRoutes,
    operation: string,
    effect: HttpWorkflowEffect,
    res: HttpResShim,
  ): void {
    let settled = false;
    let floor: ReturnType<typeof setTimeout> | null = null;
    const failOpen = (body: unknown): void => {
      if (settled) return;
      // Emit FIRST; commit (settled + clear the floor) only AFTER json() returns.
      // A Success whose value is unserializable — JSON.stringify turns it into
      // undefined, or it is circular — makes json() throw BEFORE it writes a byte.
      // Had we already flipped settled and cleared the floor, that request would be
      // answered-never. So on a throw, emit the canonical fail-open 200 {} right
      // here rather than stranding it until the floor; only if THAT also throws (the
      // socket is genuinely gone) do we swallow and leave settled=false, so the
      // unref'd floor timer stays armed as the last-resort emitter.
      try {
        json(res, 200, body);
      } catch {
        try {
          json(res, 200, {});
        } catch {
          /* socket gone — leave settled=false; the floor stays armed */
          return;
        }
      }
      settled = true;
      if (floor) clearTimeout(floor);
    };
    floor = setTimeout(() => failOpen({}), HOOK_REPLY_FLOOR_MS);
    floor.unref();
    routes
      .runRequest(operation, effect)
      .then((exit) => {
        failOpen(mapHookExit(exit).body);
      })
      .catch(() => {
        // A synchronous submission throw is the ONLY bridge rejection; still 200 {}.
        failOpen({});
      });
  }

  // Build the hook workflow's capabilities over the already-parsed event body.
  // The three thunks are called INSIDE the Effect, so constructing this object has
  // no side effect — only running the workflow dispatches. `handler` is null for an
  // unknown event name (the workflow then ingests via ingestUnknown and answers
  // {}); ingestUnknown reproduces the legacy unknown-event telemetry byte-for-byte
  // (hook_event_name first, then the spread raw body).
  function hookDispatchCapabilities(name: string, ev: unknown): HookDispatchRouteCapabilities {
    const handler = hookHandlers[name];
    return {
      handler: handler ? () => handler(ev as HookBody) : null,
      valid: () => validateHookEvent(ev).ok,
      ingestUnknown: () => {
        core.applyEvent({ hook_event_name: name, ...asRecord(ev) });
      },
    };
  }

  // POST /hook/:name dispatch: legacy when the bridge is unwired, else the
  // workflow settled fail-open. The rollback path reproduces the former inline
  // dispatch (unknown → ingest + {}; invalid → {}; known+valid → handler ?? {})
  // byte-for-byte, so removing installEffectRoutes restores the exact prior bytes.
  function dispatchHook(res: HttpResShim, name: string, ev: unknown): void {
    if (!effectRoutes) {
      const handler = hookHandlers[name];
      if (!handler) {
        core.applyEvent({ hook_event_name: name, ...asRecord(ev) });
        json(res, 200, {});
        return;
      }
      if (!validateHookEvent(ev).ok) {
        json(res, 200, {});
        return;
      }
      json(res, 200, handler(ev as HookBody) ?? {});
      return;
    }
    settleEffectHookRoute(
      effectRoutes,
      `POST /hook/${name}`,
      effectRoutes.hookDispatch(hookDispatchCapabilities(name, ev)),
      res,
    );
  }

  // GET /api/watch v2 — long-poll consumed by scripts/fleet-watch.mjs (the
  // asyncRewake watcher). v2 (orchestrator routing + mail-wake): claims mail
  // from ANY sender, not just board answers, and the watcher stays alive on
  // session_alive alone.
  //
  //   GET /api/watch?session=<sid>[&hold_ms=<0..25000>]   → always 200 JSON
  //
  //   {status:'mail', mail_id, at, from, text}
  //     The OLDEST undelivered mail for <sid> — from ANY sender — existed
  //     (or arrived during the hold) and was ATOMICALLY claimed by this
  //     response. BUG-034: the claim is an EXPIRING IN-FLIGHT LEASE, not a
  //     delivery — claimed_at is set (deadline) while delivered_at stays
  //     NULL, so the turn-boundary path (UserPromptSubmit/Stop-block/GET
  //     /mail drains, which all filter delivered_at IS NULL plus a
  //     live-lease check) can never re-deliver it WHILE the lease lives, and
  //     the watcher finalizes delivery with POST /mail/ack once it holds the
  //     body. A claim whose response never reached the watcher simply lets
  //     the lease lapse — the retention sweep releases it and the mail is
  //     re-delivered instead of lost. `text` is the RAW
  //     mail text including its own frame ([FLEETDECK ANSWER] …,
  //     [FLEETDECK ASSIGNMENT] …, or plain board/session mail) — no prefix
  //     stripping; the Stop hook's rewakeMessage is neutral in v2 and each
  //     mail carries its own frame. `from` is the sender id
  //     (fleetdeck-answer, orchestrator, human, a callsign, …).
  //   {status:'idle', session_alive, pending}
  //     Nothing deliverable. Sent IMMEDIATELY when the session is offline or
  //     unknown (session_alive:false → watcher must exit 0; any queued mail
  //     is deliberately NOT claimed so a resumed session still gets it at
  //     its first turn boundary). For a LIVE session the poll always holds —
  //     even at pending:0, because mail can arrive for an idle session at
  //     any time — and this is sent when hold_ms (default and max 25 s)
  //     lapses with no mail; the watcher keeps polling while session_alive
  //     is true. `pending` counts pending FREEFORM questions only
  //     (informational in v2 — no longer a watcher exit condition).
  //
  //   Waiter nudges fire on ANY mail insert and on SessionEnd (derive.mjs
  //   mail() / hookSessionEnd → notifyWatchers). Nudges carry no payload —
  //   the poll re-runs its own claim attempt. Permission/elicitation/choice
  //   answers still never resolve a watch: they ride the held hook response
  //   and never become mail.
  //
  //   Races: mailbox drained first → delivered_at already set → the poll's
  //   claim finds nothing and the hold simply lapses to idle. Watcher socket
  //   gone → 'close' unregisters the waiter, nothing claimed. A claim whose
  //   response the watcher never reads is no longer a loss window (BUG-034):
  //   without the ack the lease lapses and the mail comes back.
  //
  //   BUG-105: the watcher sends its per-process generation token as `wg`.
  //   Registration (newest wins, mirroring the client's pidfile) and every
  //   claim attempt run synchronously on the daemon's only thread, so a
  //   SUPERSEDED watcher's in-flight poll can no longer claim mail out from
  //   under its successor: once the newer poll registers its token, the older
  //   request's claim attempt fails the generation check and it lapses to
  //   idle (the mail stays queued for the current generation to claim). No
  //   `wg` (a hand-rolled poll, an older watcher) claims exactly as before.
  function watchHook(_req: HttpReqShim, res: HttpResShim, url: URL) {
    const sid = url.searchParams.get('session') ?? '';
    if (quiescing) {
      json(res, 200, { status: 'idle', session_alive: false, pending: 0 });
      return;
    }
    const holdRaw = Number(url.searchParams.get('hold_ms'));
    const holdMs = Number.isFinite(holdRaw) ? Math.max(0, Math.min(holdRaw, 25_000)) : 25_000;
    // Empty `wg=` MUST collapse to null, not stay '': claimMail treats a
    // non-null gen as a generation to verify (`gen !== null && !isWatchGen`),
    // so '' would fail the check and refuse to claim, while null claims freely.
    // `?? null` would keep '' and silently break mail delivery — keep the
    // truthiness fold as an explicit ternary (see ts-migration-bugs).
    const wgParam = url.searchParams.get('wg');
    const wg = wgParam === '' ? null : wgParam;
    if (wg) core.registerWatchGen(sid, wg); // newest wins; before any claim attempt

    const attempt = () => {
      const info = core.watchInfo(sid);
      if (!info.session_alive) return { status: 'idle', ...info };
      const claimed = core.claimMail(sid, wg);
      if (claimed) return { status: 'mail', ...claimed };
      return null; // session alive, no claimable mail (or stale generation) → hold
    };

    const immediate = attempt();
    if (immediate) {
      json(res, 200, immediate);
      return;
    }

    let settled = false;
    let unregister = () => {
      /* no-op until addWatchWaiter below returns the real unregister */
    };
    let timer: ReturnType<typeof setTimeout> | null = null;
    const finish = (obj: unknown) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      unregister();
      activeWatchClosers.delete(closeForShutdown);
      try {
        json(res, 200, obj);
      } catch {
        /* socket gone */
      }
    };
    const closeForShutdown = () => {
      finish({ status: 'idle', session_alive: false, pending: 0 });
    };
    activeWatchClosers.add(closeForShutdown);
    timer = setTimeout(() => {
      finish({ status: 'idle', ...core.watchInfo(sid) });
    }, holdMs);
    timer.unref();
    unregister = core.addWatchWaiter(sid, () => {
      if (settled || res.writableEnded || res.destroyed) return;
      const out = attempt();
      if (out) finish(out);
    });
    res.on('close', () => {
      settled = true;
      if (timer) clearTimeout(timer);
      unregister();
      activeWatchClosers.delete(closeForShutdown);
    });
    // response intentionally left open
  }

  // SURFACE CONTRACT: the static shell — index.html and the hashed /assets/*
  // bundle — is served to anyone who asks. Everything that carries fleet data
  // or DOES something (/state, /health, /api/*, hooks, mail, both WebSockets)
  // stays behind the token.
  //
  // This is not a softening; gating the shell simply does not work, and the
  // failure is invisible from loopback:
  //   - A browser cannot attach `?t=` or an Authorization header to the
  //     `<script type="module">` tag inside a page it is already loading. Gate
  //     the assets and `/?t=<token>` returns HTML whose own script 401s — a
  //     blank board for the one person the feature exists for.
  //   - Rewriting the token into asset URLs does not save it either: the
  //     terminal modal is a LAZY chunk, imported at click time by code we do
  //     not get to touch, and that fetch would carry no token.
  // The shell is an empty React app that knows how to ask for a key — no
  // session data, no callsigns, no token. A stranger on the network gets that
  // gate page and nothing else; every byte of fleet data still costs the token.
  //
  // Deliberately NOT a cookie: cookies ride along automatically, so any web
  // page you happened to visit could make your browser POST /api/spawn at this
  // board (CSRF) and get a live agent on your machine. A bearer token cannot be
  // forged that way. See tests/lan-auth.test.mjs — the browser-reachability of
  // the shell is pinned there precisely so this never regresses into a blank
  // page again. The exact predicate is http-policy.isPublicShell.

  // The audited router, verbatim from the node:http era. It runs synchronously
  // over the (req, res) shims; the Bun.serve `fetch` handler below constructs the
  // shims, invokes this, then pumps the request body into it. No top-level await —
  // every route resolves through res.writeHead/end (which resolve res.done).
  function routeRequest(req: HttpReqShim, res: HttpResShim): void {
    try {
      const url = new URL(req.url ?? '/', `http://127.0.0.1:${port}`);
      const shell = isPublicShell(req.method, url.pathname);
      // Hook paths are NOT refused here: a tokenless hook is silently answered
      // (and refused) only AFTER its body is parsed, because the board-side
      // legacy-session diagnostic needs the session_id. The hook dialect always
      // receives HTTP 200 `{}`; everything else 401s as usual.
      const isHookPath = url.pathname.startsWith('/hook/');
      if (!shell && !isHookPath && !authorized(req, url)) {
        json(res, 401, { ok: false, reason: 'unauthorized' });
        return;
      }
      const hookAuthed = isHookPath ? authorized(req, url) : true;
      // DNS-REBINDING DEFENSE (C1/H-S3): a page pointed at a domain that
      // re-resolves to this box arrives with a foreign Host — refuse it on every
      // route that carries data or DOES something. The data-free public shell
      // stays open (a browser must load it before it can present the token). A
      // hook keeps its fail-open dialect so an odd proxy can never wedge a
      // real session; a genuine loopback hook sends a loopback Host and is fine.
      if (!shell && !hostHeaderOk(req)) {
        if (url.pathname.startsWith('/hook/')) json(res, 200, {});
        else json(res, 403, { ok: false, reason: 'forbidden' });
        return;
      }
      if (req.method === 'GET') {
        // CSRF WALL for MUTATING GETs (C1/R1-1). Method is not the boundary —
        // state change is. GET /mail DRAINS a mailbox (marks its rows delivered)
        // and GET /api/watch CLAIMS mail (sets delivered_at); a page on another
        // site can fire a simple `fetch('http://127.0.0.1:PORT/mail?session=X')`
        // — an Origin-bearing request that needs no CORS preflight — and drain a
        // session's mail cross-site. So these two GETs get the exact same
        // Origin/Sec-Fetch-Site verdict as a state-changing POST. A genuine
        // fleet-watch/CLI caller sends NO Origin and sails through. The read-only
        // GETs (/state, /health, /api/worktrees) and the public shell do not
        // mutate and stay open; /state's data exposure is already walled by the
        // Host allowlist (hostHeaderOk above), the DNS-rebinding defense.
        if ((url.pathname === '/mail' || url.pathname === '/api/watch') && crossSiteReason(req)) {
          json(res, 403, { ok: false, reason: 'forbidden' });
          return;
        }
        if (url.pathname === '/health') {
          // P6.4: /health now runs as an Effect workflow through the ingress
          // bridge when program.ts has wired it (else the legacy handler). The
          // frozen body and every rider — spawn capability (v1.2: the board hides
          // spawn UI when unavailable), `managed` (the SessionStart hook reads it
          // before deciding to evict us), auth: termAuth (BUG-186, the /ws/term
          // capability), and startup reconciliation readiness (BUG-066: /health
          // answering 200 is NOT proof the heals ran; 'settled' flips only when
          // both do) — all live in healthCapabilities()/legacyHealthResponse and
          // the workflow in app/http-workflows/health-state.ts. See dispatchHealth.
          dispatchHealth(res);
          return;
        }
        if (url.pathname === '/state') {
          dispatchState(res);
          return;
        }
        if (url.pathname === '/api/settings') {
          json(res, 200, { ok: true, settings: core.resolveSettings() });
          return;
        }
        if (url.pathname === '/api/worktrees') {
          // Inspector failures are represented per row as verdict:unknown; one
          // broken repository must never turn this fleet-wide view into a 500 or
          // hide the other worktrees from the human. The fail-soft fold now lives
          // inside worktreesSnapshotWorkflow; dispatchWorktrees settles it through
          // the never-500 soft-read settler, or replays this legacy handler when
          // the Effect bridge is unwired.
          dispatchWorktrees(res);
          return;
        }
        const sessionFsMatch = /^\/api\/sessions\/([^/]+)\/fs\/(list|read|search)$/.exec(
          url.pathname,
        );
        if (sessionFsMatch) {
          const sid = decodeURIComponent(sessionFsMatch[1] ?? '');
          const action = sessionFsMatch[2];
          const operation =
            action === 'list'
              ? core.fsList(sid, url.searchParams.get('path') ?? '')
              : action === 'read'
                ? core.fsRead(sid, url.searchParams.get('path') ?? '')
                : core.fsSearch(sid, url.searchParams.get('q') ?? '', {
                    mode: url.searchParams.get('mode') ?? 'content',
                  });
          settleFilesystemOperation(res, 'session', operation);
          return;
        }
        const homeFsMatch = /^\/api\/fs\/(list|read|search)$/.exec(url.pathname);
        if (homeFsMatch) {
          const action = homeFsMatch[1];
          const operation =
            action === 'list'
              ? core.fsListHome(url.searchParams.get('path') ?? '')
              : action === 'read'
                ? core.fsReadHome(url.searchParams.get('path') ?? '')
                : core.fsSearchHome(url.searchParams.get('q') ?? '', {
                    mode: url.searchParams.get('mode') ?? 'content',
                  });
          settleFilesystemOperation(res, 'home', operation);
          return;
        }
        if (url.pathname === '/mail') {
          const sid = url.searchParams.get('session') ?? '';
          // BUG-034: the poll ACKNOWLEDGES the mail it already holds. Rows it
          // names are finalized (their leases were live when it drained them);
          // acking anything else is a guarded no-op (a final response never
          // carries a stale backlog — every poll acks what IT drained).
          const ackIds = url.searchParams.get('ack') ?? '';
          if (ackIds) core.ackMail(ackIds.split(',').map(Number));
          // The new drain is LEASED: the board must hand the ids back as
          // ack_mail_ids on its next poll. A poll whose response never
          // reached the board leaves the rows leased, not delivered — the
          // retention sweep releases the lease and the mail is re-delivered.
          const box = core.drainMail(sid, { lease: true });
          if (box.length) broadcast();
          json(res, 200, { mail: box, ack_mail_ids: box.map((m) => m.id) });
          return;
        }
        if (url.pathname === '/api/watch') {
          watchHook(req, res, url);
          return;
        } // F3d-2 long-poll
        if (url.pathname === '/favicon.ico') {
          // BUG-123: the shell's favicon is a data: SVG (see CSP_SHELL), so
          // board-dist ships no favicon.ico — but browsers auto-fetch this path
          // and isPublicShell advertises it. Answer it HERE, before the shell
          // serve below (which classifies /favicon.ico as shell and would 404 it
          // through serveBoardAsset's notFound). 204 + no-store: no icon today,
          // and no stale negative cache the day one ships.
          res.writeHead(204, { 'cache-control': 'no-store' });
          res.end();
          return;
        }
        if (shell) {
          // built React board (Phase 5) from board-dist — every path the auth
          // layer declared a public shell (/, /index.html, /assets/*) must
          // actually be SERVED as one, so a bookmark/proxy/health-check that
          // asks for /index.html explicitly gets the same document as /
          // (BUG-124/192). /favicon.ico is handled above; any other missing
          // file still 404s via the notFound callback.
          serveBoardAsset(res, url.pathname, () => json(res, 404, { err: 'nope' }));
          return;
        }
        json(res, 404, { err: 'nope' });
        return;
      }

      if (req.method === 'POST') {
        const isHook = url.pathname.startsWith('/hook/');
        // CSRF WALL (C1): a state-changing request driven from another origin is
        // refused before a byte of its body is read. Real CLI hooks send no
        // Origin and a loopback Host, so they pass untouched; a browser on
        // another site is turned away. A refused hook still answers in the
        // fail-open dialect so it can never break a session.
        if (crossSiteReason(req)) {
          if (isHook) json(res, 200, {});
          else json(res, 403, { ok: false, reason: 'forbidden' });
          return;
        }
        // CONTENT-TYPE WALL (C1): control POSTs must declare JSON — which also
        // forces a CORS preflight for any cross-origin attempt, a second wall in
        // front of /api/spawn et al. Hooks are EXEMPT per the hook contract: a
        // hook with an odd/absent content-type is still processed (fail open).
        if (!isHook && !isJsonContentType(req.headers['content-type'])) {
          json(res, 415, { ok: false, reason: 'expected application/json' });
          return;
        }
        // M-B3: collect raw Buffers, cap by BYTES, decode ONCE. `body += d`
        // stringified each TCP chunk independently — a multibyte glyph straddling
        // a chunk boundary decoded to U+FFFD — and `body.length` counted UTF-16
        // units, not bytes. Concatenating the bytes and decoding the whole once
        // is byte-exact.
        const chunks: Buffer[] = [];
        let size = 0;
        let tooLarge = false;
        const bodyCap = url.pathname === '/api/paste-image' ? MAX_PASTE_BODY : MAX_BODY;
        // An oversized body is answered ONCE, then its remaining bytes are DRAINED
        // (not abandoned): Bun.serve reuses keep-alive sockets, so leaving the unread
        // body in the pipe makes the next request's bytes append to the abandoned
        // stream and desync the peer (it reads a bodyless 400, or nothing). So we do
        // NOT tear the request down here — the fetch handler's drainThenRespond reads
        // the rest of this body to 'end' before handing Bun the response (the
        // 'data'/'end' listeners discard once tooLarge). shouldKeepAlive=false arms a
        // ~4s per-request timeout so a client that DECLARES a huge body then WITHHOLDS
        // it gets its socket FIN'd instead of parking the drain forever (Bun has no
        // per-socket close; see bun-serve-runtime-limits). On that stall the drain
        // never completes, so drainThenRespond's BODY_DRAIN_GRACE_MS (< the ~4s FIN)
        // is what puts the 413 on the wire before the socket closes.
        const refuseOversize = () => {
          res.shouldKeepAlive = false;
          if (isHook) json(res, 200, {});
          else json(res, 413, { ok: false, reason: 'payload too large' });
        };
        // Refuse an oversized body by its declared Content-Length before reading
        // a byte — the streaming cap below still catches a lying/absent header,
        // but this avoids buffering megabytes only to reject them.
        const declared = Number(req.headers['content-length']);
        if (Number.isFinite(declared) && declared > bodyCap) {
          refuseOversize();
          return;
        }
        req.on('data', (d: Buffer) => {
          if (tooLarge) return;
          size += d.length; // d is a Buffer — byte length, not char count
          if (size > bodyCap) {
            tooLarge = true;
            // 413 on control paths; hooks keep the fail-open 200 {}. Stop
            // accumulating either way so the body can't grow without bound.
            refuseOversize();
            return;
          }
          chunks.push(d);
        });
        req.on('end', () => {
          if (tooLarge) return;
          const body = Buffer.concat(chunks).toString('utf8');
          let ev: unknown;
          try {
            ev = JSON.parse(body || '{}');
          } catch {
            // hooks fail open: a bad body on a hook path is still 200 {}
            if (isHook) json(res, 200, {});
            else json(res, 400, { err: 'bad json' });
            return;
          }
          try {
            const hook = /^\/hook\/([A-Za-z]+)$/.exec(url.pathname);
            if (hook) {
              const name = hook[1] ?? '';
              // A tokenless/wrong-token hook is REFUSED here — nothing below may
              // ingest, hold, or derive from it — and answered with the canonical
              // silent no-op. Diagnostics stay on the board, never in Claude.
              if (!hookAuthed) {
                silentHookRefusal(res, ev);
                return;
              }
              noteUpgradedSession(asRecord(ev)['session_id']);
              // payload capture (validation aid): first 3 raw payloads per
              // hook event name, best-effort, never affects the response
              try {
                capture(name, ev);
              } catch {
                /* best-effort */
              }
              // F3c CRITICAL (validated live on CLI 2.1.206):
              // AskUserQuestion rides the permission machinery — after the
              // /hook/AskUserQuestion hold resolves {}, the CLI fires
              // PermissionRequest for the SAME tool call. NEVER hold that
              // one: an unanswered question would chain two full hold
              // windows (~50 s each) before the terminal user ever sees the
              // chooser. Ingest telemetry, answer {} immediately.
              if (name === 'PermissionRequest' && asRecord(ev)['tool_name'] === 'AskUserQuestion') {
                core.applyEvent({
                  ...asRecord(ev),
                  hook_event_name: 'PermissionRequest',
                });
                json(res, 200, {});
                return;
              }
              if (
                name === 'PermissionRequest' ||
                name === 'Elicitation' ||
                name === 'AskUserQuestion'
              ) {
                holdHook(res, ev, name);
                return; // Phase 3/4 hold-open relay
              }
              // P6.4: the unknown / malformed-payload / dispatch decision now
              // lives in the hook workflow (app/http-workflows/hooks.ts), run
              // through the P6.3 bridge and settled FAIL-OPEN by
              // settleEffectHookRoute (mapHookExit: every non-success Exit → 200
              // {}). The malformed-payload guard that formerly stood here — a
              // missing/blank session_id would key the events.mjs card on the
              // literal 'unknown', collapsing every malformed payload into one
              // shared phantom card — is now caps.valid() (the shared
              // contracts/hooks.ts validator), applied inside the workflow with the
              // identical predicate, so no dispatch outcome moves. dispatchHook's
              // rollback path reproduces the former inline branches byte-for-byte.
              dispatchHook(res, name, ev);
              return;
            }
            // BUG-034: explicit acknowledgement for a leased /api/watch claim.
            // The watcher POSTs {mail_id} once it HOLDS the claimed body (a
            // claim whose response never arrived never acks, so the lease
            // lapses and the mail is re-delivered instead of lost).
            if (url.pathname === '/mail/ack') {
              const out = core.ackMail([(ev as { mail_id?: unknown }).mail_id]);
              json(res, 200, { ok: true, ...out });
              return;
            }
            if (url.pathname === '/mail') {
              dispatchMail(res, ev);
              return;
            }
            if (url.pathname === '/api/cleanup') {
              dispatchCleanup(res);
              return;
            }
            if (url.pathname === '/api/worktrees/remove') {
              // Security and data-loss gates live together in derive: only a
              // spawn-owned path reaches git, and force is an exact boolean.
              core
                .removeWorktree(ev as Parameters<typeof core.removeWorktree>[0])
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd worktree removal error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            if (url.pathname === '/api/settings') {
              // gateway_* writes reroute every future session's LLM traffic and can
              // leak the gateway credential, so they keep requiring the bearer even
              // when everything else is waived. The ONLY waiver is the explicit
              // single-user trust-loopback opt-out, and it keys off the real peer
              // address (isLoopbackAddress) rather than any header — a direct
              // loopback caller can forge Host/Origin to look proxied, so we must
              // not waive this gate on arrivedViaTrustedProxy(). Proxy token mode,
              // proxy trust mode, and LAN never inherit the waiver here.
              if (gatewaySettingsTouched(ev)) {
                const bearer = parseBearer(req.headers.authorization);
                const bearerWaived =
                  trustLoopback &&
                  !arrivedViaTrustedProxy(req) &&
                  isLoopbackAddress(req.socket.remoteAddress);
                if (
                  !bearerWaived &&
                  !tokenMatches(bearer) &&
                  !tokenMatches(url.searchParams.get('t'))
                ) {
                  json(res, 401, {
                    ok: false,
                    reason: 'gateway settings require the bearer token',
                  });
                  return;
                }
                logExec(url.pathname, req, ' gateway=true');
              }
              dispatchSettings(res, ev);
              return;
            }
            if (url.pathname === '/command') {
              dispatchCommand(res, ev);
              return;
            }
            if (url.pathname === '/api/paste-image') {
              // v1.7 pasted image → file (paste.mjs). Same wall stack as every
              // control POST (auth → Host → CSRF → json content-type → body
              // cap); only the body cap is per-route (see MAX_PASTE_BODY). The
              // returned path is TYPED into the pane by the BOARD, not by us —
              // injection must ride TermPane's sendIn gate so the grid's
              // one-tile-types discipline also governs pastes.
              // P6.4: Effect workflow when wired; legacy handler is the rollback
              // seam. MUTATING: intra-quiesce answers the frozen shutdown 503
              // and never replays the write. See dispatchPasteImage.
              dispatchPasteImage(res, ev);
              return;
            }
            if (url.pathname === '/api/spawn/arm-unsupervised') {
              // 0.16.0: mint the one-time capability an unsupervised spawn body
              // must echo. Token-gated by tokenGatedRoute even on loopback, so
              // this route existing means the caller already proved it holds
              // the bearer — the API-side half of the board's red two-step.
              // P9.1 Slice 0: Effect workflow when wired; the legacy synchronous
              // handler below is the rollback seam. SYNC + MUTATING (mints a
              // single-use token), so it rides settleEffectMutatingRoute: an
              // intra-quiesce admission refusal answers the frozen 503 and never
              // replays the mint, and a defect reproduces the outer-catch 500
              // {"err":"internal"} the legacy throw already lands in.
              logExec(url.pathname, req);
              if (effectRoutes) {
                settleEffectMutatingRoute(
                  effectRoutes,
                  'POST /api/spawn/arm-unsupervised',
                  effectRoutes.armUnsupervised({
                    run: () => core.armUnsupervised() as string,
                  }),
                  res,
                  CONTROL_DEFECT,
                );
                return;
              }
              json(res, 200, { ok: true, arm_token: core.armUnsupervised() });
              return;
            }
            if (url.pathname === '/api/repos/preflight') {
              const body = asRecord(ev);
              const preflightError = repoPreflightBodyError(body);
              if (preflightError) {
                json(res, 400, { ok: false, reason: preflightError });
                return;
              }
              logExec(url.pathname, req);
              // P9.2 Slice 2: Effect workflow when wired; the legacy async handler
              // below is the rollback seam (effectRoutes unset → installEffectRoutes
              // not called). ASYNC — it folds a core rejection to 500
              // {ok:false,reason:'Git access check failed internally'} (its OWN 500
              // dialect, not controlAsync's {reason:'internal'}), so it rides the
              // dedicated settleEffectPreflightRoute (start-once witness +
              // JOIN-on-interrupt + quiesce 503), NOT settleControlAsyncRoute. The
              // body-validation 400 wall above runs BEFORE dispatch on BOTH paths
              // (DANGER §4.8), so repoPreflightBodyError never reaches the workflow.
              if (effectRoutes) {
                settleEffectPreflightRoute(effectRoutes, 'POST /api/repos/preflight', res, () =>
                  core.preflightRepo({
                    repo: body['repo'] as string,
                    repo_host: (body['repo_host'] as string | undefined) ?? null,
                    repo_transport: (body['repo_transport'] as string | undefined) ?? null,
                    repo_org: (body['repo_org'] as string | undefined) ?? null,
                  }),
                );
                return;
              }
              core
                .preflightRepo({
                  repo: body['repo'] as string,
                  repo_host: (body['repo_host'] as string | undefined) ?? null,
                  repo_transport: (body['repo_transport'] as string | undefined) ?? null,
                  repo_org: (body['repo_org'] as string | undefined) ?? null,
                })
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd repo preflight error:', err);
                  json(res, 500, { ok: false, reason: 'Git access check failed internally' });
                });
              return;
            }
            if (url.pathname === '/api/spawn') {
              // F1a structural gate: reject a body that isn't even a JSON
              // object before it reaches derive — the one shape spawns.mjs
              // cannot parse. Every real spawn request is an object (and a
              // tokenless one 401s upstream), so this moves no existing
              // outcome; it just hands a non-object a clean 400 instead of a
              // derive-internal throw. Deep field validation (kind XOR, enums,
              // plan_id positivity) stays in derive.spawn until Phase 5 folds
              // it into a single typed pass against SpawnRequest.
              if (!validateSpawnRequest(ev).ok) {
                json(res, 400, { ok: false, reason: 'spawn body must be a JSON object' });
                return;
              }
              // v1.2 board spawn (CONTRACT). Control API like the questions
              // answer path: real status codes, fail-loud — never a silent
              // no-op. The whole flow (validate → card → worktree → tmux →
              // row → nudge) lives in derive.mjs. v1.3 adds
              // dangerously_skip_permissions: bool and permission_mode
              // "bypassPermissions" (validated/applied in derive.spawn too).
              // BUG-040: plan_id on the body claims that plan's execution
              // atomically BEFORE launch (see derive.spawn).
              const spawnEv = asRecord(ev);
              const spawnUnsupervised = isUnsupervisedRequest(ev);
              const spawnPlanId = spawnEv['plan_id'];
              // plan_id is contractually a scalar row id; this is a cosmetic log
              // suffix only (core.spawn still receives the raw ev). Guard to a
              // stringifiable primitive so a malformed object body can't stringify
              // to '[object Object]' here (see ts-migration-bugs).
              const spawnPlanSuffix =
                typeof spawnPlanId === 'string' || typeof spawnPlanId === 'number'
                  ? ` plan=${spawnPlanId}`
                  : '';
              logExec(
                url.pathname,
                req,
                `${spawnUnsupervised ? ' unsupervised=true' : ' unsupervised=false'}${spawnPlanSuffix}`,
              );
              // P9.1 Slice 6a: /api/spawn under the P6.4 transport. The core is
              // UNCHANGED this slice (that is Slice 6b) — the capability is the raw
              // native ownedSpawn Promise, wrapped once by the start-once recorder
              // so closeClients can JOIN an in-flight spawn on shutdown. The
              // dedicated spawn settler reproduces the legacy .then(json) success
              // relay AND the redacted 500 spawnFailureReason dialect on a
              // die/joined-rejection (D6); the legacy handler below stays the
              // rollback seam (effectRoutes unset → installEffectRoutes not called).
              // The validateSpawnRequest wall above is transport-level and precedes
              // BOTH paths, exactly as it did the legacy dispatch.
              if (effectRoutes) {
                settleEffectSpawnRoute(
                  effectRoutes,
                  'POST /api/spawn',
                  res,
                  () => core.spawn(ev) as ControlResult,
                );
                return;
              }
              (core.spawn(ev) as ControlResult)
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd spawn error:', err);
                  // UX 2.3 option 4 — a spawn that escapes derive with a THROW
                  // (not a classified {status, body}) used to answer bare
                  // 'internal', the one spawn failure that said nothing at all.
                  // spawnFailureReason bounds it to one redacted line — the
                  // same register as a card note, message-only, never a stack.
                  // A failure after the 202 was handed out never reaches here:
                  // it lands in the detached chain's catch, which logs and
                  // tombstones instead (spawns.mjs), and this json() then
                  // harmlessly no-ops on the ended response.
                  json(res, 500, { ok: false, reason: spawnFailureReason(err) });
                });
              return;
            }
            const killMatch = /^\/api\/spawn\/([A-Za-z0-9-]+)\/kill$/.exec(url.pathname);
            if (killMatch) {
              // v1.2 name-verified kill: 404 unknown id, 409 card not offline
              // without force:true, 410 window already gone.
              logExec(url.pathname, req);
              if (effectRoutes) {
                settleControlAsyncRoute(
                  effectRoutes,
                  'POST /api/spawn/:id/kill',
                  res,
                  () =>
                    core.spawnKill(
                      killMatch[1] ?? '',
                      asRecord(ev)['force'] === true,
                    ) as ControlResult,
                  'fleetd spawn kill error:',
                );
                return;
              }
              (core.spawnKill(killMatch[1] ?? '', asRecord(ev)['force'] === true) as ControlResult)
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd spawn kill error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const reviveMatch = /^\/api\/spawn\/([A-Za-z0-9-]+)\/revive$/.exec(url.pathname);
            if (reviveMatch) {
              // Terminal spawn rows can be resumed only when their durable
              // cwd/transcript evidence still exists; derive owns every
              // collision/cap check and returns the control-API status. The
              // body may override remote_control (default: inherit).
              logExec(url.pathname, req);
              if (effectRoutes) {
                settleControlAsyncRoute(
                  effectRoutes,
                  'POST /api/spawn/:id/revive',
                  res,
                  () => core.revive(reviveMatch[1] ?? '', ev ?? {}) as ControlResult,
                  'fleetd spawn revive error:',
                );
                return;
              }
              (core.revive(reviveMatch[1] ?? '', ev ?? {}) as ControlResult)
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd spawn revive error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const adoptMatch = /^\/api\/sessions\/([^/]+)\/adopt$/.exec(url.pathname);
            if (adoptMatch) {
              // 0.7.0 "Move to tmux": adopt a session the board did NOT spawn
              // into a board-owned `claude --resume` pane. Context-sensitive —
              // derive arms a live session (auto-adopts on its SessionEnd) and
              // adopts an ended one now; the body may carry
              // dangerously_skip_permissions:bool or {disarm:true}. Every guard
              // (404/400/409/410) lives in derive; the CSRF/Host walls above
              // apply automatically like every other control POST.
              const adoptUnsupervised = isUnsupervisedRequest(ev);
              logExec(
                url.pathname,
                req,
                adoptUnsupervised ? ' unsupervised=true' : ' unsupervised=false',
              );
              // adoptSession's ctx surface (derive.ts) is spelled out narrowly for
              // events/retention (opts pinned to {dangerously_skip_permissions}, meta
              // required, result defensively | null | undefined). The real runtime
              // signature is (session_id, body: SpawnBody = {}, {deferred} = {}) and
              // always resolves a concrete {status, body}; re-assert it at this seam.
              if (effectRoutes) {
                settleControlAsyncRoute(
                  effectRoutes,
                  'POST /api/sessions/:sid/adopt',
                  res,
                  () =>
                    (
                      core.adoptSession as (
                        sid: string,
                        body?: unknown,
                        meta?: { deferred?: boolean },
                      ) => ControlResult
                    )(adoptMatch[1] ?? '', ev ?? {}),
                  'fleetd adopt error:',
                );
                return;
              }
              (
                core.adoptSession as (
                  sid: string,
                  body?: unknown,
                  meta?: { deferred?: boolean },
                ) => ControlResult
              )(adoptMatch[1] ?? '', ev ?? {})
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd adopt error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const nameMatch = /^\/api\/sessions\/([^/]+)\/name$/.exec(url.pathname);
            if (nameMatch) {
              // 0.7.1 custom names: rename a card's SUFFIX (the animal is never
              // the human's to choose). {suffix:"docs-review"} renames;
              // {clear:true} reverts to the automatic name (the ticket name if
              // the card has a ticket, else the birth <animal>-<sid4>). Same
              // core write as the `name` orchestrator command, so both surfaces
              // enforce one set of rules.
              const body = asRecord(ev);
              const clearing = body['clear'] === true;
              if (effectRoutes) {
                settleEffectMutatingRoute(
                  effectRoutes,
                  'POST /api/sessions/:sid/name',
                  effectRoutes.nameControl({
                    clearing,
                    suffix: body['suffix'],
                    validateSuffix: validateNameSuffix,
                    applyName: (suffix) => core.applyCustomName(nameMatch[1] ?? '', suffix),
                  }),
                  res,
                  CONTROL_DEFECT,
                );
                return;
              }
              if (!clearing && typeof body['suffix'] !== 'string') {
                json(res, 400, {
                  ok: false,
                  reason: 'suffix must be a string (or pass {clear:true})',
                });
                return;
              }
              if (!clearing) {
                // suffix is a string here — the typeof guard above 400s otherwise.
                const bad = validateNameSuffix(body['suffix'] as string);
                if (bad) {
                  json(res, 400, { ok: false, reason: bad });
                  return;
                }
              }
              const out = core.applyCustomName(
                nameMatch[1] ?? '',
                clearing ? null : (body['suffix'] as string),
              );
              json(res, out.ok ? 200 : 409, out);
              return;
            }
            const sessionDismissMatch = /^\/api\/sessions\/([^/]+)\/dismiss$/.exec(url.pathname);
            if (sessionDismissMatch) {
              // Item 3 "per-card dismiss": retire ONE offline card now, instead
              // of waiting for 24h retention or the bulk Clear that archives
              // every offline card at once. Every guard (404 unknown / 409 not
              // offline / 409 already dismissed / 409 stalled spawn) lives in
              // derive; the CSRF/Host walls above apply like any control POST.
              logExec(url.pathname, req);
              if (effectRoutes) {
                settleControlAsyncRoute(
                  effectRoutes,
                  'POST /api/sessions/:sid/dismiss',
                  res,
                  () => core.dismissSession(sessionDismissMatch[1] ?? ''),
                  'fleetd dismiss error:',
                );
                return;
              }
              core
                .dismissSession(sessionDismissMatch[1] ?? '')
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd dismiss error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            // BUG-145 retry path: a dismiss whose window-kill phase failed
            // returns retry:true; this POST re-attempts ONLY the dead-window
            // kills for that already-archived card (idempotent).
            const dismissRetryMatch = /^\/api\/sessions\/([^/]+)\/dismiss\/retry$/.exec(
              url.pathname,
            );
            if (dismissRetryMatch) {
              logExec(url.pathname, req);
              if (effectRoutes) {
                settleControlAsyncRoute(
                  effectRoutes,
                  'POST /api/sessions/:sid/dismiss/retry',
                  res,
                  () => core.dismissRetry(dismissRetryMatch[1] ?? ''),
                  'fleetd dismiss-retry error:',
                );
                return;
              }
              core
                .dismissRetry(dismissRetryMatch[1] ?? '')
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd dismiss-retry error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const rcMatch = /^\/api\/spawn\/([A-Za-z0-9-]+)\/rc$/.exec(url.pathname);
            if (rcMatch) {
              // Explicit human board action: derive enforces the idle/live
              // pane boundary, types /rc literally, and waits for harvesting.
              logExec(url.pathname, req);
              if (effectRoutes) {
                settleControlAsyncRoute(
                  effectRoutes,
                  'POST /api/spawn/:id/rc',
                  res,
                  () => core.enableRemote(rcMatch[1] ?? '') as ControlResult,
                  'fleetd remote-control error:',
                );
                return;
              }
              (core.enableRemote(rcMatch[1] ?? '') as ControlResult)
                .then((out) => {
                  json(res, out.status, out.body);
                })
                .catch((err: unknown) => {
                  console.error('fleetd remote-control error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const answerMatch = /^\/api\/questions\/(\d+)\/answer$/.exec(url.pathname);
            if (answerMatch) {
              // Board answer API (F3). NOT a hook path — real status codes.
              // v1.3: for an ExitPlanMode plan question the body may also be
              // {behavior:"capture"} (board-only pseudo-behavior) — the
              // branching lives in questions.mjs answer().
              if (effectRoutes) {
                settleEffectMutatingRoute(
                  effectRoutes,
                  'POST /api/questions/:id/answer',
                  effectRoutes.controlSync({
                    run: () =>
                      core.questions.answer(
                        Number(answerMatch[1] ?? ''),
                        ev as Parameters<typeof core.questions.answer>[1],
                      ),
                  }),
                  res,
                  CONTROL_DEFECT,
                );
                return;
              }
              const out = core.questions.answer(
                Number(answerMatch[1] ?? ''),
                ev as Parameters<typeof core.questions.answer>[1],
              );
              json(res, out.status, out.body);
              return;
            }
            const dismissMatch = /^\/api\/questions\/(\d+)\/dismiss$/.exec(url.pathname);
            if (dismissMatch) {
              // "I already handled this in the terminal." Retires the card and
              // tells the session NOTHING — unlike answer(), which mails it.
              if (effectRoutes) {
                settleEffectMutatingRoute(
                  effectRoutes,
                  'POST /api/questions/:id/dismiss',
                  effectRoutes.questionsDismiss({
                    run: () => core.questions.dismiss(Number(dismissMatch[1])),
                  }),
                  res,
                  CONTROL_DEFECT,
                );
                return;
              }
              const out = core.questions.dismiss(Number(dismissMatch[1]));
              json(res, out.ok ? 200 : 404, out);
              return;
            }
            const planMatch = /^\/api\/plans\/(\d+)\/mark$/.exec(url.pathname);
            if (planMatch) {
              // v1.3 plan library mark (CONTRACT): {status:"executed"|"archived",
              // via?} — 404 unknown id, 409 bad transition. Matrix documented
              // at core.planMark (derive.mjs).
              if (effectRoutes) {
                settleEffectMutatingRoute(
                  effectRoutes,
                  'POST /api/plans/:id/mark',
                  effectRoutes.controlSync({
                    run: () =>
                      core.planMark(
                        Number(planMatch[1] ?? ''),
                        ev as Parameters<typeof core.planMark>[1],
                      ),
                  }),
                  res,
                  CONTROL_DEFECT,
                );
                return;
              }
              const out = core.planMark(
                Number(planMatch[1] ?? ''),
                ev as Parameters<typeof core.planMark>[1],
              );
              json(res, out.status, out.body);
              return;
            }
            const assignMatch = /^\/api\/plans\/(\d+)\/assign$/.exec(url.pathname);
            if (assignMatch) {
              // BUG-039: daemon-side plan assignment — {to, instructions?}.
              // The board must send the daemon-reserved [FLEETDECK ASSIGNMENT]
              // frame, which POST /mail 422s, so the daemon composes it here
              // through its internal mail() and marks the plan executed in the
              // same request. 404 unknown plan/target, 409 non-executable plan.
              if (effectRoutes) {
                settleEffectMutatingRoute(
                  effectRoutes,
                  'POST /api/plans/:id/assign',
                  effectRoutes.controlSync({
                    run: () =>
                      core.assignPlan(
                        Number(assignMatch[1] ?? ''),
                        ev as Parameters<typeof core.assignPlan>[1],
                      ),
                  }),
                  res,
                  CONTROL_DEFECT,
                );
                return;
              }
              const out = core.assignPlan(
                Number(assignMatch[1] ?? ''),
                ev as Parameters<typeof core.assignPlan>[1],
              );
              json(res, out.status, out.body);
              return;
            }
            json(res, 404, { err: 'nope' });
            return;
          } catch (err) {
            console.error('fleetd handler error:', err);
            // fail open on hook paths; visible error elsewhere
            if (url.pathname.startsWith('/hook/')) {
              json(res, 200, {});
              return;
            }
            json(res, 500, { err: 'internal' });
            return;
          }
        });
        return;
      }

      json(res, 404, { err: 'nope' });
    } catch (err) {
      console.error('fleetd request error:', err);
      try {
        json(res, (req.url ?? '').startsWith('/hook/') ? 200 : 500, {});
      } catch {
        /* socket gone */
      }
    }
  }

  // ---------------------------------------------------------------- ws
  // Bun-native WebSocket. The two logical servers — snapshot (/ws) and terminal
  // (/ws/term) — are ONE Bun `websocket` handler dispatched on ws.data.kind; each
  // keeps its own client Set (Bun has no wss.clients to iterate). The upgrade
  // auth/CSRF gate lives in the fetch handler's handleUpgrade, before server.upgrade.
  const snapshotClients = new Set<LiveSocket>();
  const termClients = new Set<LiveSocket>();
  // Sockets whose application close has begun but whose native close callback
  // may not have fired yet. They remain transport-owned until stop(false) or
  // stop(true) settles, and are retained here for the post-stop terminate
  // backstop. The live sets retain them too until the native callback/stop so
  // ownedCounts never reports zero while a transport socket is still open;
  // quiescing, not Set membership, is the admission gate.
  const closingSockets = new Set<LiveSocket>();
  // createCore is built before the HTTP surface, so the question relay starts
  // fail-closed and receives this live probe now. Hooks cannot arrive until the
  // returned server is listened, making admission -> row creation -> attachHold
  // one synchronous, non-interleavable path with respect to websocket close.
  core.questions.setBoardConsumerProbe(() => snapshotClients.size > 0);
  const termbridge = createTermBridge({
    port,
    resolveSpawn: (spawnId) => core.terminalSpawn(spawnId),
    log: (message) => {
      console.error(`fleetd ${message}`);
    },
  });
  // M-P1 coalescing: a mutation flips `dirty` and schedules at most one flush
  // per short window, so N updateSession() calls inside one hook collapse to a
  // single snapshot rebuild+stringify+send instead of N.
  let dirty = false;
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  // Waiters parked until the coalesced flush has actually fired. Boot
  // reconciliation settles its heals, then must ALSO let the flush those heals
  // scheduled drain before reporting 'settled' — otherwise a /ws client that
  // connects the instant readiness flips can still be caught by the trailing
  // startup broadcast (BUG-066). whenBroadcastIdle() resolves with no pending
  // flush: immediately when none is scheduled, otherwise when the current one
  // runs.
  let idleWaiters: (() => void)[] = [];
  function whenBroadcastIdle() {
    if (quiescing || !flushTimer) return Promise.resolve();
    return new Promise<void>((resolve) => idleWaiters.push(resolve));
  }
  // H-S1: the broadcast/connect snapshot deliberately uses core.snapshot() and
  // NOT snapshotWithLan() — the token-bearing lan.urls/lan.mdns must never ride
  // a frame a /ws client can read. The share URLs stay on GET /state, which is
  // token-gated in LAN mode (the board reads `lan` from its /state poll).
  // legacy_upgrade is NOT secret (bare session ids + a count) and MUST ride the
  // WS frame: the board treats a live /ws snapshot as authoritative and only
  // preserves `lan` from later /state polls, so without this field the pre-0.16
  // restart banner is wiped as soon as the socket opens and never comes back.
  function wsSnapshot() {
    return assembleSnapshotFrame(core.snapshot(), legacyBanner());
  }
  function broadcast() {
    dirty = false;
    if (quiescing) return;
    if (!snapshotClients.size) return;
    const msg = JSON.stringify(wsSnapshot());
    for (const c of snapshotClients) {
      if (c.readyState !== 1) continue;
      // H-R3/R1-2 backpressure: a peer that stopped draining must not make us
      // buffer snapshot after snapshot into a dead socket until we run out of
      // memory. Past the cap we TERMINATE it rather than skip-and-forget:
      // skipping while clearing `dirty` (below) would drop THIS mutation for a
      // client that later recovers, and the board stops /state polling while its
      // socket is live, so it would never learn of the update. Terminating forces
      // a reconnect, and the connect handler seeds the fresh socket with a full
      // snapshot — correctness over a silent partial board. 'close' unwinds the
      // socket exactly as the keepalive's reap would.
      if (wsBufferEviction(c.getBufferedAmount(), MAX_WS_BUFFER) === 'evict') {
        try {
          c.terminate();
        } catch {
          /* already gone */
        }
        continue;
      }
      c.send(msg);
    }
  }
  // P6.4: LEFT as-is. The broadcast trigger is a single coalescing setTimeout —
  // transport machinery, not an application handler — so it does not go through the
  // ingress bridge: the exit gate needs route workflows for handlers, and bridging a
  // 60 ms flush timer would change nothing but the coalescing window's timing. The
  // per-frame send loop in broadcast() stays synchronous for the same reason; only the
  // PURE decisions inside it (eviction, keepalive, frame shape) are lifted to policy.
  function scheduleBroadcast() {
    if (quiescing) return;
    dirty = true;
    if (flushTimer) return;
    flushTimer = setTimeout(() => {
      flushTimer = null;
      if (dirty) broadcast();
      // Wake anyone waiting for the flush to drain (boot readiness settle).
      const waiters = idleWaiters;
      idleWaiters = [];
      for (const resolve of waiters) resolve();
    }, BROADCAST_COALESCE_MS);
    flushTimer.unref();
  }
  // ONE Bun websocket handler for both logical servers; open/message/close/pong
  // dispatch on ws.data.kind. Because these handlers are registered on the shared
  // handler object, they are LIVE the instant a socket opens — so a close arriving
  // during openTerm's openViewer await is captured by close() (it flips
  // data.abort.closed), preserving the node-era M-R5 open/close race guard.
  const websocket: WebSocketHandler<WsData> = {
    open(ws) {
      if (quiescing) {
        if (ws.data.kind === 'term') ws.data.abort.closed = true;
        try {
          ws.terminate();
        } catch {
          /* already gone */
        }
        return;
      }
      ws.data.isAlive = true;
      if (ws.data.kind === 'snapshot') {
        snapshotClients.add(ws);
        try {
          ws.send(JSON.stringify(wsSnapshot()));
        } catch {
          /* client gone */
        }
        return;
      }
      termClients.add(ws);
      openTerm(ws);
    },
    message(ws, message) {
      if (quiescing) return;
      if (ws.data.kind !== 'term') return;
      const data = ws.data;
      if (!data.handle) return;
      // Bun delivers a text frame as a string and a binary frame as a Buffer; the
      // board only ever sends JSON text. Normalize to a string and bound it by
      // BYTES (M-R4): a terminal frame is a keystroke or a modest paste — never a
      // megabyte. Refuse an oversized frame outright (1009) rather than expand it to
      // hex and queue it; termbridge.input() enforces the queued-byte bound.
      const text = typeof message === 'string' ? message : message.toString('utf8');
      if (Buffer.byteLength(text, 'utf8') > MAX_TERM_FRAME_BYTES) {
        try {
          ws.close(1009, 'input frame too large');
        } catch {
          /* already gone */
        }
        return;
      }
      let frame: unknown;
      try {
        frame = JSON.parse(text);
      } catch {
        return;
      }
      if (!frame || typeof frame !== 'object') return;
      const fr = frame as Record<string, unknown>;
      if (fr['t'] === 'in' && typeof fr['data'] === 'string') data.handle.input(fr['data']);
      else if (fr['t'] === 'paste' && typeof fr['data'] === 'string') data.handle.paste(fr['data']);
      else if (fr['t'] === 'resize') data.handle.resize(fr['cols'] as number, fr['rows'] as number);
    },
    close(ws) {
      if (ws.data.kind === 'snapshot') {
        const removed = snapshotClients.delete(ws);
        // One tab closing must not disturb another tab that can still answer.
        // The 1 -> 0 transition, however, makes every live hold undeliverable:
        // release them immediately so Claude renders its native terminal UI.
        // failOpenAllHolds suppresses re-arm because the terminal now owns each
        // question and there is no board consumer for a successor card.
        if (removed && snapshotClients.size === 0) {
          try {
            core.questions.failOpenAllHolds();
          } catch (err) {
            // The question layer releases responders before persistence work;
            // contain any unexpected hygiene error so websocket cleanup itself
            // cannot destabilize the daemon.
            console.error('fleetd board disconnect hold-release error:', err);
          }
        }
        return;
      }
      termClients.delete(ws);
      // Flip the abort latch and tear the viewer down. If close arrives mid-open
      // (handle still null), openTerm's post-await guard closes the late handle.
      ws.data.abort.closed = true;
      ws.data.handle?.close();
    },
    pong(ws) {
      ws.data.isAlive = true;
    },
  };

  // H-R3/M-P6 backpressure: a viewer that has stopped draining is EVICTED (a 1009
  // close), not fed. Silently dropping pane bytes would desync its screen; closing
  // the socket unwinds its tmux subscription (close() runs handle.close()) so a slow
  // viewer can never buffer a pane's whole output into a dead socket.
  function sendTermFrame(ws: LiveSocket, frame: unknown): void {
    if (quiescing) return;
    if (ws.readyState !== 1) return;
    if (ws.getBufferedAmount() > MAX_TERM_WS_BUFFER) {
      try {
        ws.close(1009, 'terminal viewer too far behind');
      } catch {
        /* already gone */
      }
      return;
    }
    ws.send(JSON.stringify(frame));
  }

  // Open the tmux viewer for a freshly-upgraded /ws/term socket. The spawn/cols/rows
  // were parsed at upgrade time (before the socket existed) and stashed on ws.data.
  function openTerm(ws: LiveSocket): void {
    if (ws.data.kind !== 'term') return;
    const data = ws.data;
    if (quiescing) {
      data.abort.closed = true;
      try {
        ws.terminate();
      } catch {
        /* already gone */
      }
      return;
    }
    // Async work (awaits termbridge.openViewer) runs inside a void-ed IIFE so the
    // caller returns void, not a floating promise. Fully try/caught; never rejects.
    const task = (async () => {
      const send = (frame: unknown) => {
        if (quiescing) return;
        sendTermFrame(ws, frame);
      };
      try {
        const { spawn_id, cols, rows } = data;
        if (!spawn_id) throw new Error('missing spawn id');
        // M-R5 abort path: if the socket closes mid-open (before `handle` exists),
        // openViewer() checks isAborted() between its awaits and bails, so the
        // half-opened viewer is removed instead of lingering counted forever.
        data.handle = await termbridge.openViewer({
          spawn_id,
          cols,
          rows,
          send,
          isAborted: () => data.abort.closed,
          onClose(reason) {
            send({ t: 'exit', reason });
            try {
              ws.close();
            } catch {
              /* already gone */
            }
          },
        });
        if (data.abort.closed || quiescing) data.handle.close();
      } catch (err) {
        if (quiescing) return;
        const e = err as { gone?: unknown; reason?: unknown; message?: unknown } | null;
        if (e?.gone) {
          // The row said live but its pane was already gone (agent ended, tick
          // hasn't reconciled). Report it as an exit ("the agent has ended"), not
          // a scary "viewer refused", and kick a liveness reconcile so the stale
          // row flips promptly instead of waiting for the ≤10s tick. We do NOT
          // condemn the row here: window-absence is UNKNOWN by house doctrine —
          // the tick owns condemnation with its condemnStreak hysteresis.
          send({ t: 'exit', reason: e.reason });
          // Same ctx seam as the control methods: spawnLivenessTick is declared
          // (...args) => unknown on derive's surface but resolves a promise; assert
          // that to reach .catch (NOISE, see ts-migration-bugs).
          (core.spawnLivenessTick() as Promise<unknown> | undefined)?.catch(() => {
            /* fire-and-forget reconcile */
          });
        } else {
          // e.reason / e.message are unknown off a thrown value; normalize to
          // strings and keep the truthiness-OR "first non-empty" fallback — a `??`
          // would surface an empty '' reason and suppress the default message
          // (see ts-migration-bugs).
          const failReason = typeof e?.reason === 'string' ? e.reason : '';
          const failMessage = typeof e?.message === 'string' ? e.message : '';
          send({ t: 'err', reason: failReason || failMessage || 'terminal unavailable' });
        }
        try {
          ws.close();
        } catch {
          /* already gone */
        }
      }
    })();
    openTermTasks.add(task);
    void task.then(
      () => openTermTasks.delete(task),
      () => openTermTasks.delete(task),
    );
  }
  // H-R3 + M-P1: a real keepalive replaces the "full snapshot every 5 s"
  // heartbeat. Ping every peer on both servers; terminate any that missed the
  // previous pong. terminate() fires 'close', which unwinds a leaked /ws socket
  // and — for /ws/term — the viewer + (once the last leaves) the shared tmux
  // client, the exact leak a phone that dropped wifi used to cause.
  const keepalive = setInterval(() => {
    if (quiescing) return;
    for (const clients of [snapshotClients, termClients]) {
      for (const ws of clients) {
        if (wsKeepaliveAction(ws.data.isAlive) === 'terminate') {
          ws.terminate();
          continue;
        }
        ws.data.isAlive = false;
        try {
          ws.ping();
        } catch {
          /* reaped next round */
        }
      }
    }
  }, WS_PING_MS);
  keepalive.unref();

  // ---- server: one Bun.serve fronting the router (routeRequest) and the ws
  // handler. A websocket upgrade still enters through fetch(); detect it, run the
  // SAME auth+CSRF gate the node server enforced in server.on('upgrade'), then hand
  // the socket to Bun via srv.upgrade(). A refusal returns a bodyless 4xx Response
  // instead of node's socket.destroy() — the client sees an HTTP error rather than
  // a dropped connection, which the ws test clients accept.
  function handleUpgrade(request: Request, srv: Server<WsData>, url: URL): Response | undefined {
    if (quiescing) return new Response(null, { status: 503 });
    const req = new HttpReqShim(request, srv);
    // C: every refusal below bypasses HttpResShim.end(), so arm the keep-alive-idle
    // FIN here or the socket sits in the immortal between-requests phase under
    // idleTimeout:0. This is NOT hypothetical: probed on bun 1.3.14, an
    // ATTEMPTED-and-failed srv.upgrade() (e.g. a bad Sec-WebSocket-Key on the
    // loopback-exempt path) DISARMS Bun's fixed ~12s linger reaper, so that 400
    // leaks an fd forever — the exact leak class end()'s FIN closes, reached through
    // a different door. The 401/404 (no upgrade attempt) are still reaped at Bun's
    // ~12s regardless, so arming them is belt-and-braces; doing it uniformly keeps
    // one refusal path. See bun-serve-runtime-limits.
    const refuse = (status: number): Response => {
      try {
        srv.timeout(request, KEEPALIVE_FIN_S);
      } catch {
        /* server torn down — benign */
      }
      return new Response(null, { status });
    };
    // WS AUTH + CSRF CONTRACT: reject before Bun upgrades the socket. A WebSocket is
    // NOT subject to the same-origin READ barrier, so a cross-site page could
    // otherwise read the whole snapshot or drive a live pane; the Host check closes
    // DNS rebinding (C1).
    if (!authorized(req, url) || !hostHeaderOk(req) || crossSiteReason(req)) {
      return refuse(401);
    }
    if (url.pathname === '/ws') {
      const data: WsData = { kind: 'snapshot', isAlive: true };
      return srv.upgrade(request, { data }) ? undefined : refuse(400);
    }
    if (url.pathname === '/ws/term') {
      const data: WsData = {
        kind: 'term',
        isAlive: true,
        spawn_id: url.searchParams.get('spawn') ?? '',
        cols: Number(url.searchParams.get('cols')),
        rows: Number(url.searchParams.get('rows')),
        abort: { closed: false },
        handle: null,
      };
      return srv.upgrade(request, { data }) ? undefined : refuse(400);
    }
    return refuse(404);
  }

  function fetchHandler(
    request: Request,
    srv: Server<WsData>,
  ): Response | Promise<Response> | undefined {
    let url: URL;
    try {
      url = new URL(request.url);
    } catch {
      // C: near-dead (Bun hands fetch a valid absolute request.url), but if it ever
      // fires this 400 bypasses HttpResShim.end() too — arm the keep-alive FIN so it
      // cannot leak an fd, completing the invariant that every Response Bun receives
      // is either end()-armed or FIN-armed at its bypass site.
      try {
        srv.timeout(request, KEEPALIVE_FIN_S);
      } catch {
        /* server torn down — benign */
      }
      return new Response(null, { status: 400 });
    }
    if (quiescing) {
      // Hooks are fail-open even when they lose the admission race with
      // shutdown. Other callers get a conventional retryable refusal.
      const hook = url.pathname.startsWith('/hook/');
      return new Response(hook ? '{}' : '{"ok":false,"reason":"shutting-down"}', {
        status: hook ? 200 : 503,
        headers: {
          'content-type': 'application/json',
          'x-content-type-options': 'nosniff',
        },
      });
    }
    const upgrade = (request.headers.get('upgrade') ?? '').toLowerCase();
    const connection = (request.headers.get('connection') ?? '').toLowerCase();
    if (upgrade === 'websocket' && connection.includes('upgrade')) {
      return handleUpgrade(request, srv, url);
    }
    // Feed the audited router body a node-req/res-shaped pair. routeRequest is plain
    // sync and registers any POST body 'data'/'end' listeners synchronously; _pump()
    // then replays the Bun body stream into them, and res.done resolves when the
    // router calls res.end(). We hand Bun the response only after the body has drained
    // (or the grace elapses) so a reused keep-alive socket stays in sync — see
    // drainThenRespond.
    // C: clear any keep-alive-idle FIN a PRIOR request left armed on this (reused)
    // socket, so this in-flight request runs under idleTimeout:0 like every active
    // request — a held hook / watch long-poll re-polled on a reused socket must not
    // inherit the previous response's KEEPALIVE_FIN_S bound. boundStalledDrain
    // re-arms its own FIN if THIS body withholds; end() re-arms the keep-alive FIN
    // when THIS response completes. (Assumes Bun serializes per-socket fetch
    // dispatch — true for every real client; a hand-rolled pipelining peer whose
    // req2 fetch ran before req1's response resolved would merely bound its own hold
    // at KEEPALIVE_FIN_S, never sever another request.) See bun-serve-runtime-limits
    // and HttpResShim.
    try {
      srv.timeout(request, 0);
    } catch {
      /* server torn down — benign */
    }
    const req = new HttpReqShim(request, srv);
    const res = new HttpResShim(request, srv);
    routeRequest(req, res);
    const drained = req._pump();
    const response = drainThenRespond(req, drained, res);
    const active: ActiveResponse = {
      request: req,
      response: res,
      drain: drained,
      promise: response,
      hook: url.pathname.startsWith('/hook/'),
      drained: false,
      drainFaulted: false,
    };
    activeResponses.add(active);
    // The peer can disconnect after closeHttpOnce's synchronous scan but while
    // it is awaiting this response. Re-check on the close notification so that
    // race cannot leave res.done unresolved. Queueing avoids re-entering
    // forceEnd() from the close event it emits itself.
    res.on('close', () => {
      if (!quiescing) return;
      queueMicrotask(() => forceFaultedResponseDuringShutdown(active));
    });
    void drained.then(
      () => {
        active.drained = true;
        forceFaultedResponseDuringShutdown(active);
      },
      () => {
        active.drainFaulted = true;
        active.drained = true;
        forceFaultedResponseDuringShutdown(active);
      },
    );
    void response.then(
      () => activeResponses.delete(active),
      () => activeResponses.delete(active),
    );
    return response;
  }

  // Hand Bun the response only after the request body has finished draining, so a
  // reused keep-alive socket stays in sync: Bun.serve has no per-connection close, so
  // an early response that abandons the rest of the body (oversized-refuse, or any
  // 4xx that replies before 'end') would desync the peer's NEXT request. A body that
  // is actually present drains in ~ms and wins this race immediately; a client that
  // DECLARED a large body then withheld it would park the drain forever, so the wait
  // is capped at BODY_DRAIN_GRACE_MS. On the refuse path shouldKeepAlive=false has
  // already armed the ~4s FIN, and the grace (< that FIN) lets the 413 reach the wire
  // before the socket closes. res.done resolves on res.end(); a held response (hook
  // hold / watch long-poll) resolves it long after its small body drains, so the
  // grace never gates held responses.
  function drainThenRespond(
    req: HttpReqShim,
    drained: Promise<void>,
    res: HttpResShim,
  ): Promise<Response> {
    return new Promise<Response>((resolve) => {
      let settled = false;
      const finish = (): void => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(res.done);
      };
      const timer = setTimeout(() => {
        res.boundStalledDrain();
        finish();
      }, BODY_DRAIN_GRACE_MS);
      timer.unref();
      // A body that drained CLEANLY (req not destroyed) retracts any stalled-drain
      // FIN the grace armed — clearStalledFin no-ops unless a 'stall' FIN is armed
      // — so a slow-but-real upload, or a held long-poll whose body drained just
      // past the 1s grace, is never severed. A body that FAULTED keeps the FIN so
      // the stuck socket is still reaped: _pump swallows a mid-stream read error
      // into a RESOLUTION (setting req.destroyed), not a rejection, so this guard
      // — not the reject branch — is what separates a completed body from a
      // faulted one. The rare true rejection (getReader throws) keeps it too.
      drained.then(() => {
        if (!req.destroyed) res.clearStalledFin();
        finish();
      }, finish);
    });
  }

  // Thin node-http-shaped shim over Bun.serve so the daemon entry keeps its
  // .on('error') + .listen(port,host,cb) contract. Bun.serve is constructed LAZILY
  // in .listen(): an EADDRINUSE throw (Bun sets err.code === 'EADDRINUSE') is
  // re-surfaced through the 'error' listener the daemon registered FIRST — the
  // election exit-3 path — on a microtask, matching node's async 'error' emit
  // ordering (register .on('error'), then call .listen()).
  let bunServer: Server<WsData> | null = null;
  let boundResult: HttpBound | null = null;
  let boundPromise: Promise<HttpBindResult> | null = null;
  let gracefulStartBarrier: Promise<void> = Promise.resolve();
  let resolveHoldsReleased: () => void = () => undefined;
  const holdsReleased = new Promise<void>((resolve) => {
    resolveHoldsReleased = resolve;
  });
  let holdsReleaseStarted = false;
  let gracefulStopPromise: Promise<void> | null = null;
  let forceStopPromise: Promise<void> | null = null;
  let closeClientsPromise: Promise<void> | null = null;
  let closeHttp!: () => Promise<void>;
  // The shim only ever emits 'error', and only once — Bun.serve's sole failure mode
  // here is a synchronous bind throw (EADDRINUSE) surfaced from listen(). So 'once'
  // and 'on' are identical: both register an error listener that fires at most once.
  const errorListeners: ((err: NodeJS.ErrnoException) => void)[] = [];

  function lifecycleClosedError(): NodeJS.ErrnoException {
    return Object.assign(new Error('fleetd HTTP lifecycle is closed'), {
      code: 'ERR_SERVER_CLOSED',
    }) as NodeJS.ErrnoException;
  }

  function bindFailure(error: unknown, origin: HttpBindFailed['origin']): HttpBindFailed {
    const record =
      error !== null && typeof error === 'object' ? (error as Record<string, unknown>) : null;
    const code = typeof record?.['code'] === 'string' ? record['code'] : null;
    const rawErrno = record?.['errno'];
    const errno = typeof rawErrno === 'string' || typeof rawErrno === 'number' ? rawErrno : null;
    const message = error instanceof Error ? error.message : String(error);
    return {
      _tag: 'BindFailed',
      reason:
        code === 'EADDRINUSE'
          ? 'address-in-use'
          : code === 'ERR_SERVER_CLOSED'
            ? 'closed'
            : 'other',
      origin,
      legacyDelivery: 'error-callback-microtask',
      error,
      code,
      errno,
      message,
    };
  }

  function attemptBind(port: number, host: string): HttpBindResult {
    if (quiescing) return bindFailure(lifecycleClosedError(), 'lifecycle-guard');
    if (boundResult) return boundResult;

    let live: Server<WsData>;
    try {
      live = Bun.serve({
        port,
        hostname: host,
        // 0 = never time out an idle connection (node's default): a held hook /
        // watch long-poll response must survive its full wait; the default 10s
        // idleTimeout would sever it (see bun-serve-runtime-limits). Bounded idle
        // is enforced per-request instead, across BOTH idle phases: WHILE a
        // request is in flight the stalled-drain FIN (HttpResShim.boundStalledDrain,
        // armed only when the body-drain grace expires with the body un-drained)
        // reaps a withheld-body socket; once a response completes the keep-alive
        // FIN (HttpResShim.end, ~KEEPALIVE_FIN_S) reaps a between-requests idle
        // socket whose client made one request then vanished. The fetchHandler
        // entry-clear drops both for each new in-flight request, so active
        // requests and held long-polls (bodies drain in ms) stay exempt by
        // construction while idle sockets are always bounded. NOTE: a socket that
        // connects but never completes its request line+headers is out of reach
        // here (fetch never runs) — Bun reaps that pre-request phase itself at a
        // fixed ~12s regardless of idleTimeout (bun-serve-runtime-limits).
        idleTimeout: 0,
        maxRequestBodySize: MAX_PASTE_BODY,
        fetch: fetchHandler,
        websocket,
      });
    } catch (error) {
      return bindFailure(error, 'bun-serve-throw');
    }

    bunServer = live;
    const success: HttpBound = {
      _tag: 'Bound',
      hostname: live.hostname ?? host,
      port: live.port ?? port,
    };
    boundResult = success;
    boundPromise = Promise.resolve(success);
    return success;
  }

  function bindHttp(port: number, host: string): Promise<HttpBindResult> {
    // A prior successful bind is only an acquisition identity while the owner
    // is live. Once quiesce/close retires the listener, returning that cached
    // Bound value would falsely tell the Effect root that ingress exists.
    if (quiescing) return Promise.resolve(bindFailure(lifecycleClosedError(), 'lifecycle-guard'));
    if (boundPromise) return boundPromise;
    const result = attemptBind(port, host);
    // A failed bind is deliberately retryable: the legacy node server could be
    // listened again after an error, while a successful owner is single-bind and
    // every caller observes the same completion object.
    return result._tag === 'Bound' && boundPromise ? boundPromise : Promise.resolve(result);
  }

  function emitLegacyBindFailure(failure: HttpBindFailed): void {
    const error: NodeJS.ErrnoException =
      failure.error instanceof Error
        ? (failure.error as NodeJS.ErrnoException)
        : new Error(failure.message);
    if (failure.code !== null) error.code = failure.code;
    if (typeof failure.errno === 'number') error.errno = failure.errno;
    queueMicrotask(() => {
      for (const listener of errorListeners) listener(error);
    });
  }

  const server = {
    on(event: string, cb: (err: NodeJS.ErrnoException) => void): void {
      if (event === 'error') errorListeners.push(cb);
    },
    once(event: string, cb: (err: NodeJS.ErrnoException) => void): void {
      if (event === 'error') errorListeners.push(cb);
    },
    listen(port: number, host: string, cb?: () => void): void {
      const result = attemptBind(port, host);
      if (result._tag === 'BindFailed') emitLegacyBindFailure(result);
      else cb?.();
    },
    close(cb?: () => void): void {
      // Backwards-compatible node-shaped callback surface. The explicit
      // lifecycle below is authoritative and awaitable; legacy tests that call
      // server.close(cb) now receive the callback only after every owned
      // resource has settled.
      void closeHttp().then(
        () => cb?.(),
        () => cb?.(),
      );
    },
  };
  core.onMutate = scheduleBroadcast;

  type QuestionsLifecycle = typeof core.questions & {
    quiesce?: () => void;
    releaseAll?: () => number;
  };

  const questionsLifecycle = core.questions as QuestionsLifecycle;
  let closePromise: Promise<void> | null = null;

  function resolveBroadcastWaiters(): void {
    const waiters = idleWaiters;
    idleWaiters = [];
    for (const resolve of waiters) resolve();
  }

  function finalizeNativeClients(): void {
    const clients = new Set<LiveSocket>([...closingSockets, ...snapshotClients, ...termClients]);
    for (const ws of clients) {
      if (ws.data.kind === 'term') {
        ws.data.abort.closed = true;
        ws.data.handle?.close();
      }
      try {
        ws.terminate();
      } catch {
        /* already gone */
      }
    }
    closingSockets.clear();
    snapshotClients.clear();
    termClients.clear();
  }

  function startGracefulStop(): Promise<void> {
    if (gracefulStopPromise) return gracefulStopPromise;
    const live = bunServer;
    if (!live) {
      gracefulStopPromise = Promise.resolve();
      return gracefulStopPromise;
    }

    // Let any body-fault shutdown response synchronously published by quiesce
    // cross the fetch Promise boundary before native stop begins. The operation
    // itself is still created exactly once during quiesce. A concurrent force
    // skips a not-yet-started graceful call instead of invoking stop(false)
    // against a server that stop(true) already retired.
    gracefulStopPromise = Promise.all([gracefulStartBarrier, holdsReleased])
      // `active.promise` means Bun has received the Response object, not that
      // uSockets has flushed its bytes. Yield one host turn before stop(false)
      // so a released held hook's canonical body reaches the existing socket.
      .then(() => Bun.sleep(0))
      .then(async () => {
        // Force may win while this operation is still parked behind the body /
        // held-response flush barriers. In that case graceful shutdown must
        // join the already-published force Promise; it must not clear bunServer
        // or terminate WebSockets ahead of stop(true). Bun 1.3.14 can wedge the
        // force Promise permanently when a socket is terminated first.
        if (forceStopPromise) {
          await forceStopPromise;
          return;
        }
        if (bunServer !== live) return;

        await live.stop(false);
        if (bunServer === live) bunServer = null;
        finalizeNativeClients();
      });
    // Quiesce is intentionally synchronous and cannot await this completion.
    // Observe a rejection here so a caller may still await the original shared
    // Promise without creating a process-level unhandled rejection meanwhile.
    void gracefulStopPromise.catch(() => undefined);
    return gracefulStopPromise;
  }

  function beginGracefulStopHttp(): Promise<void> {
    quiesceHttp();
    return startGracefulStop();
  }

  function forceStopHttp(): Promise<void> {
    if (forceStopPromise) return forceStopPromise;
    const live = bunServer;
    if (!live) {
      finalizeNativeClients();
      forceStopPromise = Promise.resolve();
      return forceStopPromise;
    }

    try {
      // Crucially, this does not await gracefulStopPromise. The whole-daemon
      // deadline or a second signal may enter here while stop(false) is still
      // waiting on a quiet keep-alive, held response, or WebSocket.
      forceStopPromise = Promise.resolve(live.stop(true)).then(
        () => {
          if (bunServer === live) bunServer = null;
          finalizeNativeClients();
        },
        (error: unknown) => {
          throw error;
        },
      );
    } catch (error) {
      forceStopPromise = Promise.reject(error);
    }
    void forceStopPromise.catch(() => undefined);
    return forceStopPromise;
  }

  function quiesceHttp(): void {
    if (quiescing) return;
    quiescing = true;

    // Producers can still call the core while their own owners are joining;
    // detach the transport callback first so none can schedule a post-close
    // timer or touch a retired websocket set.
    core.onMutate = () => {
      /* transport is quiescing */
    };
    questionsLifecycle.setBoardConsumerProbe(() => false);
    questionsLifecycle.quiesce?.();

    clearInterval(keepalive);
    if (flushTimer) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    dirty = false;
    resolveBroadcastWaiters();

    // Settle body-faulted/disconnected requests before asking Bun to enter its
    // native graceful state. Bun 1.3.14 can otherwise leave a withheld request
    // socket parked without delivering the canonical shutdown response after
    // stop(false) has begun. Cleanly drained application requests are untouched
    // here and remain owned until their route Promise joins below.
    const preStopSettlements: Promise<unknown>[] = [];
    for (const active of activeResponses) {
      const needsForcedResponse =
        !active.drained ||
        active.drainFaulted ||
        active.request.destroyed ||
        active.response.destroyed;
      forceFaultedResponseDuringShutdown(active);
      if (needsForcedResponse) preStopSettlements.push(active.drain, active.promise);
    }
    gracefulStartBarrier = Promise.allSettled(preStopSettlements).then(() => undefined);

    // Start native graceful shutdown now but never wait here. Holds and route /
    // client owners still need a writable transport during the following policy
    // phases, while closing-http later races this exact Promise against the one
    // absolute daemon deadline and may independently escalate through forceStop.
    void startGracefulStop();
  }

  function releaseHeldResponses(): number {
    // P1 questions exposes releaseAll(); keep the existing fail-open method as
    // the additive fallback so this HTTP owner remains compatible with narrow
    // test doubles and with a partially acquired createCore.
    try {
      return questionsLifecycle.releaseAll?.() ?? questionsLifecycle.failOpenAllHolds();
    } finally {
      if (!holdsReleaseStarted) {
        holdsReleaseStarted = true;
        // Bun 1.3.14 can reset an existing held-hook socket if stop(false)
        // begins before the response published by releaseAll crosses the fetch
        // Promise boundary. Quiesce has already created the one graceful-stop
        // operation; this barrier delays only its native call until every hook
        // response present at the phase boundary is observable by Bun.
        const hookResponses = [...activeResponses]
          .filter((active) => active.hook)
          .map((active) => active.promise);
        void Promise.allSettled(hookResponses).then(resolveHoldsReleased);
      }
    }
  }

  function beginNativeClientClose(): void {
    // Do not close OR terminate a ServerWebSocket before native stop(true): Bun
    // 1.3.14 can then leave that stop Promise pending forever. Retire the
    // application handles now; stop(false)/stop(true) remains the independently
    // awaitable owner of native socket closure in the closing-http phase.
    for (const ws of [...snapshotClients, ...termClients]) {
      closingSockets.add(ws);
      if (ws.data.kind === 'term') {
        ws.data.abort.closed = true;
        ws.data.handle?.close();
      }
    }
  }

  function forceClientsHttp(): void {
    quiesceHttp();
    // This is deliberately application-only. In Bun 1.3.14, closing or
    // terminating a native ServerWebSocket before stop(true) can leave that
    // native stop Promise pending forever. Retire viewer handles, then ask the
    // bridge to synchronously SIGKILL any control child that ignored TERM; the
    // closing-http phase remains the sole native socket force owner.
    beginNativeClientClose();
    void termbridge.force().catch((err: unknown) => {
      console.error('fleetd terminal bridge force error:', err);
    });
  }

  async function closeClientsOnce(): Promise<void> {
    quiesceHttp();

    // Watch polls are not hook decisions; settle them as an idle watcher while
    // the transport can still write. Held hook decisions are owned by the
    // preceding releaseHolds phase and deliberately stay separate.
    for (const closeWatch of [...activeWatchClosers]) {
      try {
        closeWatch();
      } catch {
        activeWatchClosers.delete(closeWatch);
      }
    }

    // Start every independent client retirement before awaiting any one owner.
    // In particular, a cleanly drained route may own DB/process work and remain
    // joined indefinitely; it must not delay terminal SIGTERM, terminal-open
    // cancellation, watch retirement, or application WebSocket handle close.
    const responsesAtClose = [...activeResponses];
    const terminalOpensAtClose = [...openTermTasks];
    for (const active of responsesAtClose) forceFaultedResponseDuringShutdown(active);
    beginNativeClientClose();

    let bridgeClose: Promise<void>;
    try {
      bridgeClose = termbridge.close().catch((err: unknown) => {
        console.error('fleetd terminal bridge close error:', err);
      });
    } catch (err) {
      console.error('fleetd terminal bridge close error:', err);
      bridgeClose = Promise.resolve();
    }

    await Promise.all([
      Promise.allSettled(responsesAtClose.map((active) => active.promise)),
      Promise.allSettled(terminalOpensAtClose),
      bridgeClose,
    ]);
    activeResponses.clear();
    openTermTasks.clear();
  }

  function closeClientsHttp(): Promise<void> {
    closeClientsPromise ??= closeClientsOnce();
    return closeClientsPromise;
  }

  async function closeHttpOnce(): Promise<void> {
    quiesceHttp();

    // Held hooks must receive canonical 200 {} while Bun can still write their
    // responses. The P4 coordinator invokes this as its own preceding phase;
    // the P1 aggregate still reaches the same ordering through close().
    try {
      releaseHeldResponses();
    } catch (err) {
      console.error('fleetd shutdown hold-release error:', err);
    }
    await closeClientsHttp();

    try {
      // Never await stop(false) here. A quiet keep-alive or a peer that ignores
      // its WebSocket close frame may hold it open indefinitely; this P1 facade
      // preserves its bounded forced close while the P4 coordinator races the
      // two shared stop operations against its absolute deadline.
      await forceStopHttp();
    } catch {
      // Preserve the P1 close contract: native stop failures never prevent the
      // remaining application handles from being retired. P4 may await the
      // shared forceStop Promise directly when it needs the typed phase failure.
    }

    finalizeNativeClients();

    errorListeners.length = 0;
  }

  closeHttp = (): Promise<void> => {
    closePromise ??= closeHttpOnce();
    return closePromise;
  };

  const lifecycle = {
    quiesce: quiesceHttp,
    beginGracefulStop: beginGracefulStopHttp,
    forceStop: forceStopHttp,
    releaseHolds: releaseHeldResponses,
    closeClients: closeClientsHttp,
    forceClients: forceClientsHttp,
    close: closeHttp,
    isQuiescing: () => quiescing,
    ownedCounts: () => ({
      listener: bunServer ? 1 : 0,
      snapshotClients: snapshotClients.size,
      terminalClients: termClients.size,
      activeResponses: activeResponses.size,
      watchWaiters: activeWatchClosers.size,
      terminalOpens: openTermTasks.size,
      broadcastTimers: flushTimer ? 1 : 0,
      keepaliveTimers: quiescing ? 0 : 1,
    }),
  };

  // `server`, `whenBroadcastIdle` and `refreshLan` are used externally:
  // fleetd.mjs listens on the server, awaits whenBroadcastIdle so the boot
  // readiness settle can wait out the coalesced flush the heals scheduled
  // (BUG-066), and drives refreshLan from its network-change poll — in the same
  // tick as the mDNS update — so the share panel (currentLan) and the Host
  // allowlist (refreshed per request from the same interface data) never
  // disagree for long (BUG-118/129). refreshLan re-enumerates the allowlist AND
  // swaps the LAN source currentLan() resolves, so the next /state snapshot
  // shows the address the host has NOW. wss/termWss/broadcast stay internal.
  return {
    server,
    bind: bindHttp,
    lifecycle,
    whenBroadcastIdle,
    // P6.4 INJECTION SEAM: program.ts (app zone) calls this after constructing
    // the HttpServer owner to hand in the ingress runRequest + the app-zone
    // workflow builders. Until it does, effectRoutes stays null and every
    // converted route answers through its legacy handler (the rollback path).
    // Arrow property for the same unbound-method reason as refreshLan; it
    // closes over effectRoutes and never touches `this`.
    installEffectRoutes: (routes: HttpEffectRoutes) => {
      effectRoutes = routes;
    },
    // Arrow-PROPERTY (not method shorthand) so the daemon entry can destructure it
    // without tripping @typescript-eslint/unbound-method: the body closes over
    // refreshLanHosts/lan and never touches `this`, so an arrow is behaviorally
    // identical while typing the field as a property rather than a method.
    refreshLan: (nextLan: LanSource | (() => LanSource) | null) => {
      if (quiescing) return;
      refreshLanHosts();
      lan = nextLan;
    },
  };
}
