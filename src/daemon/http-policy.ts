// http-policy.ts — pure parsing, security, and response-shaping policy for the
// fleetd HTTP/WebSocket surface.
//
// P6.2 extraction: this module holds the request/response DECISIONS that were
// previously inlined in http.ts — auth/CSRF/origin/loopback predicates, body
// validators, and the static-asset header policy. Everything here is pure: no
// I/O, no timers, no Bun.serve types beyond Request/Response/Headers/URL, no
// core/store access, no module-level mutable state. The transport callback in
// http.ts owns every side effect and calls into these functions; the dependency
// points ONE way (http.ts → http-policy.ts), so the router stays byte-for-byte
// while the policy becomes independently testable.
//
// Wire contract: the exact reason strings, header names/order, status codes and
// CSP below are frozen by docs/v1/evidence/effect/p6-http-matrix.md and
// tests/p6-http-freeze.test.ts. Moving a literal is fine; rewording one is not.
//
// P6.4: mapEffectRouteExit (bottom of file) turns an Effect route's Exit into a
// response PLAN. It stays pure — it never writes res — and imports effect/Exit +
// effect/Cause only as pure classifiers (a DOMAIN module may import bare effect/*;
// only contracts/ and the fail-open floor are barred — see import-boundaries.ts).
import type * as http from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import path from 'node:path';
import * as Cause from 'effect/Cause';
import * as Exit from 'effect/Exit';

// A parsed JSON POST body is any value — object, array, scalar, or null. asRecord
// gives a typed view for the handful of fields http reads defensively WITHOUT
// asserting object-ness: a non-object body (null / array / scalar) reads every
// field as `undefined`, exactly matching the `body?.field` optional chains and
// `{ ...body }` spreads this replaces. The real narrowing still happens through
// the validate*() gates and typeof checks below; this only keeps the reads honest.
export function asRecord(v: unknown): Record<string, unknown> {
  return v !== null && typeof v === 'object' ? (v as Record<string, unknown>) : {};
}

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

export { MIME, CSP_SHELL };

// Resolve one request path to an absolute file under boardDist, or null when it
// escapes. Traversal-safe: the decoded request path is resolved against
// boardDist and must stay strictly inside it (any '..' — raw or percent-encoded
// — normalizes outside and returns null). A malformed percent-encoding also
// returns null. I/O (the actual read) stays with the caller in http.ts.
export function resolveBoardAssetPath(pathname: string, boardDist: string): string | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return null;
  }
  const rel = decoded === '/' ? 'index.html' : decoded.replace(/^\/+/, '');
  const abs = path.resolve(boardDist, rel);
  if (abs !== boardDist && !abs.startsWith(boardDist + path.sep)) return null;
  return abs;
}

