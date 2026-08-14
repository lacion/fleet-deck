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
import { timingSafeEqual } from 'node:crypto';
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
        for (const cb of this._closeListeners) {
          try {
            cb();
          } catch {
            /* listener hygiene only */
          }
        }
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
    }
  }
  private _emitEnd(): void {
    if (this._destroyed) return;
    for (const cb of this._endListeners) cb();
  }
}

// A parsed entry of FLEETDECK_TRUSTED_ORIGINS (see parseTrustedOrigins).
interface TrustedOrigin {
  scheme: string;
  wildcard: boolean;
  host: string;
  port: string; // '' means the scheme default (80/443)
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

// A parsed JSON POST body is any value — object, array, scalar, or null. asRecord
// gives a typed view for the handful of fields http reads defensively WITHOUT
// asserting object-ness: a non-object body (null / array / scalar) reads every
// field as `undefined`, exactly matching the `body?.field` optional chains and
// `{ ...body }` spreads this replaces. The real narrowing still happens through
// the validate*() gates and typeof checks below; this only keeps the reads honest.
function asRecord(v: unknown): Record<string, unknown> {
  return v !== null && typeof v === 'object' ? (v as Record<string, unknown>) : {};
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

// LOOPBACK CONTRACT: local hooks and board traffic remain zero-config even
// when fleetd is in LAN mode. Node reports IPv4 peers either directly or as
// IPv4-mapped IPv6, so all three explicit forms must remain exempt. Bind-time
// classification also accepts localhost and the complete 127/8 block.
export function isLoopbackAddress(address: unknown) {
  const value = (typeof address === 'string' ? address : '').trim().toLowerCase();
  return (
    value === 'localhost' ||
    value === '::1' ||
    /^127(?:\.[0-9]{1,3}){3}$/.test(value) ||
    /^::ffff:127(?:\.[0-9]{1,3}){3}$/.test(value)
  );
}

// ------------------------------------------------------------ board static
// GET / and /assets/* serve the built React board from board-dist, resolved
// relative to THIS file's directory at runtime — the esbuild bundle keeps
// import.meta.url pointing at scripts/fleetd/, so both the source run
// (fleetd.mjs) and the bundle run (fleetd.bundle.mjs) find the same dist.
const BOARD_DIST = path.join(path.dirname(fileURLToPath(import.meta.url)), 'board-dist');

const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.txt': 'text/plain; charset=utf-8',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
};

// CSP for the HTML shell only (the one response a browser parses as a document).
// Verified against board-dist/index.html: it loads IBM Plex from
// fonts.googleapis.com (a stylesheet) and fonts.gstatic.com (the font files),
// the favicon is a data: SVG, the paste flow can mint blob: image URLs, and
// React sets inline style ATTRIBUTES (hence 'unsafe-inline' in style-src only —
// there are no inline <script>s, so script-src stays 'self'). connect-src covers
// the /state|/health|/api fetches and both WebSockets, same-origin under a proxy.
const CSP_SHELL =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; connect-src 'self'; img-src 'self' data: blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

// Serve one file from board-dist. Traversal-safe: the decoded request path is
// resolved against BOARD_DIST and must stay strictly inside it (any '..' —
// raw or percent-encoded — normalizes outside and 404s).
function serveBoardAsset(
  res: HttpResShim,
  pathname: string,
  notFound: () => HttpResShim,
): HttpResShim {
  let decoded;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return notFound();
  }
  const rel = decoded === '/' ? 'index.html' : decoded.replace(/^\/+/, '');
  const abs = path.resolve(BOARD_DIST, rel);
  if (abs !== BOARD_DIST && !abs.startsWith(BOARD_DIST + path.sep)) return notFound();
  let data;
  try {
    data = fs.readFileSync(abs);
  } catch {
    return notFound();
  }
  const ext = path.extname(abs).toLowerCase();
  // nosniff on EVERY asset (kills MIME-confusion on the hashed JS/CSS); CSP only
  // on the HTML document — subresources inherit the document's policy.
  const headers: http.OutgoingHttpHeaders = {
    'content-type': MIME[ext] ?? 'application/octet-stream',
    'content-length': data.length,
    'x-content-type-options': 'nosniff',
    // The board boots from a ?t=<token> URL; no subresource (notably the
    // Google Fonts stylesheet, which fires before token.js can scrub the URL)
    // may ever see it as a Referer.
    'referrer-policy': 'no-referrer',
    // UPGRADE CONTRACT. Vite fingerprints every asset, so /assets/* is safe to
    // cache forever — but index.html is the ONLY thing that names the current
    // fingerprints, and it shipped with no cache directives at all. A browser
    // is then free to reuse yesterday's shell after an upgrade, which is not
    // theoretical: it cost a user a full debugging session on 0.19.2, running
    // the previous board while the daemon served the new one and nothing in
    // either said so. `no-store` on the shell means an upgrade cannot be
    // invisible; `immutable` on the fingerprinted assets means it stays cheap.
    'cache-control': ext === '.html' ? 'no-store' : 'public, max-age=31536000, immutable',
  };
  if (ext === '.html') headers['content-security-policy'] = CSP_SHELL;
  res.writeHead(200, headers);
  return res.end(data);
}

