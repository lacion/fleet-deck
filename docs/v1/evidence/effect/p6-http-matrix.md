# P6.1 — Frozen HTTP/WS behavior characterization matrix

**Purpose.** This document freezes the exact wire-facing behavior of the daemon
HTTP + WebSocket surface *as implemented today*, so P6 transport work (the one
scoped `Bun.serve` HTTP adapter) can be verified against a fixed target. Every
row carries a `file:line` anchor into the current tree and a pointer to the
covering test file (or an explicit COVERAGE GAP).

**Provenance / how to read this.**

- **Anchor tree:** branch `fd/v1-effect-feasibility`, HEAD `67758ba9`
  (`docs(effect): record P5 completion`). That commit is docs-only; the
  implementation checkpoint is `ca62b94f` and `src/daemon/http.ts` is byte-identical
  at both. All `src/daemon/http.ts:N` anchors are valid at `67758ba9`.
- **All line anchors** below point at `src/daemon/http.ts` unless another file is
  named. `http.ts` is 3138 lines.
- **Empirical Bun 1.3.14 facts are CITED, not re-probed.** The authority is
  `memory/bun-serve-runtime-limits.md` (referenced in-code as
  `bun-serve-runtime-limits`) and `docs/v1/evidence/effect/p4.md` /
  `p4-shutdown.json`. Where a claim depends on a Bun runtime fact this doc says
  so and cites; it does not re-measure.
- **UNVERIFIED** marks any row whose exact value could not be pinned from source
  alone (would require a runtime probe or a test read not done in this pass). An
  UNVERIFIED row is acceptable; a wrong frozen row is not. UNVERIFIED rows are
  collected in §8.
- **"derive-owned"** means the HTTP layer only relays `{status, body}` from a
  `core.*` call (see the `ControlResult` type, `src/daemon/http.ts:496`); the
  status *enum* for such a route is a `derive` contract, not an `http.ts`
  constant. Those rows freeze the **transport** behavior (what http.ts does with
  the result); the enumerated sub-statuses are quoted from the route comment and
  marked as derive-owned, not independently re-derived here.

**Section inventory:** §1 route table (GET, POST, WS upgrade) · §2 body limit /
drain / FIN / timeout / disconnect · §3 WebSocket · §4 static assets · §5 hook
fail-open (canonical `200 {}`) conditions · §6 shutdown ordering · §7 coverage
gaps · §8 UNVERIFIED rows · §9 contradictions with the plan/status docs.

---

## 0. Global transport facts

| Fact | Value | Anchor | Notes |
| --- | --- | --- | --- |
| Server | one `Bun.serve({ port, hostname, idleTimeout:0, maxRequestBodySize:MAX_PASTE_BODY, fetch, websocket })` | `2721`–`2743` | Single handler for HTTP + WS. |
| Bind address | `hostname` = whatever `bindHttp(port, host)` is called with | `2723`, `2759` | Daemon entry binds `0.0.0.0` for LAN/Tailscale (memory `tailscale-lan-access`); LAN reachability is gated per-request by auth + Host allowlist, not by the bind. |
| `idleTimeout` | `0` (never sever an idle conn natively) | `2739` | Required so a held hook / watch long-poll survives its full wait; bounded idle is enforced **per-request** via `server.timeout(request,N)` instead (see §2). |
| Global body ceiling | `maxRequestBodySize = MAX_PASTE_BODY = 14e6` | `2740`, `46` | A body **over 14e6** gets Bun's bodyless fast-413 (`Connection: close`, ~3–7 ms) *before* `fetch` runs — CITED from `bun-serve-runtime-limits`. In-handler caps (§2) are stricter for non-paste routes. |
| node shims | `HttpReqShim` / `HttpResShim` wrap Bun `Request`/`Response` so the ~700-line synchronous router (`routeRequest`) stays byte-for-byte | `146`–`310`, `317`+ | The single Node affordance Bun lacks — per-socket close — is emulated as a `server.timeout()` FIN (§2). |
| `json(res,code,obj)` helper | body = `JSON.stringify(obj)`; headers `content-type: application/json`, `content-length: Buffer.byteLength(body)`, `x-content-type-options: nosniff`; `writeHead(code)` then `end(body)` | `778`–`788` | Every JSON response (success + error) goes through here → nosniff is universal on JSON. |
| `termAuth` (advertised on `/health`) | `{ term_token: !(proxyAuth === 'trust' \|\| trustLoopback) }` | `713`, `1462` | BUG-186 capability probe for `/ws/term`. |

### Auth model (referenced by the route tables)

| Helper | Behavior | Anchor |
| --- | --- | --- |
| `isLoopbackAddress` | localhost, `::1`, `127/8`, `::ffff:127/8` | `527` |
| `authorized(req,url)` | `/hook/*` authenticated **unconditionally** (guard leads the loopback block); loopback block: `proxyAuth==='token'&&viaProxy` → fall through to token; `proxyAuth==='trust'&&viaProxy` → `true`; else plain-loopback: `/health`\|\|`isPublicShell` → `true`, then `!requireToken && (trustLoopback \|\| !tokenGatedRoute)` → `true`; final fallback = bearer `Authorization: Bearer <t>` or `?t=` via `tokenMatches` | `835`–`894` |
| `tokenMatches` | length check then `timingSafeEqual` | `809`–`814` |
| `tokenGatedRoute` | `true` for `/ws/term` (any method), `POST /mail`, `POST /api/spawn/arm-unsupervised` | `904`–`908` |
| `hostHeaderOk` | missing Host → `true`; else `hostAllowed \|\| authorityTrusted` (DNS-rebinding wall) | `1066` |
| `crossSiteReason` | `sec-fetch-site` cross-site/cross-origin → reason; bad Origin → `'bad-origin'`; Origin not allowed/trusted → `'cross-origin'`; else `null` (CSRF wall) | `1082` |
| `isPublicShell` | GET & (`/`, `/index.html`, `/favicon.ico`, `/assets/*`) | `1398`–`1403` |