// Header policy for a served board asset. nosniff on EVERY asset (kills
// MIME-confusion on the hashed JS/CSS); CSP only on the HTML document —
// subresources inherit the document's policy.
export function boardAssetHeaders(ext: string, length: number): http.OutgoingHttpHeaders {
  const headers: http.OutgoingHttpHeaders = {
    'content-type': MIME[ext] ?? 'application/octet-stream',
    'content-length': length,
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
  return headers;
}

// ------------------------------------------------------- trusted origins
// A parsed entry of FLEETDECK_TRUSTED_ORIGINS (see parseTrustedOrigins).
export interface TrustedOrigin {
  scheme: string;
  wildcard: boolean;
  host: string;
  port: string; // '' means the scheme default (80/443)
}

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
export function trustedHostMatch(entry: TrustedOrigin, host: string, port: string) {
  if (entry.port !== port) return false;
  if (!entry.wildcard) return entry.host === host;
  if (!host.endsWith(entry.host)) return false;
  const label = host.slice(0, -entry.host.length);
  return label.length > 0 && !label.includes('.'); // exactly one label, non-empty
}

// ------------------------------------------------------------ auth leaves
// AUTH CONTRACT: every non-loopback HTTP route and WebSocket upgrade shares
// this exact gate. Presented secrets are compared only after byte lengths
// match, because timingSafeEqual throws for unequal buffers. Never include a
// rejected credential in logs or response bodies.
export function tokenMatches(token: unknown, candidate: unknown) {
  if (typeof token !== 'string' || typeof candidate !== 'string') return false;
  const expected = Buffer.from(token);
  const presented = Buffer.from(candidate);
  return expected.length === presented.length && timingSafeEqual(expected, presented);
}

// Extract the bearer credential from an Authorization header value, or undefined
// when it is absent/non-string/not a Bearer token.
export function parseBearer(authorization: unknown): string | undefined {
  return typeof authorization === 'string' ? /^Bearer (.+)$/.exec(authorization)?.[1] : undefined;
}

// 0.16.0 LOOPBACK GATES. Default loopback stays open for ordinary routes,
// but these powers require the bearer unless an explicit trust mode applies:
// typing into a live pane (/ws/term), injecting mail into sessions, and
// arming an unsupervised spawn. The board, the hook shims and the fleet skill
// docs all present the token; a caller without it is precisely the attacker
// the gate names. Two gated powers need the parsed body and live at their
// handlers instead: gateway_* settings writes (POST /api/settings) and
// unsupervised spawn bodies (POST /api/spawn, adopt) — see those routes.
export function tokenGatedRoute(method: string | undefined, pathname: string) {
  if (pathname === '/ws/term') return true;
  if (method !== 'POST') return false;
  return pathname === '/mail' || pathname === '/api/spawn/arm-unsupervised';
}

// The data-free public shell — GET routes a browser may fetch before it can
// present any credential. Kept deliberately narrow so nothing that reads state
// or wields a power leaks through it.
export function isPublicShell(method: string | undefined, pathname: string) {
  return (
    method === 'GET' &&
    (pathname === '/' ||
      pathname === '/index.html' ||
      pathname === '/favicon.ico' ||
      pathname.startsWith('/assets/'))
  );
}

export function isJsonContentType(v: unknown) {
  return typeof v === 'string' && /^application\/json\b/i.test(v.trim());
}

// ------------------------------------------------------- host / origin walls
// WHATWG URL keeps the brackets on an IPv6 hostname ([::1]); strip them so the
// value matches what isLoopbackAddress / the lanHosts set hold.
export function normHost(h: string) {
  return h.toLowerCase().replace(/^\[/, '').replace(/\]$/, '');
}

// A parsed URL's EFFECTIVE port. WHATWG URL normalizes an explicit default port
// away (new URL('http://x:80').port === ''), so an absent port means the scheme
// default 80/443 — NOT "whatever port fleetd happens to listen on". Without
// resolving it, an Origin of plain http://127.0.0.1 (a page served by any other
// local service on :80) read as same-origin with a daemon on a non-default
// port, and the whole CSRF wall fell open. (BUG-030)
export function effectivePort(u: URL) {
  if (u.port) return u.port;
  return u.protocol === 'https:' ? '443' : '80'; // Host-only parses under http://
}

// A parsed URL is ours when its hostname is loopback / an own LAN address /
// the .local name AND its effective port is our port. The caller owns the
// mutable lanHosts set and refreshes it before asking (that refresh is I/O and
// stays in http.ts); this decision is pure over the set it is handed.
export function hostIsOwn(u: URL, lanHosts: ReadonlySet<string>, daemonPort: string) {
  const host = normHost(u.hostname);
  return (isLoopbackAddress(host) || lanHosts.has(host)) && effectivePort(u) === daemonPort;
}

// The operator-named extension of "us" (see parseTrustedOrigins). Kept separate
// from hostIsOwn so that a deployment which configures nothing gets today's
// behaviour byte-for-byte: with an empty list both helpers are false and every
// wall is exactly as tight as it was.
//
// authorityTrusted ignores the scheme (a Host header has none); originTrusted
// demands it. That asymmetry is deliberate, not an oversight: the Host wall
// exists to stop DNS rebinding, which a scheme cannot help with, while the
// Origin wall is the CSRF wall, where http-vs-https is a real distinction.
export function authorityTrusted(trustedOrigins: readonly TrustedOrigin[], u: URL) {
  const host = normHost(u.hostname);
  return trustedOrigins.some((e) => trustedHostMatch(e, host, u.port));
}
export function originTrusted(trustedOrigins: readonly TrustedOrigin[], u: URL) {
  const host = normHost(u.hostname);
  const scheme = u.protocol.slice(0, -1);
  return trustedOrigins.some((e) => e.scheme === scheme && trustedHostMatch(e, host, u.port));
}

// ------------------------------------------------------- body shape checks
// Is this a request to arm an unsupervised (permission-bypassing) spawn/adopt?
// v1.3 accepts either dangerously_skip_permissions:true or permission_mode
// "bypassPermissions" (validated/applied in derive.spawn too). Shared by the
// spawn and adopt routes so their detection can never drift.
export function isUnsupervisedRequest(ev: unknown) {
  const body = asRecord(ev);
  const pmode = body['permission_mode'];
  return (
    body['dangerously_skip_permissions'] === true ||
    (typeof pmode === 'string' && pmode.toLowerCase() === 'bypasspermissions')
  );
}

// Does this settings body touch any gateway_* key? Those writes reroute every
// future session's LLM traffic and can leak the gateway credential, so the
// route keeps requiring the bearer for them; this is only the shape probe, the
// waiver decision (which reads peer address / headers) stays at the handler.
export function gatewaySettingsTouched(ev: unknown) {
  return Object.keys(asRecord(ev)).some((k) => k.toLowerCase().startsWith('gateway_'));
}

// Validate a POST /api/repos/preflight body. Returns the exact 400 reason
// string, or null when the body's field types are acceptable.
export function repoPreflightBodyError(body: Record<string, unknown>): string | null {
  if (typeof body['repo'] !== 'string') return 'repo must be a string';
  for (const key of ['repo_host', 'repo_transport', 'repo_org']) {
    if (body[key] != null && typeof body[key] !== 'string') return `${key} must be a string`;
  }
  return null;
}

// ------------------------------------------------------- websocket snapshot leaves
// P6.4 WS-snapshot ingress slice. The /ws snapshot surface is CONVERTED-BY-OWNERSHIP
// (its lifecycle already runs under the P6.3 HttpServer owner) plus PURE-LEAVES-ONLY:
// the three decisions the surface makes are lifted here so each is testable without a
// live socket. They are DECISIONS, never side effects — the transport in http.ts still
// owns the send/terminate/ping calls, the isAlive write, and the frame stringify. Byte
// identity is frozen by docs/v1/evidence/effect/p6-http-matrix.md §3 and pinned by the
// ws-hardening suite; the backpressure signal is the per-socket buffered-byte count vs
// a cap (P6.5 preserve-as-implemented), never a send()/ping() return value.
//
// These stay PURE POLICY rather than Effect workflows on purpose: they run inside the
// synchronous broadcast loop and the keepalive timer — transport machinery, not
// application handlers — so wrapping them in an Effect through the ingress bridge would
// manufacture Effect for its own sake and buy nothing. The broadcast TRIGGER (a coalesced
// setTimeout in http.ts) is left untouched for the same reason.

// H-R3/R1-2 broadcast backpressure: a /ws peer whose queued bytes have passed the cap is
// EVICTED rather than fed another snapshot — the transport terminates it so the connect
// handler can re-seed a full snapshot on reconnect (correctness over a silent partial
// board); at or under the cap the frame is sent. The test forces the cap to -1 to evict
// every peer deterministically (bufferedAmount is always >= 0), so the comparison is
// strictly-greater-than to match: 0 > -1 evicts, and an idle socket at 0 > 0 is admitted.
export function wsBufferEviction(bufferedAmount: number, cap: number): 'evict' | 'send' {
  return bufferedAmount > cap ? 'evict' : 'send';
}

// H-R3/M-P1 heartbeat liveness: on each keepalive tick a peer that has not ponged since
// the previous tick (isAlive === false) is TERMINATED; a live one is PINGED (and marked
// not-alive until its next pong). Both logical servers share the SAME rule — the /ws
// snapshot sockets and the /ws/term viewers — so this one decision serves the shared
// keepalive loop, which still owns ws.terminate()/ws.ping() and the isAlive reset.
export function wsKeepaliveAction(isAlive: boolean): 'ping' | 'terminate' {
  return isAlive ? 'ping' : 'terminate';
}

// The /ws snapshot FRAME shape, in frozen key order: the literal `type:'snapshot'`
// discriminator, then the core snapshot spread (own key order preserved), then
// legacy_upgrade LAST. H-S1: the caller passes core.snapshot() — NOT snapshotWithLan() —
// so the token-bearing lan block never rides a frame a /ws client can read; that choice
// stays at the call site, this leaf only fixes the wrapper shape/order. BUG-031:
// legacy_upgrade MUST ride the frame or a live board wipes the restart banner on connect.
export function assembleSnapshotFrame<S extends object, L>(
  snapshot: S,
  legacyUpgrade: L,
): { readonly type: 'snapshot' } & S & { readonly legacy_upgrade: L } {
  return { type: 'snapshot', ...snapshot, legacy_upgrade: legacyUpgrade };
}

// ------------------------------------------------------- effect route mapping
// P6.4: the Exit → Response PLAN for an Effect route (see the CONVENTION header
// in app/http-workflows/health-state.ts). Pure — it classifies the Exit and
// returns a discriminated plan; the transport in http.ts performs the actual res
// write, so this stays as side-effect-free as every other policy leaf. Cases are
// FROZEN against the legacy behaviour of these routes:
//   success → the workflow value. Snapshot settlers write it as 200 JSON;
//             mutating settlers write `json(res, value.status, value.body)`
//             because 201/400/409/413/500-from-core are data responses, not
//             typed errors;
//   quiesce → ApplicationQuiescingError (ingress refused, workflow never ran)
//             OR an interrupts-only Exit (shutdown cancelled an already-
//             admitted fiber). Detected STRUCTURALLY by _tag so this domain
//             module needs no import of app/errors.ts. Classification is
//             shared; interpretation is per settler:
//               snapshot (health/state) → fall back to the legacy handler
//                 (we do NOT invent a new 503 for those);
//               sync mutating (paste-image, settings, command, five
//                 controlSync POSTs) → frozen shutdown 503, never replay
//                 the write (Effect.sync completes on the admitting turn;
//                 interrupt-after-start is a no-op);
//               async mutating (POST /mail, /api/cleanup, six
//                 controlAsync POSTs) → startOnce witness: 503 ONLY when
//                 the native Promise never started; else JOIN it and emit
//                 the true legacy bytes so res.done still ties closeClients
//                 to the write;
//   defect  → a die (or an unexpected fail on an E=never route); the
//             transport replays the byte-identical catch of that route class
//             (GET outer-catch `500 {}`; POST inner-catch `500 {err:'internal'}`).
export type EffectRouteOutcome<A> =
  | { readonly kind: 'success'; readonly value: A }
  | { readonly kind: 'quiesce' }
  | { readonly kind: 'defect'; readonly defect: unknown };

function isApplicationQuiescing(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    (error as { readonly _tag?: unknown })._tag === 'ApplicationQuiescingError'
  );
}