// ------------------------------------------------------- trusted origins
// STANDALONE/PROXY CONTRACT. Behind a reverse proxy (Coder, nginx, Traefik) the
// browser-facing Host and Origin are the PROXY's, not ours — Coder's reverse
// proxy never rewrites req.Host — so the same-origin walls below refuse every
// POST, both WS upgrades and the mutating GETs. `FLEETDECK_TRUSTED_ORIGINS` is
// how an operator says "this other origin is also me".
//
// Entries are full origins (scheme REQUIRED, so an operator can never widen
// http and https at once by accident): `https://board.example.com`,
// `https://board.example.com:8443`, or one leading wildcard LABEL:
// `https://*.coder.example.com` — which matches `fd--main--ws--luis.coder.
// example.com` but NOT `coder.example.com` itself and NOT `a.b.coder.example.com`.
// A wildcard is deliberately single-label: `*.example.com` must not hand the
// fleet to every subdomain of a shared apex.
export function parseTrustedOrigins(spec: unknown): TrustedOrigin[] {
  const out: TrustedOrigin[] = [];
  for (const raw of (typeof spec === 'string' ? spec : '').split(',')) {
    const entry = raw.trim();
    if (!entry) continue;
    // The wildcard label is not a legal URL host, so swap in a placeholder to
    // parse, then remember that the first label was a star.
    const wild = /^([a-z][a-z0-9+.-]*:\/\/)\*\./i.exec(entry);
    const probe = wild ? entry.replace('://*.', '://wildcard-placeholder.') : entry;
    let u;
    try {
      u = new URL(probe);
    } catch {
      throw new Error(`not a valid origin: ${entry}`);
    }
    if (u.protocol !== 'http:' && u.protocol !== 'https:') {
      throw new Error(`origin must be http:// or https://: ${entry}`);
    }
    if (u.pathname !== '/' || u.search || u.hash || u.username || u.password) {
      throw new Error(`origin must be scheme://host[:port] with no path or credentials: ${entry}`);
    }
    const host = u.hostname.toLowerCase().replace(/^\[/, '').replace(/\]$/, '');
    out.push({
      scheme: u.protocol.slice(0, -1),
      // For a wildcard we keep the SUFFIX including the leading dot, so matching
      // is a suffix test plus a "no further dots" test.
      wildcard: Boolean(wild),
      host: wild ? host.replace(/^wildcard-placeholder/, '') : host,
      port: u.port, // '' means the scheme default (80/443)
    });
  }
  return out;
}