**Auth classes used in the tables:**
- **shell** — no auth, no Host wall, no CSRF wall (`isPublicShell`; walls are skipped when `shell`, `1418`/`1429`).
- **loopback-open** — open on loopback (default `requireToken=off`), bearer required off-loopback (LAN); Host wall applies.
- **token-gated** — bearer required even on loopback unless `trustLoopback` (`tokenGatedRoute`, or an in-handler gate).
- **hook** — `/hook/*`, authenticated unconditionally; a tokenless/failed hook is answered with the canonical silent `200 {}` (fail-open), never 401.

---

## 1. Route table

### 1a. GET routes (dispatched in `routeRequest`, `req.method === 'GET'`, `1434`–`1576`)

Pre-route walls for every non-shell GET: unauthorized → `401 {ok:false,reason:'unauthorized'}` (`1418`–`1419`); Host wall fail → `403 {ok:false,reason:'forbidden'}` (`1429`–`1433`).

| # | Path | Auth | CSRF wall | Sync/async | Success | Error(s) | Anchor | Covering test |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| G1 | `/health` | loopback-open; bearer on LAN | no | sync | `200 {ok:true, fleet, pid, version, managed, spawn, auth:termAuth, startup}` | 401/403 walls | `1450`–`1478` | `security-headers.test.ts` (nosniff+json), `lan-auth.test.ts`, `loopback-gates.test.ts` |
| G2 | `/state` | loopback-open; bearer on LAN | no (walled by Host allowlist) | sync | `200 snapshotWithLan()` — **includes token-bearing `lan.urls`/`lan.mdns`** | 401/403 walls | `1479`–`1481`, `771` | `static-serving.test.ts` (regression), `lan-auth.test.ts` |
| G3 | `GET /api/settings` | loopback-open; bearer on LAN | no | sync | `200 {ok:true, settings: core.resolveSettings()}` | 401/403 walls | `1483`–`1485` | `smoke-settings.test.ts`, `gateway.test.ts` (domain) |
| G4 | `/api/worktrees` | loopback-open; bearer on LAN | no | **async** (relays `core.worktrees()`) | `200 <out>` | **fail-open**: `.catch` → `200 {ok:true, worktrees:[]}` | `1487`–`1500` | `worktrees.test.ts` (domain) |
| G5 | `/api/sessions/:sid/fs/(list\|read\|search)` | loopback-open; bearer on LAN | no | async (`settleFilesystemOperation`) | `200 <op.status/body>` | `.catch` → `500 {ok:false, reason:'internal'}` | `1502`–`1517`, `790`–`803` | `session-fs.test.ts` (domain) |
| G6 | `/api/fs/(list\|read\|search)` (home) | loopback-open; bearer on LAN | no | async (`settleFilesystemOperation`) | `200 <op.status/body>` | `.catch` → `500 {ok:false, reason:'internal'}` | `1519`–`1531` | `session-fs.test.ts` (domain) |
| G7 | `GET /mail` | loopback-open; bearer on LAN | **yes** → `403 {ok:false,reason:'forbidden'}` | sync (mutates: drains+leases) | `200 {mail, ack_mail_ids}` (+ broadcast if box non-empty) | 401/403 walls | `1446`–`1449`, `1533`–`1548` | `mail-delivery-lease.test.ts`, `mail-frames.test.ts`, `csrf-guard.test.ts` |
| G8 | `GET /api/watch` | loopback-open; bearer on LAN | **yes** → `403` | async long-poll (`watchHook`) | **always `200` JSON** (`{status, session_alive, pending, ...}`); `hold_ms` 0..25000 | 401/403 walls | `1446`–`1449`, `1550`–`1552`, `1304`–`1372` | `watch-rewake.test.ts`, `csrf-guard.test.ts` |
| G9 | `/favicon.ico` | shell (public) | no | sync | `204` empty, `cache-control: no-store` | — | `1554`–`1563` | `static-serving.test.ts` |
| G10 | shell (`/`, `/index.html`, `/assets/*`) | shell (public) | no | sync | `200` board asset (see §4) | notFound → `404 {err:'nope'}` | `1565`–`1573`, `572`–`616` | `static-serving.test.ts`, `security-headers.test.ts` |
| G11 | any other GET | (walls apply) | no | sync | — | `404 {err:'nope'}` | `1575` | `static-serving.test.ts` (missing asset) |

### 1b. POST routes (`req.method === 'POST'`, `1579`+)

Pre-route walls for every POST: CSRF → hook `200 {}` else `403 {ok:false,reason:'forbidden'}` (`1586`–`1590`); content-type wall (hooks exempt) → `415 {ok:false,reason:'expected application/json'}` (`1595`–`1598`); body cap (§2); bad JSON → hook `200 {}` else `400 {err:'bad json'}` (`1650`–`1656`). Every control POST is also subject to the unauthorized `401` and Host-wall `403` above (`1418`, `1429`).

