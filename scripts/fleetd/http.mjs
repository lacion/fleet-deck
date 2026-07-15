// http.mjs — fleetd HTTP + WebSocket surface.
//
// Hook endpoints answer with hook-output JSON directly; every
// handler fails open — an internal error still returns 200 {} so a hook can
// never break a session. Board/control API: /health /state /mail /command,
// /api/cleanup,
// static board at / + /assets/* (built React app from board-dist), the spike
// board at /plain, and WS /ws (snapshot on connect and on every mutation; a
// ping/pong keepalive — not a periodic snapshot — reaps dead peers).

import http from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import os from 'node:os';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
// 0.7.1: one validator for the custom-name suffix, shared with the `name`
// orchestrator command so the REST route and the text command can never drift.
import { validateNameSuffix } from './helpers.mjs';
import { WebSocketServer } from 'ws';
import { createTermBridge } from './termbridge.mjs';

const MAX_BODY = 1e6;
// /api/paste-image only: a screenshot is megabytes, and base64-in-JSON (kept —
// the json content-type wall forces a CORS preflight that raw image/png would
// dodge) inflates it another third. paste.mjs caps the DECODED image at 10 MB;
// 10 MB base64 is ~13.4 MB, plus the small JSON/data-URL envelope — 14 MB
// carries it with headroom and nothing more. Every other POST keeps MAX_BODY.
const MAX_PASTE_BODY = 14e6;
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
  const n = Number(process.env.FLEETDECK_WS_BUFFER_MAX);
  return Number.isFinite(n) ? n : (1 << 20); // 1 MiB
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
const MAX_TERM_WS_BUFFER = 4 << 20;   // 4 MiB

// LOOPBACK CONTRACT: local hooks and board traffic remain zero-config even
// when fleetd is in LAN mode. Node reports IPv4 peers either directly or as
// IPv4-mapped IPv6, so all three explicit forms must remain exempt. Bind-time
// classification also accepts localhost and the complete 127/8 block.
export function isLoopbackAddress(address) {
  const value = String(address || '').trim().toLowerCase();
  return value === 'localhost'
    || value === '::1'
    || /^127(?:\.[0-9]{1,3}){3}$/.test(value)
    || /^::ffff:127(?:\.[0-9]{1,3}){3}$/.test(value);
}

// ------------------------------------------------------------ board static
// GET / and /assets/* serve the built React board from board-dist, resolved
// relative to THIS file's directory at runtime — the esbuild bundle keeps
// import.meta.url pointing at scripts/fleetd/, so both the source run
// (fleetd.mjs) and the bundle run (fleetd.bundle.mjs) find the same dist.
// The spike board stays available verbatim at GET /plain.
const BOARD_DIST = path.join(path.dirname(fileURLToPath(import.meta.url)), 'board-dist');

