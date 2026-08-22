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
import type * as http from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import path from 'node:path';

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