| # | Path | Auth | Sync/async | Success | Error(s) | Anchor | Covering test |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | `/hook/:Name` | hook (unconditional) | mixed (dispatch table sync; holds async) | see §5 | fail-open `200 {}` for every refusal | `1658`–`1722` | `hook-auth.test.ts`, `hook-compat-silence.test.ts`, `hook-output.test.ts`, `hook-missing-session-id.test.ts` |
| P2 | `/mail/ack` | loopback-open; bearer on LAN | sync | `200 {ok:true, ...core.ackMail([mail_id])}` | walls | `1727`–`1731` | `mail-delivery-lease.test.ts` |
| P3 | `POST /mail` | **token-gated** (bearer even on loopback unless `trustLoopback`) | **async** (`core.postMail`) | `200`/`out.status` + `out.body` (adapter: bare success shape) | `.catch` → `500 {ok:false, err:'internal'}`; refusal `{status,body}` from core | `1732`–`1746`, `904`–`908` | `mail-delivery-lease.test.ts`, `mail-and-blocking.test.ts`, `loopback-gates.test.ts` |
| P4 | `/api/cleanup` | loopback-open; bearer on LAN | **async** (`core.cleanup`) | `200 <out>` | `!out.ok` → `409 <out>`; `.catch` → `500 {ok:false, err:'internal'}` (BUG-145) | `1747`–`1761` | `cleanup-api.test.ts`, `audit-cleanup.test.ts` |
| P5 | `/api/worktrees/remove` | loopback-open; bearer on LAN | **async** (`core.removeWorktree`) | `<out.status> <out.body>` | `.catch` → `500 {ok:false, reason:'internal'}` | `1762`–`1775` | `worktrees.test.ts` (domain) |
| P6 | `POST /api/settings` | loopback-open **+ gateway_* bearer gate** | sync (`core.setSettings`) | `<out.status> <out.body>` | `gateway_*` key without bearer (unless `trustLoopback && !viaProxy && loopback`) → `401 {ok:false, reason:'gateway settings require the bearer token'}` | `1776`–`1811` | `gateway.test.ts`, `settings-transaction.test.ts` (domain) |
| P7 | `/command` | loopback-open; bearer on LAN | sync | `200 core.command(text)` | walls | `1812`–`1815` | `fleet-command.test.ts` (domain) |
| P8 | `/api/paste-image` | loopback-open; bearer on LAN | sync (`core.pasteImage`) | `<out.status> <out.body>` | body cap = **MAX_PASTE_BODY (14e6)** (§2) | `1816`–`1826`, `1607` | `paste-image.test.ts` |
| P9 | `/api/spawn/arm-unsupervised` | **token-gated** (bearer even on loopback) | sync | `200 {ok:true, arm_token: core.armUnsupervised()}` | walls | `1827`–`1835`, `904`–`908` | `arm-gate.test.ts`, `spawn-unsupervised.test.ts` |
| P10 | `/api/repos/preflight` | loopback-open; bearer on LAN | **async** (`core.preflightRepo`) | `<out.status> <out.body>` | `repo`/other keys not string → `400 {ok:false, reason:'... must be a string'}`; `.catch` → `500 {ok:false, reason:'Git access check failed internally'}` | `1836`–`1864` | `repos.test.ts` (domain) |
| P11 | `POST /api/spawn` | loopback-open (unsupervised body needs arm_token, derive-checked) | **async** (`core.spawn`) | `<out.status> <out.body>` (contract: `202` on accept) | non-object body → `400 {ok:false, reason:'spawn body must be a JSON object'}`; `.catch` → `500 {ok:false, reason: spawnFailureReason(err)}` | `1865`–`1924` | `spawn.test.ts`, `spawn-repo.test.ts`, `spawn-unsupervised.test.ts` (domain) |
| P12 | `/api/spawn/:id/kill` | loopback-open; bearer on LAN | **async** (`core.spawnKill`) | `<out.status> <out.body>` (derive: 404 unknown / 409 not offline w/o force / 410 gone) | `.catch` → `500 {ok:false, reason:'internal'}` | `1925`–`1939` | `spawn.test.ts` (domain) |
| P13 | `/api/spawn/:id/revive` | loopback-open; bearer on LAN | **async** (`core.revive`) | `<out.status> <out.body>` | `.catch` → `500 {ok:false, reason:'internal'}` | `1940`–`1956` | `revive.test.ts` (domain) |
| P14 | `/api/sessions/:sid/adopt` | loopback-open (unsupervised body derive-checked) | **async** (`core.adoptSession`) | `<out.status> <out.body>` (derive: 404/400/409/410) | `.catch` → `500 {ok:false, reason:'internal'}` | `1957`–`1997` | `adopt.test.ts` (domain) |
| P15 | `/api/sessions/:sid/name` | loopback-open; bearer on LAN | sync (`core.applyCustomName`) | `out.ok ? 200 : 409` `<out>` | **`suffix` validated BEFORE unknown-id:** malformed/missing suffix → `400 {ok:false, reason:"suffix must be a string (or pass {clear:true})"}` (`2008`–`2013`); `validateNameSuffix` fail → `400 {ok:false, reason:bad}` (`2015`–`2021`); unknown-id (valid `{suffix:'x'}`) → `409` (`2023`–`2027`) | `1998`–`2029` | `rename.test.ts` (domain); unknown-id 409: `tests/p6-http-freeze.test.ts` |
| P16 | `/api/sessions/:sid/dismiss` | loopback-open; bearer on LAN | **async** (`core.dismissSession`) | `<out.status> <out.body>` (derive: 404/409) | `.catch` → `500 {ok:false, reason:'internal'}` | `2030`–`2048` | `dismiss.test.ts` (domain) |
| P17 | `/api/sessions/:sid/dismiss/retry` | loopback-open; bearer on LAN | **async** (`core.dismissRetry`) | `<out.status> <out.body>` (BUG-145 idempotent) | `.catch` → `500 {ok:false, reason:'internal'}` | `2052`–`2067` | `dismiss.test.ts` (domain) |
| P18 | `/api/spawn/:id/rc` | loopback-open; bearer on LAN | **async** (`core.enableRemote`) | `<out.status> <out.body>` | `.catch` → `500 {ok:false, reason:'internal'}` | `2068`–`2082` | `spawn.test.ts` / `needs-you.test.ts` (domain) |
| P19 | `/api/questions/:n/answer` | loopback-open; bearer on LAN | sync (`core.questions.answer`) | `<out.status> <out.body>` | (derive-owned) | `2083`–`2095` | `choice-relay.test.ts`, `needs-you.test.ts` (domain) |
| P20 | `/api/questions/:n/dismiss` | loopback-open; bearer on LAN | sync (`core.questions.dismiss`) | `out.ok ? 200 : 404` `<out>` | — | `2096`–`2103` | `question-rearm.test.ts`, `questions-audit.test.ts` (domain) |
| P21 | `/api/plans/:n/mark` | loopback-open; bearer on LAN | sync (`core.planMark`) | `<out.status> <out.body>` (derive: 404/409) | (derive-owned) | `2104`–`2115` | `plans.test.ts`, `accept-plan-mark.test.ts` (domain) |
| P22 | `/api/plans/:n/assign` | loopback-open; bearer on LAN | sync (`core.assignPlan`) | `<out.status> <out.body>` (derive: 404/409) | (derive-owned) | `2116`–`2129` | `plans.test.ts` (domain) |
| P23 | any other POST | (walls apply) | sync | — | `404 {err:'nope'}` | `2130` | — |

**POST inner catch** (`2132`–`2141`): any thrown error → hook path `200 {}`, else `500 {err:'internal'}`.
**Whole-handler outer catch** (`2147`–`2154`): thrown error → `/hook/*` `200 {}` else `500 {}`.

### 1c. WebSocket upgrade (`handleUpgrade`, `2473`–`2517`; entered from `fetchHandler` on `Upgrade: websocket`, `2550`–`2554`)