// Does `host`/`port` match this entry? Scheme is checked separately, because a
// Host header carries no scheme and an Origin does.
function trustedHostMatch(entry: TrustedOrigin, host: string, port: string) {
  if (entry.port !== port) return false;
  if (!entry.wildcard) return entry.host === host;
  if (!host.endsWith(entry.host)) return false;
  const label = host.slice(0, -entry.host.length);
  return label.length > 0 && !label.includes('.'); // exactly one label, non-empty
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

  // AUTH CONTRACT: every non-loopback HTTP route and WebSocket upgrade shares
  // this exact gate. Presented secrets are compared only after byte lengths
  // match, because timingSafeEqual throws for unequal buffers. Never include a
  // rejected credential in logs or response bodies.
  function tokenMatches(candidate: unknown) {
    if (typeof token !== 'string' || typeof candidate !== 'string') return false;
    const expected = Buffer.from(token);
    const presented = Buffer.from(candidate);
    return expected.length === presented.length && timingSafeEqual(expected, presented);
  }

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
    // local forgery — must fall through to the bearer check below and 401. This
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
    const authorization = req.headers.authorization;
    const bearer =
      typeof authorization === 'string' ? /^Bearer (.+)$/.exec(authorization)?.[1] : undefined;
    return tokenMatches(bearer) || tokenMatches(url.searchParams.get('t'));
  }

  // 0.16.0 LOOPBACK GATES. Default loopback stays open for ordinary routes,
  // but these powers require the bearer unless an explicit trust mode applies:
  // typing into a live pane (/ws/term), injecting mail into sessions, and
  // arming an unsupervised spawn. The board, the hook shims and the fleet skill
  // docs all present the token; a caller without it is precisely the attacker
  // the gate names. Two gated powers need the parsed body and live at their
  // handlers instead: gateway_* settings writes (POST /api/settings) and
  // unsupervised spawn bodies (POST /api/spawn, adopt) — see those routes.
  function tokenGatedRoute(method: string | undefined, pathname: string) {
    if (pathname === '/ws/term') return true;
    if (method !== 'POST') return false;
    return pathname === '/mail' || pathname === '/api/spawn/arm-unsupervised';
  }

  // 0.16.0 UPGRADE WHISPER. Sessions started before this release run the OLD
  // (0.15-era) hooks: plain http POSTs with no token. With the gate up, those
  // calls must be REFUSED — a tokenless hook is exactly the forgery the gate
  // exists to stop — but a refusal alone leaves the old session silently dark
  // on the board until someone restarts it. So instead of a bare 401, a
  // tokenless hook is answered in the CLI's own dialect: no state change
  // (nothing is ingested, held, or derived), but the response carries a
  // context whisper the old CLI shows its agent, which relays it to the human.
  // A session that KEEPS calling (the agent saw the whisper and the human
  // hasn't acted) escalates once: its next tokenless Stop is answered with a
  // turn-blocking restart instruction — the strongest signal the hook protocol
  // allows, sent at most once per session per daemon boot, and never losing
  // work (the turn continues after the block). Forgery vs. legacy is
  // deliberately NOT distinguished: both get the same response, the whisper
  // carries no privileged data, and the request is always refused.
  const legacyWhisperedSessions = new Set();
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
  const LEGACY_WHISPER =
    '[FLEETDECK] This session is running pre-0.16.0 hooks and is no longer reaching the fleet daemon (hook calls now require a token). Tell the human: please RESTART this Claude session — after the restart it reconnects to the board automatically.';
  const LEGACY_BLOCK_REASON =
    '[FLEETDECK] This session is running pre-0.16.0 hooks and cannot reach the fleet daemon. Stop and tell the human NOW: restart this Claude session (exit and relaunch in the same directory). Do not continue the current task until the human acknowledges — the session is running without fleet oversight.';
  function legacyHookResponse(res: HttpResShim, ev: unknown, name: string) {
    const sidRaw = asRecord(ev)['session_id'];
    const sid = typeof sidRaw === 'string' ? sidRaw : null;
    noteLegacySession(sid);
    if (name === 'Stop' && sid && !legacyWhisperedSessions.has(sid)) {
      legacyWhisperedSessions.add(sid);
      json(res, 200, { decision: 'block', reason: LEGACY_BLOCK_REASON });
      return;
    }
    json(res, 200, {
      hookSpecificOutput: { hookEventName: name, additionalContext: LEGACY_WHISPER },
    });
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

  // WHATWG URL keeps the brackets on an IPv6 hostname ([::1]); strip them so the
  // value matches what isLoopbackAddress / the lanHosts set hold.
  const normHost = (h: string) => h.toLowerCase().replace(/^\[/, '').replace(/\]$/, '');
  // A parsed URL is ours when its hostname is loopback / an own LAN address /
  // the .local name AND its EFFECTIVE port is our port. WHATWG URL normalizes
  // an explicit default port away (new URL('http://x:80').port === ''), so an
  // absent port means the scheme default 80/443 — NOT "whatever port fleetd
  // happens to listen on". Without resolving it, an Origin of plain
  // http://127.0.0.1 (a page served by any other local service on :80) read
  // as same-origin with a daemon on a non-default port, and the whole CSRF
  // wall below fell open. (BUG-030)
  function effectivePort(u: URL) {
    if (u.port) return u.port;
    return u.protocol === 'https:' ? '443' : '80'; // Host-only parses under http://
  }
  function hostAllowed(u: URL) {
    refreshLanHosts();
    const host = normHost(u.hostname);
    return (isLoopbackAddress(host) || lanHosts.has(host)) && effectivePort(u) === daemonPort;
  }
  // The operator-named extension of "us" (see parseTrustedOrigins). Kept separate
  // from hostAllowed so that a deployment which configures nothing gets today's
  // behaviour byte-for-byte: with an empty list both helpers below are false and
  // every wall is exactly as tight as it was.
  //
  // authorityTrusted ignores the scheme (a Host header has none); originTrusted
  // demands it. That asymmetry is deliberate, not an oversight: the Host wall
  // exists to stop DNS rebinding, which a scheme cannot help with, while the
  // Origin wall is the CSRF wall, where http-vs-https is a real distinction.
  function authorityTrusted(u: URL) {
    const host = normHost(u.hostname);
    return trustedOrigins.some((e) => trustedHostMatch(e, host, u.port));
  }
  function originTrusted(u: URL) {
    const host = normHost(u.hostname);
    const scheme = u.protocol.slice(0, -1);
    return trustedOrigins.some((e) => e.scheme === scheme && trustedHostMatch(e, host, u.port));
  }
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
  const isJsonContentType = (v: unknown) =>
    typeof v === 'string' && /^application\/json\b/i.test(v.trim());

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
  // unknown-name hook, telemetry-only Notification/FileChanged, and the
  // AskUserQuestion→PermissionRequest pairing all stay visible), but they are
  // never DISPATCHED to a hook handler — the dispatch gate below refuses them.
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
    const finish = (obj: unknown) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unregister();
      try {
        json(res, 200, obj);
      } catch {
        /* socket gone */
      }
    };
    const timer = setTimeout(() => {
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
      clearTimeout(timer);
      unregister();
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
  // page again.
  const isPublicShell = (method: string | undefined, pathname: string) =>
    method === 'GET' &&
    (pathname === '/' ||
      pathname === '/index.html' ||
      pathname === '/favicon.ico' ||
      pathname.startsWith('/assets/'));

  // The audited router, verbatim from the node:http era. It runs synchronously
  // over the (req, res) shims; the Bun.serve `fetch` handler below constructs the
  // shims, invokes this, then pumps the request body into it. No top-level await —
  // every route resolves through res.writeHead/end (which resolve res.done).
  function routeRequest(req: HttpReqShim, res: HttpResShim): void {
    try {
      const url = new URL(req.url ?? '/', `http://127.0.0.1:${port}`);
      const shell = isPublicShell(req.method, url.pathname);
      // Hook paths are NOT refused here: a tokenless hook is answered (and
      // refused) by legacyHookResponse AFTER its body is parsed — the upgrade
      // whisper needs the session_id and event name, and the hook dialect
      // never sends an error page. Everything else 401s as usual.
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
          // v1.2: spawn capability rides /health so the launcher/board can
          // hide all spawn UI when unavailable.
          // `managed` rides /health because that is the one thing the SessionStart
          // hook already fetches before it decides whether to evict us.
          json(res, 200, {
            ok: true,
            fleet: core.fleetSize(),
            pid: process.pid,
            version,
            managed,
            spawn: core.spawnCapability(),
            auth: termAuth,
            // Boot-reconciliation readiness (BUG-066). Both heals are kicked
            // fire-and-forget from the listen callback, so /health answering 200
            // is NOT proof they have run — and the asynchronous half
            // (reconcileSpawns) pushes a mutation broadcast when it settles a
            // row. A client with zero tolerance for a broadcast it did not cause
            // (the /ws backpressure hardening test) needs a deterministic
            // "startup mutation window is closed" signal; 'settled' flips only
            // when BOTH heals are done. No status exposed (a boot tmux failure
            // still settles); the startup refusals that would leave it
            // 'reconciling' forever never reach listen. Tests poll /health →
            // http.mjs stays the consumer of readiness, so no timer keeps the
            // loop alive. auth: termAuth is BUG-186 (the /ws/term capability).
            startup: startup?.reconciliationStatus?.() ?? null,
          });
          return;
        }
        if (url.pathname === '/state') {
          json(res, 200, snapshotWithLan());
          return;
        }
        if (url.pathname === '/api/settings') {
          json(res, 200, { ok: true, settings: core.resolveSettings() });
          return;
        }
        if (url.pathname === '/api/worktrees') {
          // Inspector failures are represented per row as verdict:unknown;
          // one broken repository must never turn this fleet-wide view into a
          // 500 or hide the other worktrees from the human.
          core
            .worktrees()
            .then((out) => {
              json(res, 200, out);
            })
            .catch((err: unknown) => {
              console.error('fleetd worktree inspector error:', err);
              json(res, 200, { ok: true, worktrees: [] });
            });
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
          operation
            .then(({ status, body }) => {
              json(res, status, body);
            })
            .catch((err: unknown) => {
              console.error('fleetd session filesystem error:', err);
              json(res, 500, { ok: false, reason: 'internal' });
            });
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
          operation
            .then(({ status, body }) => {
              json(res, status, body);
            })
            .catch((err: unknown) => {
              console.error('fleetd home filesystem error:', err);
              json(res, 500, { ok: false, reason: 'internal' });
            });
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
              // 0.16.0 upgrade path: a tokenless hook is REFUSED here — nothing
              // below may ingest, hold, or derive from it — and answered with
              // the restart whisper (see legacyHookResponse).
              if (!hookAuthed) {
                legacyHookResponse(res, ev, name);
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
              const handler = hookHandlers[name];
              if (!handler) {
                // unknown hook event: ingest telemetry anyway, respond no-op
                core.applyEvent({ hook_event_name: name, ...asRecord(ev) });
                json(res, 200, {});
                return;
              }
              // A hook payload without a usable session_id must never reach
              // the state machine: the events.mjs sid fallback would key the
              // card on the literal string 'unknown', collapsing EVERY
              // malformed payload into one shared phantom card that each
              // subsequent ID-less event then mutates. Fail open like every
              // hook path — 200 {} with no dispatch — so a broken payload
              // no-ops instead of corrupting the board. This guard is now the
              // shared runtime validator (contracts/hooks.ts); its predicate
              // (non-object body, or a missing/blank session_id) is identical
              // to the hand check it replaces, so no dispatch outcome moves.
              if (!validateHookEvent(ev).ok) {
                json(res, 200, {});
                return;
              }
              json(res, 200, handler(ev as HookBody) ?? {});
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
              core
                .postMail(ev as Parameters<typeof core.postMail>[0])
                // 0.16.0: postMail returns {status, body} on a refusal and the
                // historical bare delivery object on success (in-process
                // callers consume the bare shape — the adapter lives HERE).
                .then((out) => {
                  json(res, out.status ?? 200, out.body ?? out);
                })
                .catch((err: unknown) => {
                  console.error('fleetd mail error:', err);
                  json(res, 500, { ok: false, err: 'internal' });
                });
              return;
            }
            if (url.pathname === '/api/cleanup') {
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
              if (Object.keys(asRecord(ev)).some((k) => k.toLowerCase().startsWith('gateway_'))) {
                const authorization = req.headers.authorization;
                const bearer =
                  typeof authorization === 'string'
                    ? /^Bearer (.+)$/.exec(authorization)?.[1]
                    : undefined;
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
              const out = core.setSettings(ev);
              json(res, out.status, out.body);
              return;
            }
            if (url.pathname === '/command') {
              json(res, 200, core.command((ev as { text?: unknown }).text));
              return;
            }
            if (url.pathname === '/api/paste-image') {
              // v1.7 pasted image → file (paste.mjs). Same wall stack as every
              // control POST (auth → Host → CSRF → json content-type → body
              // cap); only the body cap is per-route (see MAX_PASTE_BODY). The
              // returned path is TYPED into the pane by the BOARD, not by us —
              // injection must ride TermPane's sendIn gate so the grid's
              // one-tile-types discipline also governs pastes.
              const out = core.pasteImage(ev as Parameters<typeof core.pasteImage>[0]);
              json(res, out.status, out.body);
              return;
            }
            if (url.pathname === '/api/spawn/arm-unsupervised') {
              // 0.16.0: mint the one-time capability an unsupervised spawn body
              // must echo. Token-gated by tokenGatedRoute even on loopback, so
              // this route existing means the caller already proved it holds
              // the bearer — the API-side half of the board's red two-step.
              logExec(url.pathname, req);
              json(res, 200, { ok: true, arm_token: core.armUnsupervised() });
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
              const spawnPmode = spawnEv['permission_mode'];
              const spawnUnsupervised =
                spawnEv['dangerously_skip_permissions'] === true ||
                (typeof spawnPmode === 'string' &&
                  spawnPmode.toLowerCase() === 'bypasspermissions');
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
              const adoptEv = asRecord(ev);
              const adoptPmode = adoptEv['permission_mode'];
              const adoptUnsupervised =
                adoptEv['dangerously_skip_permissions'] === true ||
                (typeof adoptPmode === 'string' &&
                  adoptPmode.toLowerCase() === 'bypasspermissions');
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
              const out = core.questions.dismiss(Number(dismissMatch[1]));
              json(res, out.ok ? 200 : 404, out);
              return;
            }
            const planMatch = /^\/api\/plans\/(\d+)\/mark$/.exec(url.pathname);
            if (planMatch) {
              // v1.3 plan library mark (CONTRACT): {status:"executed"|"archived",
              // via?} — 404 unknown id, 409 bad transition. Matrix documented
              // at core.planMark (derive.mjs).
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
    if (!flushTimer) return Promise.resolve();
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
    return { type: 'snapshot', ...core.snapshot(), legacy_upgrade: legacyBanner() };
  }
  function broadcast() {
    dirty = false;
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
      if (c.getBufferedAmount() > MAX_WS_BUFFER) {
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
  function scheduleBroadcast() {
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
      else if (fr['t'] === 'resize') data.handle.resize(fr['cols'] as number, fr['rows'] as number);
    },
    close(ws) {
      if (ws.data.kind === 'snapshot') {
        snapshotClients.delete(ws);
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
    // Async work (awaits termbridge.openViewer) runs inside a void-ed IIFE so the
    // caller returns void, not a floating promise. Fully try/caught; never rejects.
    void (async () => {
      const send = (frame: unknown) => {
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
        if (data.abort.closed) data.handle.close();
      } catch (err) {
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
  }
  // H-R3 + M-P1: a real keepalive replaces the "full snapshot every 5 s"
  // heartbeat. Ping every peer on both servers; terminate any that missed the
  // previous pong. terminate() fires 'close', which unwinds a leaked /ws socket
  // and — for /ws/term — the viewer + (once the last leaves) the shared tmux
  // client, the exact leak a phone that dropped wifi used to cause.
  const keepalive = setInterval(() => {
    for (const clients of [snapshotClients, termClients]) {
      for (const ws of clients) {
        if (!ws.data.isAlive) {
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
    return drainThenRespond(req, req._pump(), res);
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
  // The shim only ever emits 'error', and only once — Bun.serve's sole failure mode
  // here is a synchronous bind throw (EADDRINUSE) surfaced from listen(). So 'once'
  // and 'on' are identical: both register an error listener that fires at most once.
  const errorListeners: ((err: NodeJS.ErrnoException) => void)[] = [];
  const server = {
    on(event: string, cb: (err: NodeJS.ErrnoException) => void): void {
      if (event === 'error') errorListeners.push(cb);
    },
    once(event: string, cb: (err: NodeJS.ErrnoException) => void): void {
      if (event === 'error') errorListeners.push(cb);
    },
    listen(port: number, host: string, cb?: () => void): void {
      try {
        bunServer = Bun.serve({
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
      } catch (err) {
        queueMicrotask(() => {
          for (const l of errorListeners) l(err as NodeJS.ErrnoException);
        });
        return;
      }
      if (cb) cb();
    },
    close(cb?: () => void): void {
      // stop(true) force-closes active connections too. node's server.close() drains
      // idle then waits, but this daemon deliberately keeps sockets alive
      // (idleTimeout:0, held long-polls, ws keepalive), so a graceful stop would hang
      // teardown; tests and shutdown want a prompt close.
      try {
        bunServer?.stop(true);
      } catch {
        /* already stopped */
      }
      bunServer = null;
      if (cb) cb();
    },
  };
  core.onMutate = scheduleBroadcast;

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
    whenBroadcastIdle,
    // Arrow-PROPERTY (not method shorthand) so the daemon entry can destructure it
    // without tripping @typescript-eslint/unbound-method: the body closes over
    // refreshLanHosts/lan and never touches `this`, so an arrow is behaviorally
    // identical while typing the field as a property rather than a method.
    refreshLan: (nextLan: LanSource | (() => LanSource) | null) => {
      refreshLanHosts();
      lan = nextLan;
    },
  };
}