export function mapEffectRouteExit<A>(exit: Exit.Exit<A, unknown>): EffectRouteOutcome<A> {
  if (Exit.isSuccess(exit)) return { kind: 'success', value: exit.value };
  // v4 Cause: inspect reasons directly, mirroring live-layer.ts's exit mapping.
  const failure = exit.cause.reasons.find(Cause.isFailReason);
  if (failure && isApplicationQuiescing(failure.error)) return { kind: 'quiesce' };
  // Interruption during shutdown (the quiescing fiber cancels this in-flight
  // request) reports 'quiesce' — the SAME classification as an explicit
  // ApplicationQuiescingError refusal. hasInterruptsOnly is true only when
  // EVERY reason is an interrupt, so a mixed defect+interrupt cause still
  // falls through to the defect arm below.
  //
  // Classification is shared; interpretation is per settler (see the header
  // above). Do NOT read this arm as "always fall back to the legacy
  // synchronous handler": that is snapshot-only. Mutating settlers must not
  // replay a refused write; async-mutating settlers JOIN a started native
  // Promise instead of 503ing it.
  if (Cause.hasInterruptsOnly(exit.cause)) return { kind: 'quiesce' };
  const die = exit.cause.reasons.find(Cause.isDieReason);
  if (die) return { kind: 'defect', defect: die.defect };
  // An unexpected non-quiesce fail (should not occur on an E=never route) is a
  // defect: the transport replays the byte-identical console.error + 500 {} the
  // legacy outer catch already emits for a non-hook route.
  return { kind: 'defect', defect: failure ? failure.error : exit.cause };
}