| Condition | Result | Anchor |
| --- | --- | --- |
| `quiescing` | `new Response(null, {status:503})` (no FIN arm) | `2474` |
| `!authorized(req,url) \|\| !hostHeaderOk(req) \|\| crossSiteReason(req)` | `refuse(401)` — arms `srv.timeout(request, KEEPALIVE_FIN_S)` then `Response(null,401)` | `2485`–`2499` |
| `/ws` | `srv.upgrade(request,{data:{kind:'snapshot',isAlive:true}}) ? undefined : refuse(400)` | `2500`–`2503` |
| `/ws/term` | parse `spawn`/`cols`/`rows` from query; `srv.upgrade(...) ? undefined : refuse(400)` | `2504`–`2515` |
| any other path | `refuse(404)` | `2516` |

**Auth at upgrade:** `/ws/term` is `tokenGatedRoute` (bearer even on loopback unless `trustLoopback`); `/ws` follows ordinary loopback-open rules. Both go through the same `authorized + hostHeaderOk + crossSiteReason` gate — a WS is not subject to the browser same-origin READ barrier, so the CSRF/Host walls are the defense (`2493`–`2497`). Covering: `ws-hardening.test.ts` (H-S1 tokenless snapshot), `terminal-ws.test.ts`, `csrf-guard.test.ts`, `loopback-gates.test.ts`.

---

## 2. Body limit, drain, FIN, request timeout, disconnect

**Constants** (all `src/daemon/http.ts`):

| Constant | Value | Env override | Anchor |
| --- | --- | --- | --- |
| `MAX_BODY` | `1e6` | — | `40` |
| `MAX_PASTE_BODY` | `14e6` | — | `46` |
| `BODY_DRAIN_GRACE_MS` | `1000` | — | `57` |
| `BODY_STALL_FIN_S` | `120` (0/neg → 120) | `FLEETDECK_STALL_FIN_S` (`>0?n`) | `72`–`75` |
| `KEEPALIVE_FIN_S` | `120` (0/neg → 120; clamp `min(n,255)`) | `FLEETDECK_KEEPALIVE_FIN_S` | `94`–`97` |
| `maxRequestBodySize` (Bun global) | `MAX_PASTE_BODY` = `14e6` | — | `2740` |

**Body cap layering (per POST):**

| Layer | Trigger | Behavior | Anchor |
| --- | --- | --- | --- |
| Bun global | body **> 14e6** on ANY route | bodyless fast-413 `Connection: close` ~3–7 ms, **before `fetch` runs** — CITED `bun-serve-runtime-limits` | `2740` |
| Content-Length pre-check | declared `content-length` > `bodyCap` | `refuseOversize()` before reading a byte | `1628`–`1632` |
| Streaming cap | accumulated bytes > `bodyCap` on `'data'` | set `tooLarge`, `refuseOversize()`, stop accumulating | `1633`–`1644` |
| `bodyCap` | `/api/paste-image` → `MAX_PASTE_BODY` (14e6); else `MAX_BODY` (1e6) | — | `1607` |
| `refuseOversize()` | — | `res.shouldKeepAlive=false`; hook → `200 {}`, else `413 {ok:false, reason:'payload too large'}` | `1620`–`1624` |

**FIN / request-timeout state machine** (`HttpResShim._finKind`, `146`–`303`; Bun has no per-socket close, so a per-request `server.timeout(request,N)` idle FIN substitutes — CITED `bun-serve-runtime-limits`):