const MIME = {
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

// Serve one file from board-dist. Traversal-safe: the decoded request path is
// resolved against BOARD_DIST and must stay strictly inside it (any '..' —
// raw or percent-encoded — normalizes outside and 404s).
function serveBoardAsset(res, pathname, notFound) {
  let decoded;
  try { decoded = decodeURIComponent(pathname); } catch { return notFound(); }
  const rel = decoded === '/' ? 'index.html' : decoded.replace(/^\/+/, '');
  const abs = path.resolve(BOARD_DIST, rel);
  if (abs !== BOARD_DIST && !abs.startsWith(BOARD_DIST + path.sep)) return notFound();
  let data;
  try { data = fs.readFileSync(abs); } catch { return notFound(); }
  res.writeHead(200, {
    'content-type': MIME[path.extname(abs).toLowerCase()] || 'application/octet-stream',
    'content-length': data.length,
  });
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
export function parseTrustedOrigins(spec) {
  const out = [];
  for (const raw of String(spec || '').split(',')) {
    const entry = raw.trim();
    if (!entry) continue;
    // The wildcard label is not a legal URL host, so swap in a placeholder to
    // parse, then remember that the first label was a star.
    const wild = /^([a-z][a-z0-9+.-]*:\/\/)\*\./i.exec(entry);
    const probe = wild ? entry.replace('://*.', '://wildcard-placeholder.') : entry;
    let u;
    try { u = new URL(probe); } catch { throw new Error(`not a valid origin: ${entry}`); }
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
function trustedHostMatch(entry, host, port) {
  if (entry.port !== port) return false;
  if (!entry.wildcard) return entry.host === host;
  if (!host.endsWith(entry.host)) return false;
  const label = host.slice(0, -entry.host.length);
  return label.length > 0 && !label.includes('.'); // exactly one label, non-empty
}

export function createHttp(core, {
  port, boardFile, version = '0.0.0', capture = () => {}, token, lan = null,
  trustedOrigins = [], proxyAuth = 'token', managed = false,
}) {
  // The board renders its share panel from this: the exact URLs a peer can
  // open, token included (a browser cannot send an Authorization header on its
  // first navigation). Absent/disabled ⇒ the panel says "local only" rather
  // than inventing a URL. Only ever handed to an ALREADY-AUTHORIZED caller —
  // snapshot() is behind the same gate as everything else.
  const lanInfo = lan?.enabled
    ? { enabled: true, urls: lan.urls ?? [], mdns: lan.mdns ?? null }
    : { enabled: false, urls: [] };

  function snapshotWithLan() {
    return { ...core.snapshot(), lan: lanInfo };
  }

  function json(res, code, obj) {
    const body = JSON.stringify(obj);
    res.writeHead(code, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
    res.end(body);
  }

  // AUTH CONTRACT: every non-loopback HTTP route and WebSocket upgrade shares
  // this exact gate. Presented secrets are compared only after byte lengths
  // match, because timingSafeEqual throws for unequal buffers. Never include a
  // rejected credential in logs or response bodies.
  function tokenMatches(candidate) {
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
  // Either way a LOCAL CLI hook is untouched: it sends no Origin, so
  // viaTrustedProxy is false and the loopback exemption still applies.
  function authorized(req, url) {
    if (isLoopbackAddress(req.socket?.remoteAddress)) {
      if (!(proxyAuth === 'token' && viaTrustedProxy(req))) return true;
    }
    const authorization = req.headers.authorization;
    const bearer = typeof authorization === 'string' ? /^Bearer (.+)$/.exec(authorization)?.[1] : undefined;
    return tokenMatches(bearer) || tokenMatches(url.searchParams.get('t'));
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
  // advertised mDNS .local name. Built once — a LAN address is not going to
  // change under the daemon's feet.
  const lanHosts = new Set();
  try {
    for (const entries of Object.values(os.networkInterfaces())) {
      for (const entry of entries || []) {
        if (entry?.address) lanHosts.add(String(entry.address).toLowerCase());
      }
    }
  } catch { /* restricted sandbox: loopback stays allowed regardless */ }
  try {
    if (lan?.mdns) lanHosts.add(new URL(lan.mdns).hostname.toLowerCase());
  } catch { /* malformed mDNS URL — skip it; the IP URLs still work */ }

  // WHATWG URL keeps the brackets on an IPv6 hostname ([::1]); strip them so the
  // value matches what isLoopbackAddress / the lanHosts set hold.
  const normHost = h => String(h || '').toLowerCase().replace(/^\[/, '').replace(/\]$/, '');
  // A parsed URL is ours when its hostname is loopback / an own LAN address /
  // the .local name AND its port is our port (or absent, i.e. a default 80/443).
  function hostAllowed(u) {
    const host = normHost(u.hostname);
    return (isLoopbackAddress(host) || lanHosts.has(host)) && (u.port === '' || u.port === daemonPort);
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
  function authorityTrusted(u) {
    const host = normHost(u.hostname);
    return trustedOrigins.some(e => trustedHostMatch(e, host, u.port));
  }
  function originTrusted(u) {
    const host = normHost(u.hostname);
    const scheme = u.protocol.slice(0, -1);
    return trustedOrigins.some(e => e.scheme === scheme && trustedHostMatch(e, host, u.port));
  }
  // Host header check — the DNS-rebinding wall. A browser always sends Host, so a
  // domain that re-resolves to this box arrives as Host: evil.example and is
  // refused. A missing Host is a non-browser caller and is left alone. A proxied
  // request arrives with the PROXY's Host, which only passes once an operator has
  // named it in FLEETDECK_TRUSTED_ORIGINS.
  function hostHeaderOk(req) {
    const host = req.headers.host;
    if (typeof host !== 'string' || !host) return true;
    let u; try { u = new URL('http://' + host); } catch { return false; }
    return hostAllowed(u) || authorityTrusted(u);
  }
  // Sec-Fetch-Site + Origin verdict for a STATE-CHANGING request. Returns null
  // when it may proceed. Sec-Fetch-Site, when the browser sends it, is
  // authoritative for the cross-site call; an Origin, when present, must resolve
  // to one of our own hosts; no Origin at all is a non-browser CLI hook and is
  // allowed. The reason drives our control flow only — it is never echoed back.
  function crossSiteReason(req) {
    const site = req.headers['sec-fetch-site'];
    if (site === 'cross-site' || site === 'cross-origin') return 'cross-site';
    const origin = req.headers.origin;
    if (typeof origin === 'string' && origin) {
      let u; try { u = new URL(origin); } catch { return 'bad-origin'; } // 'null', junk
      if (!hostAllowed(u) && !originTrusted(u)) return 'cross-origin';
    }
    return null;
  }

  // Is this a browser arriving through a reverse proxy — i.e. an Origin that is
  // trusted but is NOT one of our own hosts? Such a request has already cleared
  // the walls above; this only decides whether it must ALSO carry the token.
  function viaTrustedProxy(req) {
    const origin = req.headers.origin;
    if (typeof origin !== 'string' || !origin) return false; // a CLI hook
    let u; try { u = new URL(origin); } catch { return false; }
    return !hostAllowed(u) && originTrusted(u);
  }
  const isJsonContentType = v => typeof v === 'string' && /^application\/json\b/i.test(v.trim());

  // PermissionRequest / Elicitation / AskUserQuestion are handled OUT of this
  // table (Phase 3/4 hold-open relay — the response is parked, see the hook
  // branch below).
  const hookHandlers = {
    SessionStart: ev => core.hookSessionStart(ev),
    UserPromptSubmit: ev => core.hookUserPromptSubmit(ev),
    PostToolUse: ev => core.hookPostToolUse(ev),
    PreToolUse: ev => core.hookPostToolUse(ev), // same derivation branch as the spike
    Stop: ev => core.hookStop(ev),
    SessionEnd: ev => core.hookSessionEnd(ev),
    Notification: ev => (core.applyEvent({ ...ev, hook_event_name: 'Notification' }), {}),
    FileChanged: ev => (core.applyEvent({ ...ev, hook_event_name: 'FileChanged' }), {}),
  };

  // F3a/F3b/F3c hold-open relay: create the durable question row, then park
  // the HTTP response until the board answers, the hold window lapses
  // (respond {} — normal flow resumes in the terminal), or the client
  // disconnects. questions.mjs owns the arbitration; this only wires the
  // socket to it. Fail open like every hook path: intake errors still 200 {}.
  function holdHook(res, ev, name) {
    let row = null;
    try { row = core.hookHoldQuestion(ev, name); } catch (err) { console.error('fleetd hold intake error:', err); }
    if (!row) return json(res, 200, {});
    core.questions.attachHold(row, obj => json(res, 200, obj));
    res.on('close', () => {
      try { core.questions.socketClosed(row.id); } catch { /* hold hygiene only */ }
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
  //     response: the mail row's delivered_at is set in the same synchronous
  //     step that resolves the poll, so the turn-boundary path
  //     (UserPromptSubmit/Stop-block/GET /mail drains, which all filter
  //     delivered_at IS NULL) can never re-deliver it. `text` is the RAW
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
  //   gone → 'close' unregisters the waiter, nothing claimed. Accepted
  //   window: a claim whose response the watcher never reads loses
  //   auto-delivery (the mail row is marked delivered either way).
  function watchHook(req, res, url) {
    const sid = url.searchParams.get('session') || '';
    const holdRaw = Number(url.searchParams.get('hold_ms'));
    const holdMs = Number.isFinite(holdRaw) ? Math.max(0, Math.min(holdRaw, 25_000)) : 25_000;

    const attempt = () => {
      const info = core.watchInfo(sid);
      if (!info.session_alive) return { status: 'idle', ...info };
      const claimed = core.claimMail(sid);
      if (claimed) return { status: 'mail', ...claimed };
      return null; // session alive, no undelivered mail → hold (mail-wake)
    };

    const immediate = attempt();
    if (immediate) return json(res, 200, immediate);

    let settled = false;
    let unregister = () => {};
    const finish = obj => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unregister();
      try { json(res, 200, obj); } catch { /* socket gone */ }
    };
    const timer = setTimeout(() => finish({ status: 'idle', ...core.watchInfo(sid) }), holdMs);
    timer.unref?.();
    unregister = core.addWatchWaiter(sid, () => {
      if (settled || res.writableEnded || res.destroyed) return;
      const out = attempt();
      if (out) finish(out);
    });
    res.on('close', () => { settled = true; clearTimeout(timer); unregister(); });
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
  const isPublicShell = (method, pathname) => method === 'GET'
    && (pathname === '/' || pathname === '/index.html' || pathname === '/favicon.ico'
      || pathname.startsWith('/assets/'));

  const server = http.createServer((req, res) => {
    try {
      const url = new URL(req.url, `http://127.0.0.1:${port}`);
      const shell = isPublicShell(req.method, url.pathname);
      if (!shell && !authorized(req, url)) {
        return json(res, 401, { ok: false, reason: 'unauthorized' });
      }
      // DNS-REBINDING DEFENSE (C1/H-S3): a page pointed at a domain that
      // re-resolves to this box arrives with a foreign Host — refuse it on every
      // route that carries data or DOES something. The data-free public shell
      // stays open (a browser must load it before it can present the token). A
      // hook keeps its fail-open dialect so an odd proxy can never wedge a
      // real session; a genuine loopback hook sends a loopback Host and is fine.
      if (!shell && !hostHeaderOk(req)) {
        return url.pathname.startsWith('/hook/') ? json(res, 200, {}) : json(res, 403, { ok: false, reason: 'forbidden' });
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
          return json(res, 403, { ok: false, reason: 'forbidden' });
        }
        if (url.pathname === '/health') {
          // v1.2: spawn capability rides /health so the launcher/board can
          // hide all spawn UI when unavailable.
          // `managed` rides /health because that is the one thing the SessionStart
          // hook already fetches before it decides whether to evict us.
          return json(res, 200, {
            ok: true, fleet: core.fleetSize(), pid: process.pid, version, managed,
            spawn: core.spawnCapability(),
          });
        }
        if (url.pathname === '/state') return json(res, 200, snapshotWithLan());
        if (url.pathname === '/api/worktrees') {
          // Inspector failures are represented per row as verdict:unknown;
          // one broken repository must never turn this fleet-wide view into a
          // 500 or hide the other worktrees from the human.
          core.worktrees()
            .then(out => json(res, 200, out))
            .catch(err => {
              console.error('fleetd worktree inspector error:', err);
              json(res, 200, { ok: true, worktrees: [] });
            });
          return;
        }
        if (url.pathname === '/mail') {
          const sid = url.searchParams.get('session') || '';
          const box = core.drainMail(sid);
          if (box.length) broadcast();
          return json(res, 200, { mail: box });
        }
        if (url.pathname === '/api/watch') return watchHook(req, res, url); // F3d-2 long-poll
        if (url.pathname === '/plain') {
          // the Phase 1 spike board, served verbatim
          res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
          return res.end(fs.readFileSync(boardFile));
        }
        if (url.pathname === '/' || url.pathname.startsWith('/assets/')) {
          // built React board (Phase 5) from board-dist
          return serveBoardAsset(res, url.pathname, () => json(res, 404, { err: 'nope' }));
        }
        return json(res, 404, { err: 'nope' });
      }

      if (req.method === 'POST') {
        const isHook = url.pathname.startsWith('/hook/');
        // CSRF WALL (C1): a state-changing request driven from another origin is
        // refused before a byte of its body is read. Real CLI hooks send no
        // Origin and a loopback Host, so they pass untouched; a browser on
        // another site is turned away. A refused hook still answers in the
        // fail-open dialect so it can never break a session.
        if (crossSiteReason(req)) {
          return isHook ? json(res, 200, {}) : json(res, 403, { ok: false, reason: 'forbidden' });
        }
        // CONTENT-TYPE WALL (C1): control POSTs must declare JSON — which also
        // forces a CORS preflight for any cross-origin attempt, a second wall in
        // front of /api/spawn et al. Hooks are EXEMPT per the hook contract: a
        // hook with an odd/absent content-type is still processed (fail open).
        if (!isHook && !isJsonContentType(req.headers['content-type'])) {
          return json(res, 415, { ok: false, reason: 'expected application/json' });
        }
        // M-B3: collect raw Buffers, cap by BYTES, decode ONCE. `body += d`
        // stringified each TCP chunk independently — a multibyte glyph straddling
        // a chunk boundary decoded to U+FFFD — and `body.length` counted UTF-16
        // units, not bytes. Concatenating the bytes and decoding the whole once
        // is byte-exact.
        const chunks = [];
        let size = 0;
        let tooLarge = false;
        const bodyCap = url.pathname === '/api/paste-image' ? MAX_PASTE_BODY : MAX_BODY;
        // Refuse an oversized body by its declared Content-Length before reading
        // a byte — the streaming cap below still catches a lying/absent header,
        // but this avoids buffering megabytes only to reject them.
        const declared = Number(req.headers['content-length']);
        if (Number.isFinite(declared) && declared > bodyCap) {
          return isHook ? json(res, 200, {}) : json(res, 413, { ok: false, reason: 'payload too large' });
        }
        req.on('data', d => {
          if (tooLarge) return;
          size += d.length; // d is a Buffer — byte length, not char count
          if (size > bodyCap) {
            tooLarge = true;
            // 413 on control paths; hooks keep the fail-open 200 {}. Stop
            // accumulating either way so the body can't grow without bound.
            return isHook ? json(res, 200, {}) : json(res, 413, { ok: false, reason: 'payload too large' });
          }
          chunks.push(d);
        });
        req.on('end', () => {
          if (tooLarge) return;
          const body = Buffer.concat(chunks).toString('utf8');
          let ev = {};
          try { ev = JSON.parse(body || '{}'); } catch {
            // hooks fail open: a bad body on a hook path is still 200 {}
            return isHook ? json(res, 200, {}) : json(res, 400, { err: 'bad json' });
          }
          try {
            const hook = /^\/hook\/([A-Za-z]+)$/.exec(url.pathname);
            if (hook) {
              const name = hook[1];
              // payload capture (validation aid): first 3 raw payloads per
              // hook event name, best-effort, never affects the response
              try { capture(name, ev); } catch { /* best-effort */ }
              // F3c CRITICAL (validated live on CLI 2.1.206):
              // AskUserQuestion rides the permission machinery — after the
              // /hook/AskUserQuestion hold resolves {}, the CLI fires
              // PermissionRequest for the SAME tool call. NEVER hold that
              // one: an unanswered question would chain two full hold
              // windows (~50 s each) before the terminal user ever sees the
              // chooser. Ingest telemetry, answer {} immediately.
              if (name === 'PermissionRequest' && ev?.tool_name === 'AskUserQuestion') {
                core.applyEvent({ ...ev, hook_event_name: 'PermissionRequest' });
                return json(res, 200, {});
              }
              if (name === 'PermissionRequest' || name === 'Elicitation' || name === 'AskUserQuestion') {
                return holdHook(res, ev, name); // Phase 3/4 hold-open relay
              }
              const handler = hookHandlers[name];
              if (!handler) {
                // unknown hook event: ingest telemetry anyway, respond no-op
                core.applyEvent({ hook_event_name: name, ...ev });
                return json(res, 200, {});
              }
              return json(res, 200, handler(ev) ?? {});
            }
            if (url.pathname === '/mail') {
              core.postMail(ev)
                .then(out => json(res, 200, out))
                .catch(err => {
                  console.error('fleetd mail error:', err);
                  json(res, 500, { ok: false, err: 'internal' });
                });
              return;
            }
            if (url.pathname === '/api/cleanup') {
              core.cleanup()
                .then(out => json(res, 200, out))
                .catch(err => {
                  console.error('fleetd cleanup error:', err);
                  json(res, 500, { ok: false, err: 'internal' });
                });
              return;
            }
            if (url.pathname === '/api/worktrees/remove') {
              // Security and data-loss gates live together in derive: only a
              // spawn-owned path reaches git, and force is an exact boolean.
              core.removeWorktree(ev)
                .then(out => json(res, out.status, out.body))
                .catch(err => {
                  console.error('fleetd worktree removal error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            if (url.pathname === '/command') return json(res, 200, core.command(ev.text));
            if (url.pathname === '/api/paste-image') {
              // v1.7 pasted image → file (paste.mjs). Same wall stack as every
              // control POST (auth → Host → CSRF → json content-type → body
              // cap); only the body cap is per-route (see MAX_PASTE_BODY). The
              // returned path is TYPED into the pane by the BOARD, not by us —
              // injection must ride TermPane's sendIn gate so the grid's
              // one-tile-types discipline also governs pastes.
              const out = core.pasteImage(ev);
              return json(res, out.status, out.body);
            }
            if (url.pathname === '/api/spawn') {
              // v1.2 board spawn (CONTRACT). Control API like the questions
              // answer path: real status codes, fail-loud — never a silent
              // no-op. The whole flow (validate → card → worktree → tmux →
              // row → nudge) lives in derive.mjs. v1.3 adds
              // dangerously_skip_permissions: bool and permission_mode
              // "bypassPermissions" (validated/applied in derive.spawn too).
              core.spawn(ev)
                .then(out => json(res, out.status, out.body))
                .catch(err => {
                  console.error('fleetd spawn error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const killMatch = /^\/api\/spawn\/([A-Za-z0-9-]+)\/kill$/.exec(url.pathname);
            if (killMatch) {
              // v1.2 name-verified kill: 404 unknown id, 409 card not offline
              // without force:true, 410 window already gone.
              core.spawnKill(killMatch[1], ev?.force === true)
                .then(out => json(res, out.status, out.body))
                .catch(err => {
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
              core.revive(reviveMatch[1], ev ?? {})
                .then(out => json(res, out.status, out.body))
                .catch(err => {
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
              core.adoptSession(adoptMatch[1], ev ?? {})
                .then(out => json(res, out.status, out.body))
                .catch(err => {
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
              const body = ev ?? {};
              const clearing = body.clear === true;
              if (!clearing && typeof body.suffix !== 'string') {
                return json(res, 400, { ok: false, reason: 'suffix must be a string (or pass {clear:true})' });
              }
              if (!clearing) {
                const bad = validateNameSuffix(body.suffix);
                if (bad) return json(res, 400, { ok: false, reason: bad });
              }
              const out = core.applyCustomName(nameMatch[1], clearing ? null : body.suffix);
              return json(res, out.ok ? 200 : 409, out);
            }
            const rcMatch = /^\/api\/spawn\/([A-Za-z0-9-]+)\/rc$/.exec(url.pathname);
            if (rcMatch) {
              // Explicit human board action: derive enforces the idle/live
              // pane boundary, types /rc literally, and waits for harvesting.
              core.enableRemote(rcMatch[1])
                .then(out => json(res, out.status, out.body))
                .catch(err => {
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
              const out = core.questions.answer(Number(answerMatch[1]), ev);
              return json(res, out.status, out.body);
            }
            const dismissMatch = /^\/api\/questions\/(\d+)\/dismiss$/.exec(url.pathname);
            if (dismissMatch) {
              // "I already handled this in the terminal." Retires the card and
              // tells the session NOTHING — unlike answer(), which mails it.
              const out = core.questions.dismiss(Number(dismissMatch[1]));
              return json(res, out.ok ? 200 : 404, out);
            }
            const planMatch = /^\/api\/plans\/(\d+)\/mark$/.exec(url.pathname);
            if (planMatch) {
              // v1.3 plan library mark (CONTRACT): {status:"executed"|"archived",
              // via?} — 404 unknown id, 409 bad transition. Matrix documented
              // at core.planMark (derive.mjs).
              const out = core.planMark(Number(planMatch[1]), ev);
              return json(res, out.status, out.body);
            }
            return json(res, 404, { err: 'nope' });
          } catch (err) {
            console.error('fleetd handler error:', err);
            // fail open on hook paths; visible error elsewhere
            if (url.pathname.startsWith('/hook/')) return json(res, 200, {});
            return json(res, 500, { err: 'internal' });
          }
        });
        return;
      }

      json(res, 404, { err: 'nope' });
    } catch (err) {
      console.error('fleetd request error:', err);
      try { json(res, String(req.url || '').startsWith('/hook/') ? 200 : 500, {}); } catch { /* socket gone */ }
    }
  });

  // ---------------------------------------------------------------- ws
  const wss = new WebSocketServer({ noServer: true });
  const termWss = new WebSocketServer({ noServer: true });
  const termbridge = createTermBridge({
    port,
    resolveSpawn: spawnId => core.terminalSpawn(spawnId),
    log: message => console.error(`fleetd ${message}`),
  });
  // ws re-emits http server errors (e.g. EADDRINUSE) on the wss; without a
  // listener that throws and masks the election exit-3 path in fleetd.mjs.
  wss.on('error', () => { /* the http server's 'error' listener owns this */ });
  termWss.on('error', () => { /* the http server's 'error' listener owns this */ });

  // Explicit upgrade routing keeps the snapshot socket's long-standing /ws
  // contract separate from /ws/term. Terminal query values are never tmux
  // targets: only the opaque spawn id reaches the core resolver.
  server.on('upgrade', (req, socket, head) => {
    let url;
    try { url = new URL(req.url || '/', 'http://127.0.0.1'); } catch { socket.destroy(); return; }
    // WS AUTH + CSRF CONTRACT: reject before either noServer WebSocketServer
    // sees the socket. A WebSocket is NOT subject to the same-origin READ
    // barrier, so a cross-site page could otherwise read the whole snapshot or
    // drive a live pane. Destroying the socket — no HTTP upgrade response —
    // guarantees nothing is observable through an unauthenticated OR
    // cross-origin connection; the Host check closes DNS rebinding (C1).
    if (!authorized(req, url) || !hostHeaderOk(req) || crossSiteReason(req)) { socket.destroy(); return; }
    if (url.pathname === '/ws') {
      wss.handleUpgrade(req, socket, head, ws => wss.emit('connection', ws, req));
    } else if (url.pathname === '/ws/term') {
      termWss.handleUpgrade(req, socket, head, ws => termWss.emit('connection', ws, req));
    } else {
      socket.destroy();
    }
  });
  // M-P1 coalescing: a mutation flips `dirty` and schedules at most one flush
  // per short window, so N updateSession() calls inside one hook collapse to a
  // single snapshot rebuild+stringify+send instead of N.
  let dirty = false;
  let flushTimer = null;
  // H-S1: the broadcast/connect snapshot deliberately uses core.snapshot() and
  // NOT snapshotWithLan() — the token-bearing lan.urls/lan.mdns must never ride
  // a frame a /ws client can read. The share URLs stay on GET /state, which is
  // token-gated in LAN mode (the board reads `lan` from its /state poll).
  function broadcast() {
    dirty = false;
    if (!wss.clients.size) return;
    const msg = JSON.stringify({ type: 'snapshot', ...core.snapshot() });
    for (const c of wss.clients) {
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
      if (c.bufferedAmount > MAX_WS_BUFFER) { try { c.terminate(); } catch { /* already gone */ } continue; }
      c.send(msg);
    }
  }
  function scheduleBroadcast() {
    dirty = true;
    if (flushTimer) return;
    flushTimer = setTimeout(() => {
      flushTimer = null;
      if (dirty) broadcast();
    }, BROADCAST_COALESCE_MS);
    flushTimer.unref?.();
  }
  wss.on('connection', ws => {
    ws.isAlive = true;
    ws.on('pong', () => { ws.isAlive = true; });
    try { ws.send(JSON.stringify({ type: 'snapshot', ...core.snapshot() })); } catch { /* client gone */ }
  });

  termWss.on('connection', async (ws, req) => {
    let handle = null;
    let socketClosed = false;
    ws.isAlive = true;
    ws.on('pong', () => { ws.isAlive = true; });
    // H-R3/M-P6 backpressure: a viewer that has stopped draining is EVICTED (a
    // 1009 close), not fed. Silently dropping pane bytes would desync its screen;
    // closing the socket unwinds its tmux subscription (the 'close' handler runs
    // handle.close()) so a slow viewer can never buffer a pane's whole output
    // into a dead socket.
    const send = frame => {
      if (ws.readyState !== 1) return;
      if (ws.bufferedAmount > MAX_TERM_WS_BUFFER) {
        try { ws.close(1009, 'terminal viewer too far behind'); } catch { /* already gone */ }
        return;
      }
      ws.send(JSON.stringify(frame));
    };
    ws.on('close', () => {
      socketClosed = true;
      handle?.close();
    });
    ws.on('message', raw => {
      if (!handle) return;
      // M-R4: a terminal frame is a keystroke or a modest paste — never a
      // megabyte. Refuse an oversized frame outright (1009) rather than expand it
      // to hex and queue it; termbridge.input() enforces the queued-byte bound.
      if (raw.length > MAX_TERM_FRAME_BYTES) { try { ws.close(1009, 'input frame too large'); } catch { /* already gone */ } return; }
      let frame;
      try { frame = JSON.parse(raw.toString('utf8')); } catch { return; }
      if (!frame || typeof frame !== 'object') return;
      if (frame.t === 'in' && typeof frame.data === 'string') handle.input(frame.data);
      else if (frame.t === 'resize') handle.resize(frame.cols, frame.rows);
    });

    try {
      const url = new URL(req.url || '/', 'http://127.0.0.1');
      const spawn_id = url.searchParams.get('spawn');
      const cols = Number(url.searchParams.get('cols'));
      const rows = Number(url.searchParams.get('rows'));
      if (!spawn_id) throw new Error('missing spawn id');
      // M-R5 abort path: if the socket closes mid-open (before `handle` exists),
      // openViewer() checks isAborted() between its awaits and bails, so the
      // half-opened viewer is removed instead of lingering counted forever.
      handle = await termbridge.openViewer({
        spawn_id, cols, rows, send,
        isAborted: () => socketClosed,
        onClose(reason) {
          send({ t: 'exit', reason });
          try { ws.close(); } catch { /* already gone */ }
        },
      });
      if (socketClosed) handle.close();
    } catch (err) {
      send({ t: 'err', reason: err?.reason || err?.message || 'terminal unavailable' });
      try { ws.close(); } catch { /* already gone */ }
    }
  });
  // H-R3 + M-P1: a real keepalive replaces the "full snapshot every 5 s"
  // heartbeat. Ping every peer on both servers; terminate any that missed the
  // previous pong. terminate() fires 'close', which unwinds a leaked /ws socket
  // and — for /ws/term — the viewer + (once the last leaves) the shared tmux
  // client, the exact leak a phone that dropped wifi used to cause.
  const keepalive = setInterval(() => {
    for (const server of [wss, termWss]) {
      for (const ws of server.clients) {
        if (ws.isAlive === false) { ws.terminate(); continue; }
        ws.isAlive = false;
        try { ws.ping(); } catch { /* reaped next round */ }
      }
    }
  }, WS_PING_MS);
  keepalive.unref();
  core.onMutate = scheduleBroadcast;

  // Only `server` is used externally (fleetd.mjs listens on it); wss/termWss/
  // broadcast stay internal.
  return { server };
}