| Transition | `server.timeout` call | `_finKind` | Effect | Anchor |
| --- | --- | --- | --- | --- |
| `set shouldKeepAlive=false` (refuse path) | `timeout(request, 1)` | `refuse` | uSockets 4 s granularity → **effective ~4 s FIN** (CITED); body/413 still flushes immediately | `262`–`271` |
| `boundStalledDrain()` (drain grace expired, body un-drained) | `timeout(request, BODY_STALL_FIN_S)` — only if `_finKind==='none'` | `stall` | reaps a withheld/stalled in-flight body (default 120 s idle bound) | `278`–`286`, `2641`–`2643` |
| `clearStalledFin()` (body drained cleanly after grace) | `timeout(request, 0)` — only if `_finKind==='stall'` | `none` | retracts the stall FIN; slow-but-real upload / held long-poll unbounded again | `295`–`303`, `2654`–`2656` |
| `end()` (response completed) | `timeout(request, KEEPALIVE_FIN_S)` — only if `_finKind==='none'` | `keepalive` | reaps between-requests idle socket (client made one request then vanished) | `215`–`222` |
| `fetchHandler` entry (each new in-flight request) | `timeout(request, 0)` | — | clears a reused socket's prior keepalive FIN so active requests / held long-polls run under idleTimeout:0 | `2571`–`2575` |
| `handleUpgrade` refuse path | `timeout(request, KEEPALIVE_FIN_S)` | — | every WS refusal bypasses `end()`, so arm FIN here (an *attempted-and-failed* `srv.upgrade` disarms Bun's ~12 s linger reaper — CITED) | `2485`–`2492` |
| `fetchHandler` URL-parse catch | `timeout(request, KEEPALIVE_FIN_S)` | — | 400 bypasses `end()` | `2531`–`2536` |

**Guards:** `boundStalledDrain` and `end()` never overwrite an already-armed FIN; `clearStalledFin` retracts ONLY a `stall` FIN (a `refuse` FIN is left intact so BUG-125's prompt close is never lengthened to 120 s) — `159`–`163`, `279`, `296`.

**Drain-then-respond** (`drainThenRespond`, `2628`–`2658`; keep-alive socket sync — Bun reuses sockets with no per-conn close):
- Bun is handed the Response only after the request body drains (or `BODY_DRAIN_GRACE_MS` elapses), so an early 4xx that abandons the body cannot desync the next request on a reused socket.
- A present body drains in ~ms and wins immediately; a declared-then-withheld body parks the drain, capped at 1000 ms grace → `boundStalledDrain()` + `finish()`.
- A cleanly-drained body (`!req.destroyed`) calls `clearStalledFin()`; a faulted body keeps the FIN. `_pump` swallows a mid-stream read error into a *resolution* (sets `req.destroyed`), not a rejection (`2646`–`2657`).
- Held responses (hook hold / watch long-poll) resolve `res.done` long after their small body drains, so the grace never gates them.

**Disconnect behavior per route class:**

| Route class | Peer disconnect handling | Anchor |
| --- | --- | --- |
| Any active response | `res.on('close')` tracked in `activeResponses`; during shutdown → `forceFaultedResponseDuringShutdown` (queued via microtask) | `2581`–`2598` |
| Held hook (Permission/Elicitation/AskUserQuestion) | `holdHook` parks response; `res.on('close')` → `socketClosed` (releases the hold) | `1210`–`1247` |
| Watch long-poll | `watchHook` registers `addWatchWaiter`/`activeWatchClosers`; settled as idle watcher on close-clients phase | `1304`–`1372`, `3016`–`3022` |
| `/ws` snapshot | `close`: delete from `snapshotClients`; on 1→0 transition `core.questions.failOpenAllHolds()` | `2312`–`2331` |
| `/ws/term` | `close`: delete; `data.abort.closed=true`; `handle?.close()` | `2332`–`2337` |

**Request-timeout policy summary:** there is **no wall-clock request-timeout**; the only bounds are the idle FINs above (all reset by inbound data — an idle clock, not an absolute deadline) and Bun's fixed ~12 s reaper for the pre-`fetch` header phase (out of the daemon's reach — CITED `bun-serve-runtime-limits`). Covering: `http-stall.test.ts` (A1 stalled-FIN, A1-retraction clean-drain, C keep-alive-idle FIN, C held-long-poll-reused-socket, C refused-WS-upgrade FIN), `csrf-guard.test.ts` (M-B3 byte-cap), `paste-image.test.ts` (paste body cap).

---

## 3. WebSocket

**Two logical servers on one Bun `websocket` handler**, dispatched on `ws.data.kind` (`snapshot` = `/ws`, `term` = `/ws/term`) — `2252`–`2341`.

### 3a. Upgrade
See §1c. Auth/CSRF/Host at upgrade time (`2493`–`2515`); subprotocols: **none negotiated** (`srv.upgrade(request,{data})` passes no `protocol`). `/ws/term` query params `spawn`, `cols`, `rows` parsed at upgrade and stashed on `ws.data` (`2504`–`2513`).

### 3b. Snapshot server (`/ws`)

| Behavior | Detail | Anchor |
| --- | --- | --- |
| Frame content | `wsSnapshot()` = `{type:'snapshot', ...core.snapshot(), legacy_upgrade: legacyBanner()}` — **`core.snapshot()`, NOT `snapshotWithLan()`**: token-bearing `lan.urls`/`lan.mdns` never ride a `/ws` frame (H-S1) | `2200`–`2210` |
| On open | `quiescing` → terminate; else `isAlive=true`, add to `snapshotClients`, send one snapshot | `2258`–`2277` |
| Broadcast | coalesced (`scheduleBroadcast`, 60 ms `BROADCAST_COALESCE_MS`); per client if `readyState!==1` skip; if `getBufferedAmount() > MAX_WS_BUFFER` → `c.terminate()` (force reconnect + fresh snapshot); else `c.send(msg)` | `2211`–`2251`, `2227`–`2235` |
| Backpressure | `getBufferedAmount()` vs `MAX_WS_BUFFER` (1 MiB, env `FLEETDECK_WS_BUFFER_MAX`). **`send()` return value is discarded** (`2235`) | `107`–`110`, `2227` |
| On close | delete; on 1→0 `core.questions.failOpenAllHolds()` | `2312`–`2331` |

### 3c. Terminal server (`/ws/term`)

| Behavior | Detail | Anchor |
| --- | --- | --- |
| On open | `quiescing` → `abort.closed=true`+terminate; else add to `termClients`, `openTerm(ws)` | `2258`–`2279` |
| Open viewer | async `termbridge.openViewer({spawn_id,cols,rows,send,isAborted,onClose})`; err → `send({t:'exit'\|'err', reason})` then `ws.close()`; `gone` err → `{t:'exit'}` + liveness reconcile | `2363`–`2442` |
| Inbound frame | normalize to string; **`Buffer.byteLength > MAX_TERM_FRAME_BYTES` (1 MiB) → `ws.close(1009,'input frame too large')`**; JSON frames `t:'in'`/`'paste'` (string `data`), `t:'resize'` (`cols`,`rows`) | `2281`–`2311`, `124` |
| Outbound frame | `sendTermFrame`: `quiescing`/`readyState!==1` guard; **`getBufferedAmount() > MAX_TERM_WS_BUFFER` (4 MiB) → `ws.close(1009,'terminal viewer too far behind')`**; else `ws.send`. **`send()` return discarded** (`2358`) | `2347`–`2359`, `125` |
| On close | delete; `abort.closed=true`; `handle?.close()` | `2332`–`2337` |

### 3d. Heartbeat / liveness (both servers)

| Behavior | Detail | Anchor |
| --- | --- | --- |
| Cadence | `setInterval(WS_PING_MS = 30_000)`, `.unref()`; replaces the old "snapshot every 5 s" | `2448`–`2465`, `116` |
| Liveness | per peer: `!isAlive` → `ws.terminate()`; else `isAlive=false` + `ws.ping()` | `2452`–`2462` |
| Pong | `pong` handler sets `ws.data.isAlive=true` | `2338`–`2340` |

### 3e. Close codes

| Code | Meaning | Anchor |
| --- | --- | --- |
| `1009` | oversized inbound terminal frame ("input frame too large") | `2294` |
| `1009` | terminal viewer too far behind (send buffer > 4 MiB) | `2352` |
| `ws.close()` (no code) | term viewer open error / `onClose` | `2397`, `2431` |
| `terminate()` (abrupt, no close frame) | snapshot backpressure eviction; keepalive reap; quiescing-open; shutdown finalize | `2229`, `2453`, `2263`/`2369`, `2831` |

### 3f. `send()` return values — **NOT consulted anywhere**

All three `.send()` sites discard the return value; backpressure is exclusively `getBufferedAmount()`:

| Site | Code | Anchor |
| --- | --- | --- |
| broadcast | `c.send(msg)` | `2235` |
| open-snapshot | `ws.send(JSON.stringify(wsSnapshot()))` | `2272` |
| `sendTermFrame` | `ws.send(JSON.stringify(frame))` | `2358` |

This directly contradicts the P6.5 plan/status assumption — see §9.1. Covering: `ws-hardening.test.ts` (R1-2 buffer-cap TERMINATE, M-P1 coalesce, BUG-066, H-S1, BUG-031), `terminal-ws.test.ts` (24 tests incl. 1009 paths, heartbeat).

---

## 4. Static asset serving (`serveBoardAsset`, `572`–`616`; `BOARD_DIST` at `src/daemon/board-dist`)

| Behavior | Detail | Anchor |
| --- | --- | --- |
| Traversal safety | `path.resolve` must stay inside `BOARD_DIST` else `notFound()` | `572`–`616` |
| Content-Type | `MIME[ext] ?? 'application/octet-stream'` | `544`–`557` |
| MIME table | includes `.html→text/html`, `.js→text/javascript`, `.css→text/css`, etc. | `544`–`557` |
| Universal headers | `content-length`, `x-content-type-options: nosniff`, `referrer-policy: no-referrer` | `572`–`616` |
| Cache-Control | `.html` → `no-store`; else `public, max-age=31536000, immutable` | `572`–`616` |
| CSP | `CSP_SHELL` on `.html` **only** (subresources inherit) | `566`–`567`, `572`–`616` |
| Status | `writeHead(200)` on hit; miss → `notFound` callback (routeRequest passes `json(404,{err:'nope'})`) | `1572` |
| `/favicon.ico` | handled before shell serve → `204` + `no-store` (board-dist ships no favicon; data: SVG in CSP) | `1554`–`1563` |

**Pinned CSP** (`security-headers.test.ts` `EXPECTED_CSP`):
`default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; connect-src 'self'; img-src 'self' data: blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`.

Covering: `security-headers.test.ts` (CSP/nosniff/cache-control/referrer-policy, pinned exactly), `static-serving.test.ts` (board, `/index.html`==`/`, assets MIME, 204 favicon, traversal raw + percent-encoded 404, no leak).

---

## 5. Hook fail-open — every path yielding canonical `200 {}`

`/hook/*` never 401s, never surfaces an error into Claude; every refusal is HTTP `200 {}`. Sources:

| Condition | Site | Anchor |
| --- | --- | --- |
| CSRF wall fail on a hook POST | `if (isHook) json(res,200,{})` | `1586`–`1588` |
| Host-wall fail on a hook | `if (/hook/) json(res,200,{})` | `1430` |
| Oversized hook body (`refuseOversize`) | `if (isHook) json(res,200,{})` | `1622` |
| Bad JSON on a hook | `if (isHook) json(res,200,{})` | `1652`–`1653` |
| Tokenless / wrong-token hook (`silentHookRefusal`) | `noteLegacySession` + `json(res,200,{})` | `949`–`954`, `1664`–`1667` |
| `PermissionRequest` + `tool_name === 'AskUserQuestion'` pairing | `applyEvent` then `json(res,200,{})` immediately | `1683`–`1690` |
| Unknown hook event name | `applyEvent(...)` then `json(res,200,{})` | `1700`–`1704` |
| Hook body failing `validateHookEvent` (missing/blank `session_id`, non-object) | `json(res,200,{})` no dispatch | `1716`–`1719` |
| Known hook handler | `json(res,200, handler(ev) ?? {})` | `1720` |
| `Notification` / `FileChanged` / `CwdChanged` handlers | return `{}` (FileChanged returns `{}` WITHOUT ingest — v0.22.5 hotfix) | `1169`–`1203` |
| `holdHook` quiescing / intake error / null row | `200 {}` | `1210`–`1247` |
| POST inner catch on `/hook/*` | `json(res,200,{})` | `2135`–`2136` |
| Outer handler catch on `/hook/*` | `json(res,200,{})` | `2150` |
| `fetchHandler` quiescing, hook path | `Response('{}', {status:200, ...json headers})` | `2538`–`2548` |

Covering: `hook-auth.test.ts` (fail-open dialect, forgery refusal, `fleet-hook.mjs` shim), `hook-compat-silence.test.ts`, `hook-missing-session-id.test.ts`, `hook-output.test.ts`, `filechanged-watch.test.ts`.

---

## 6. Shutdown ordering (as implemented; empirical timings CITED from P4)

**In-code lifecycle** (`http.ts` `lifecycle` object, `3092`–`3111`; exposed as `quiesce`, `beginGracefulStop`, `forceStop`, `releaseHolds`, `closeClients`, `forceClients`, `close`, `isQuiescing`, `ownedCounts`):

| Phase / fn | Behavior | Anchor |
| --- | --- | --- |
| `quiesceHttp` | `quiescing=true`; detach `core.onMutate`; `clearInterval(keepalive)`; clear flush timer; force-settle body-faulted/disconnected `activeResponses`; set `gracefulStartBarrier`; `void startGracefulStop()` (never awaited here) | `2916`–`2959` |
| `releaseHeldResponses` | `questionsLifecycle.releaseAll?.() ?? failOpenAllHolds()`; barrier on all `hook` `activeResponses.promise` → `resolveHoldsReleased` (Bun 1.3.14 can reset a held-hook socket if `stop(false)` begins before the released response crosses the fetch Promise boundary) | `2961`–`2981` |
| `startGracefulStop` | await `[gracefulStartBarrier, holdsReleased]` → `Bun.sleep(0)` → if `forceStopPromise` join it & return; else `await live.stop(false)`, null `bunServer`, `finalizeNativeClients` | `2841`–`2880` |
| `beginNativeClientClose` | retire application WS handles (`abort.closed`, `handle?.close()`) but **do NOT close/terminate the native `ServerWebSocket` before `stop(true)`** (Bun 1.3.14 can wedge the stop Promise forever) | `2983`–`2995` |
| `closeClientsOnce` | settle watch pollers as idle; `forceFaultedResponseDuringShutdown` each; `beginNativeClientClose`; `termbridge.close()`; await responses + terminal opens + bridge | `3010`–`3050` |
| `forceStopHttp` | `live.stop(true)` — **does NOT await `gracefulStopPromise`**; null `bunServer`, `finalizeNativeClients` | `2887`–`2914` |
| `closeHttpOnce` | `releaseHeldResponses` → `closeClientsHttp` → `forceStopHttp` → `finalizeNativeClients` → clear error listeners | `3057`–`3085` |
| `finalizeNativeClients` | terminate all closing/snapshot/term sockets; clear sets | `2823`–`2839` |

**Established Bun 1.3.14 ordering invariants preserved (do not re-derive):**
1. Held HTTP hook + active board WS → canonical `200 {}` delivered **before** transport close (held responses must cross the fetch Promise boundary before `stop(false)`).
2. Never `close()`/`terminate()` a native `ServerWebSocket` before `stop(true)` — else the stop Promise wedges permanently (`2863`–`2864`, `2984`–`2987`).
3. `stop(false)` (graceful) and `stop(true)` (force) are independently-awaitable; force never awaits graceful; graceful joins an in-flight force instead of racing it (`2860`–`2868`, `2897`–`2899`).

**Empirical shutdown evidence (CITED, historical P4 — `docs/v1/evidence/effect/p4.md`, `p4-shutdown.json`, checkpoint `661dfe3`, Darwin/M4 Pro, `fleetdeck@0.23.6`):** 1750 ms whole-daemon deadline; ordinary takeover 2000 ms / managed stop 3000 ms budget; 250 ms force reserve; 8-phase `LifecycleCoordinator`; measured forced-hook p95 = 223.079 ms; 100 measured shutdowns passed all residue gates. **This measures the pre-P5 tree — do not relabel it as measuring the current tree** (per `effect-migration-status.md` §"P4 evidence remains historical"). Covering (behavioral, not timing): `http-lifecycle.test.ts` (held-hook `200 {}`, WS close, force), `daemon-lifecycle-integration.test.ts`, `daemon-resources.test.ts`.

---

## 7. Coverage gaps (original inventory, now dispositioned)

The ten items below are the original P6.1 coverage-gap list, preserved
verbatim. Each now carries a **Disposition** folded back from
`tests/p6-http-freeze.test.ts` (gaps 1, 3, 4, 6, 7), from an existing pin
(gap 5), or left OPEN (gaps 2, 8, 10). Gap 9 remains the recorded meta-note.

1. **Derive-owned control-route error status enums not confirmed at the transport layer.** For P5, P10–P18, P19, P21, P22 the http layer relays `core.*` `{status,body}` (verified). The specific status codes quoted in route comments (kill 404/409/410; adopt 404/400/409/410; plan 404/409; etc.) are **derive** contracts; I did not read `spawn.test.ts`/`adopt.test.ts`/`revive.test.ts`/`dismiss.test.ts`/`rename.test.ts`/`plans.test.ts`/`repos.test.ts`/`worktrees.test.ts`/`settings-transaction.test.ts` in this pass, so which exact transport statuses each asserts is **UNCONFIRMED**. The relay behavior itself (http hands `out.status`/`out.body` through, `.catch`→500) is verified from source. **Disposition: COVERED** by `tests/p6-http-freeze.test.ts` (unknown-id statuses recorded in §8).
2. **`.catch` → 500 branches** on every async control route (P3–P5, P10–P14, P16–P18, and G4–G6) — the internal-error path (`500 {ok:false, reason:'internal'}` / route-specific message) has **no confirmed dedicated test**; these fire only when a `core.*` Promise rejects, which the domain tests may not force. **Disposition: OPEN** as a fault-injection-only path (unreachable from well-formed requests; deliberately not faked).
3. **Bun global `maxRequestBodySize` fast-413 (> 14e6, before `fetch`)** — CITED from `bun-serve-runtime-limits`; **no in-repo test** exercises a body over the 14e6 global ceiling (the in-handler caps at 1e6 / 14e6 are covered by `csrf-guard.test.ts`/`paste-image.test.ts`, but the pre-`fetch` bodyless 413 is not). **Disposition: COVERED** by `tests/p6-http-freeze.test.ts` (observed wire form recorded in §8).
4. **`/api/settings` gateway_* bearer gate `trustLoopback` waiver** (`1791`–`1794`) — `gateway.test.ts` covers the gate; the specific `trustLoopback && !arrivedViaTrustedProxy && isLoopbackAddress` **waiver branch** is not confirmed tested. **Disposition: COVERED** by `tests/p6-http-freeze.test.ts`.
5. **`GET /health` requiring a bearer off-loopback (LAN)** — `/health` is loopback-open via `authorized` (`885`) and 401s over LAN without a token; `lan-auth.test.ts` exists but this exact `/health`-on-LAN row is not confirmed. **Disposition: STALE** — behavior was already pinned by `tests/lan-auth.test.ts:280-282` (`GET /health` off-loopback → `401 {ok:false,reason:'unauthorized'}`).
6. **`/command` (P7)** — `fleet-command.test.ts` covers the orchestrator command; the HTTP `POST /command` → `200 core.command(text)` transport row is not confirmed via that file. **Disposition: COVERED** by `tests/p6-http-freeze.test.ts`.
7. **`refuse(404)`/`refuse(400)` WS upgrade paths** (`2503`, `2514`, `2516`) — the failed-`srv.upgrade` (400) and unknown-path (404) upgrade refusals; `http-stall.test.ts` covers a *refused* WS upgrade FIN (401 class) but the 400/404 upgrade-refuse rows specifically are not confirmed. **Disposition: COVERED** by `tests/p6-http-freeze.test.ts` (observed statuses recorded in §8).
8. **`WS idleTimeout` behavior** — `bun-serve-runtime-limits` records the WS idleTimeout probe as **INCONCLUSIVE**; app-level liveness (30 s ping / `isAlive`) is the only bound and is covered, but native WS idle behavior remains uncharacterized. **Disposition: OPEN/INCONCLUSIVE** (WS idleTimeout).
9. **`raw-request-timeout.test.ts` is NOT daemon coverage** — it tests the `rawRequest` *helper* against a synthetic `node:http` hanging server; it does not exercise any daemon request-timeout policy. Daemon FIN policy is covered by `http-stall.test.ts` only. (Recorded here so the gap is not masked by the file's name.) **Disposition:** recorded meta-note (unchanged).
10. **Outer/inner handler `catch` 500 branches** (`2132`–`2141`, `2147`–`2154`) — the non-hook thrown-error `500 {err:'internal'}` / `500 {}` fallbacks have no confirmed test. **Disposition: OPEN** as a fault-injection-only path (unreachable from well-formed requests; deliberately not faked).

---

## 8. UNVERIFIED rows (could not be pinned from source alone this pass)

Rows subsequently settled by `tests/p6-http-freeze.test.ts` are marked
**VERIFIED** with the observed wire values. Remaining CITED / INCONCLUSIVE
rows are unchanged.

| Row | Why UNVERIFIED | Disposition |
| --- | --- | --- |
| Exact `~4 s` effective floor of `timeout(request,1)` (refuse FIN) | Empirical Bun uSockets 4 s granularity | CITED `bun-serve-runtime-limits`; not re-probed (per task rule) |
| Global `maxRequestBodySize` > 14e6 → bodyless `413 Connection: close` ~3–7 ms pre-`fetch` | Empirical Bun behavior; now observed on HEAD | **VERIFIED** by `tests/p6-http-freeze.test.ts`: literal bodyless wire response `HTTP/1.1 413 Request Entity Too Large` + `Connection: close` header, distinct from the in-handler JSON-body 413 that keeps the connection alive; caveat — Bun defers the actual FIN (no reliable socket close within 8s), so the pinned contract is the header/terminator, not prompt socket close |
| Native WS `idleTimeout` semantics under `idleTimeout:0` | Prior probe INCONCLUSIVE | CITED as inconclusive; app-liveness is the operative bound |
| Derive-owned status enums for control routes (§7.1) | `derive` contract, not `http.ts`; unknown-id now probed at transport | **VERIFIED** by `tests/p6-http-freeze.test.ts`: unknown-id statuses — kill 404, revive 404, rc 404, adopt 404, dismiss 404, dismiss-retry 404, name 409, questions.answer 404, questions.dismiss 404, plans.mark 404, plans.assign 404 |
| WS-upgrade refusals (`handleUpgrade`) | Was §7.7; not pinned from source alone | **VERIFIED** by `tests/p6-http-freeze.test.ts`: unknown path 404, failed `/ws` and `/ws/term` upgrade 400, tokenless `/ws/term` 401 |
| `Bun.serve` pre-request (header-phase) ~12 s reaper | Empirical, out of daemon reach | CITED `bun-serve-runtime-limits` |

Everything in §1–§6 not listed here is **verified from `src/daemon/http.ts` source** at HEAD `67758ba9`.

---

## 9. Contradictions with the plan / status docs (flag explicitly)

### 9.1 PRIMARY — WS `send()` return values are assumed load-bearing; they are not.

`effect-migration-status.md` §"P6 preflight constraints" (`211`): *"Characterize Bun
WebSocket `send()` return values before defining the new backpressure policy."* The
migration plan's P6.5 similarly frames preserving native `send()` results
(`-1` backpressure / `0` drop / positive bytes) as a requirement.

**Reality:** `http.ts` consults `send()`'s return value **nowhere**. All three send
sites discard it (`2235`, `2272`, `2358`); backpressure is decided *only* by
`getBufferedAmount()` against `MAX_WS_BUFFER` (1 MiB, `2227`) and `MAX_TERM_WS_BUFFER`
(4 MiB, `2350`), with over-cap peers **evicted** (`terminate()` for snapshot, `close(1009)`
for term), not throttled by a send-result. The P6 backpressure policy to preserve is
the `getBufferedAmount()` threshold + eviction model, not a `send()`-return contract.
Characterizing `send()` returns is still fine as due-diligence, but no current
behavior depends on them — the constraint as written targets a mechanism the daemon
does not use.

### 9.2 `GET /state` vs `/ws` snapshot are NOT identical ("parity" caveat).

The ledger lists P6 focused tests as *"Raw HTTP/WS parity …"*. But `GET /state`
returns `snapshotWithLan()` (token-bearing `lan.urls`/`lan.mdns`, `1480`/`771`)
while the `/ws` frame returns `core.snapshot()` **without** any `lan` data (H-S1,
`2200`–`2209`). Any P6 "parity" assertion must treat this divergence as
intentional and preserved, not a bug to unify.

### 9.3 Memory `bun-serve-runtime-limits` is stale on A1 (memory-vs-code, not plan/status).

The memory marks A1 (stalled-request FIN) **"DEFERRED"**, but the code has fully
implemented it: `BODY_STALL_FIN_S`/`boundStalledDrain`/`clearStalledFin`
(`72`–`75`, `278`–`303`) plus the keep-alive-idle FIN `KEEPALIVE_FIN_S`/`end()`
(`94`–`97`, `215`–`222`). The empirical Bun facts in that memory remain valid and
are the cited authority above; only its A1 status line is out of date. Flagged so a
P6 reader does not treat the FIN machinery as absent.

### 9.4 Confirmations (no contradiction) worth stating for P6.

- **"Adapt and join every legacy async route Promise before enabling request
  interruption"** (`status:207`) is consistent with the code: 12 GET/POST routes are
  async Promises the transport joins via `res.done` + `activeResponses` (§1); the
  rest are synchronous through the `json()` helper. The async set is exactly:
  `GET /api/worktrees`, `GET fs` (session+home), `POST /mail`, `/api/cleanup`,
  `/api/worktrees/remove`, `/api/repos/preflight`, `/api/spawn`, `/api/spawn/:id/kill`,
  `/api/spawn/:id/revive`, `/api/sessions/:sid/adopt`, `/api/sessions/:sid/dismiss`,
  `/api/sessions/:sid/dismiss/retry`, `/api/spawn/:id/rc`.
- **Held-response barriers + `stop(false)`/`stop(true)` ordering** (`status:213`)
  match §6 exactly.

---

## Route count

- **GET:** 11 dispatch rows (G1–G11), of which 2 are the CSRF-walled mutating GETs (`/mail`, `/api/watch`).
- **POST:** 22 dispatched routes (P1–P22) + fall-through `404` (P23).
- **WebSocket upgrade:** 2 endpoints (`/ws`, `/ws/term`) + refusal paths.
- **Total distinct HTTP endpoints:** ~33 (11 GET + 22 POST) plus 2 WS.

*End of frozen matrix. Anchors valid at HEAD `67758ba9` (`src/daemon/http.ts`, 3138 lines). This is the only file written for P6.1; it is left uncommitted.*
