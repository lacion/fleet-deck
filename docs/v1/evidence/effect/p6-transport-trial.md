# P6.7 — BunHttpServer + HttpRouter transport trial

**Purpose.** Record the independent P6.7 trial of `effect/unstable/http/HttpRouter` plus
`@effect/platform-bun/BunHttpServer` against the frozen HTTP/WS matrix, and the resulting
**KEEP CUSTOM ADAPTER** decision. The trial is of BunHttpServer as the *transport*, not of
HttpRouter-as-library in front of the existing scoped `Bun.serve` adapter.

**Provenance / how to read this.** Distilled from the spike's canonical write-up
`spike/p6-7/REPORT.md` and the raw dumps `spike/p6-7/out/evidence.json` (first-pass matrix +
bundle + load) and `spike/p6-7/out/extra-evidence.json` (HOLD_STARTED shutdown, keep-alive-idle,
WS-held shutdown). The spike lived on branch `fd/p6-7-spike` in a disposable worktree; it did
not modify daemon source. Numbers below are transcribed from that report and those JSON dumps.

**Section inventory:** §0 identity · §1 verdict and continuation-rule framing · §2 capability
table · §3 four blocking gaps · §4 byte-drift · §5 bundle and load smoke · §6 shutdown ·
§7 archival spike (scratch).

---

## 0. Identity

| Field | Value |
| --- | --- |
| Date | 2026-08-22 |
| Cohort | `effect@4.0.0-rc.110` / `@effect/platform-bun@4.0.0-rc.110` / bun 1.3.14 |
| Product branch (spike parent) | `fd/v1-effect-feasibility` @ `3735759e` (`test(effect): retarget the cache-contract tripwire to http-policy`) |
| Spike branch | `fd/p6-7-spike` @ `3cf7aa29337f640c9b213abfa8e105d9eda0de38` (`spike(effect): P6.7 BunHttpServer vs frozen HTTP/WS matrix`) |
| Spike tree | `/tmp/fd-wt-p6-7` — **scratch, not repo-tracked** |
| Spike code | `spike/p6-7/` (standalone; daemon source untouched) |
| Product tree | `fd/v1-effect-feasibility` was not modified by the trial |

The REPORT.md header still names workspace HEAD `3735759e`; that is the implementation parent.
The archival spike commit is `3cf7aa29`, which adds the write-up on top of that parent.

---

## 1. Verdict and continuation-rule framing

**KEEP CUSTOM ADAPTER.**

Plan default was keep-custom. Switch-transport required every black-box fixture **and** the
shutdown budget, with default shutdown timing overridden to Fleet Deck's
`stop(false)`-once-then-race-then-`stop(true)` machine. At rc.110 three of those are blocking
API gaps and a fourth is only a TS-private field.

The plan already records this outcome as a successful full migration: Effect for all route
workflows while retaining a custom scoped `Bun.serve` adapter. P6.7 does not require switching
transport and does not block P6 continuation. HttpRouter-as-library in front of the custom
adapter was not this trial.

---

## 2. Capability table

| Capability | Status | Key evidence |
| --- | --- | --- |
| Bearer-gated JSON POST (status/body) | **WORKS** | effect `HTTP/1.1 200 OK` body=`{"ok":true,"echo":{"text":"hi"}}`; 401 `{"ok":false,"reason":"unauthorized"}`. Byte-identical to control. |
| Frozen `json()` headers (`content-type`, `content-length`, `nosniff`) | **WORKS** (caller must pass nosniff on `jsonUnsafe`) | Both emit `Content-Type: application/json`, `X-Content-Type-Options: nosniff`, `Content-Length: 32`, plus Bun's `Date`. `jsonUnsafe` does **not** add nosniff by default. |
| 404 `{err:'nope'}` | **EXPRESSIBLE WITH ESCAPE HATCH** (`HttpRouter.add("*","*", ...)`) | Catch-all: `HTTP/1.1 404 Not Found` body=`{"err":"nope"}`. Default `RouteNotFound` → `Response.empty({ status: 404 })`: `HTTP/1.1 404 Not Found` `Content-Length: 0` empty body. Catch-all did **not** shadow named routes (`/health` still 200). |
| In-handler 413 `{ok:false,reason:'payload too large'}` keep-alive | **WORKS** | `HTTP/1.1 413 Payload Too Large` body=`{"ok":false,"reason":"payload too large"}` `connection=null` (keep-alive). |
| Bun global `maxRequestBodySize` bodyless 413 + `Connection: close` | **WORKS** (`ServeOptions` spread into `Bun.serve`) | Both: `HTTP/1.1 413 Request Entity Too Large` `connection=close` `bodyLen=0`. |
| Held / long-poll (`idleTimeout:0`) | **WORKS** | idleTimeout 0, 8s hold: effect 8004ms / control 7991ms, both `{timeoutReachable:true}`. **idleTimeout does not bound in-flight handlers**: default idleTimeout=10 + 12s hold still completed (`ms:12003, status:200`). |
| Per-request `server.timeout(request, N)` | **EXPRESSIBLE WITH ESCAPE HATCH** (TS-private `bunServer`) | `/inspect` `timeoutCalled:"ok"`, `hasBunServer:true`. Keep-alive-idle: `/health` stays open 3s (`closed:false` both). `/arm-fin` with `timeout(req,1)` FINs the socket: effect `closeMs=1944`, control `closeMs=46`. Public API: none. |
| WS upgrade + echo | **WORKS** | effect `open=true` first frame is Socket inspect; control first frame is `{kind,isAlive,getBufferedAmount,...}`. Echo works on both. |
| Custom WS `data` `{kind,isAlive}` | **NOT EXPRESSIBLE** | Adapter `WebSocketOptions` omits `"open"\|"message"\|"close"\|"drain"\|"ping"\|"pong"\|"data"\|"binaryType"`. `upgrade()` always sets Effect-owned context. Inspect: effect `data: null`; control `kind: "snapshot", isAlive: true`. |
| `ws.close(1009, reason)` | **WORKS** | effect `closeCode=1009` reason=`"snapshot too large"`; control identical. Residual: writer `ws.close(code,reason)` then `runRaw` `onExit` also `ws.close(1000\|1011)`; `defaultCloseCodeIsError = () => true`. Double-close was **not** observed — client got 1009. |
| `getBufferedAmount()` | **NOT EXPRESSIBLE** | effect flood `{"tag":"flood-done","getBufferedAmount":"missing"}`; control flood `{"tag":"flood-done","getBufferedAmount":382464}`. |
| `terminate()` / `ws.ping()` / drain | **NOT EXPRESSIBLE** | Socket.make exposes `runRaw` / `run` / `runString` / `writer` only. Writer is sendText/sendBinary/close. WebSocketOptions omits those handlers. |
| Token-gated WS refuse (401 before upgrade) | **WORKS** | Handler-level, before `request.upgrade`. Both: `HTTP/1.1 401 Unauthorized` body=`{"ok":false,"reason":"unauthorized"}`. |
| Failed upgrade (GET `/ws` without Upgrade) | **BYTE-DRIFT** | effect `HTTP/1.1 400 Bad Request` `Content-Length: 0` empty body (`RequestParseError` → `Response.empty({ status: 400 })`). control `HTTP/1.1 400 Bad Request` body=`{"err":"upgrade failed"}`. Daemon freeze is 400. **Not** a 500. |
| `stop(false)`-once-then-race-deadline-then-`stop(true)` | **NOT EXPRESSIBLE** | Adapter is `Effect.promise(() => server.stop())` (no boolean) cached. Default 20s + held HTTP: process **HUNG** (`HOLD_STARTED` confirmed). grace=500ms + held: process **exited 130 in 513ms** — that is `timeoutOrElse` + `runMain` `process.exit`, **not** `stop(true)`. |

---

## 3. Four blocking gaps

Plan switch-transport was gated on every black-box fixture **and** the shutdown budget. These
four gaps are each empirically blocking at rc.110:

1. **Socket surface lacks `getBufferedAmount` / `terminate` / `ping` / drain.** Socket.make
   exposes `runRaw` / `run` / `runString` / `writer` only. The frozen buffered-amount + eviction
   WS contract ([p6-http-matrix.md](./p6-http-matrix.md) §3,
   [p6-ws-send-probe.md](./p6-ws-send-probe.md)) is inexpressible. Flood probe: effect
   `getBufferedAmount: "missing"`; control `getBufferedAmount: 382464`.
2. **`upgrade()` overwrites custom `ws.data`.** Effect always writes its own
   `WebSocketContext {deferred, closeDeferred, buffer, run}`, killing per-viewer state.
   Inspect: effect `data: null`; control `kind: "snapshot", isAlive: true`.
3. **Shutdown cannot express `stop(false)`-once-race-deadline-`stop(true)`.** The adapter's
   grace path is `timeoutOrElse` around a cached `server.stop()` with no boolean, then
   `BunRuntime.runMain` `process.exit(130)`. That violates the never-close-before-`stop(true)`
   invariant. `gracefulShutdownTimeout` is **not** an override of shutdown timing to Fleet
   Deck's budget. See §6.
4. **`server.timeout(request, N)` is reachable only via a TS-private field.**
   `(request as any).bunServer.timeout(toBunServerRequest(request), N)`. `toBunServerRequest`
   returns the Web `Request` (`.source`) only. The hatch is real (`timeoutCalled:"ok"`; FIN
   `closeMs=1944`) and unsupported.

---

## 4. Byte-drift

Frozen daemon `json()` writes lowercase header *keys* on the `Headers` object (`content-type`,
`content-length`, `x-content-type-options: nosniff`) via `HttpResShim` →
`new Response(payload, {status, headers})`. Bun serializes them **Title-Case** on the wire
(`Content-Type`, `X-Content-Type-Options`, `Date`) for **both** effect and control — not drift
versus the daemon (same Bun `Response` path). The original generated report's "lowercase on the
wire" claim was wrong.

Observed drift versus the frozen daemon bodies:

- default 404 effect: `HTTP/1.1 404 Not Found` `Content-Length: 0` empty body. Daemon freeze is
  JSON `{err:'nope'}` via the catch-all hatch, not the adapter default.
- failed upgrade effect: empty `HTTP/1.1 400 Bad Request`. control / daemon freeze:
  `HTTP/1.1 400 Bad Request` body=`{"err":"upgrade failed"}`.

Not drift (same on both sides, or a Bun option rather than an Effect API):

- command-ok: `HTTP/1.1 200 OK | Content-Type: application/json | X-Content-Type-Options: nosniff | Date: … | Content-Length: 32`
- catch-all 404: `HTTP/1.1 404 Not Found` body=`{"err":"nope"}`
- in-handler 413: `HTTP/1.1 413 Payload Too Large` body=`{"ok":false,"reason":"payload too large"}` connection unset
- global 413 both: `HTTP/1.1 413 Request Entity Too Large` `Connection: close` `bodyLen=0`

Bun's pre-fetch fast-413 is a `Bun.serve` option, not an Effect API. rc.110 spreads
`maxRequestBodySize` through — enough for that fixture.

Keep-alive-idle extra probe: `idleTimeout:0` (daemon-like) leaves a keep-alive `/health` socket
open after 3s of silence (`closed:false` both). Default BunHttpServer does **not** arm the
daemon's `KEEPALIVE_FIN_S` after `end`. The private-field hatch **does** FIN (`GET /arm-fin`
arms `timeout(req, 1)`): effect 1944ms, control 46ms. Timing is Bun 1.3.14 `timeout(N)` behavior
(the documented ~4s floor is for other N; not an Effect API question).

---

## 5. Bundle and load smoke

Scratch bundle used the daemon esbuild flags (`--bundle --platform=node --format=esm
--external:bun:sqlite --minify-identifiers --minify-syntax --keep-names --charset=utf8`), then
PURE-comment strip. gzip = `Bun.gzipSync(..., {level:9, library:'zlib'})`.

| Artifact | raw bytes | gzip | ok |
| --- | ---: | ---: | --- |
| HttpRouter+BunHttpServer scratch | 435084 | 118319 | true |
| control json() helper scratch | 456 | 317 | true |
| delta (spike − control) | 434628 | 118002 | |
| existing daemon `fleetd.bundle.mjs` (context at spike parent `3735759e`) | 652488 | 188720 | |

Pulling the platform adapter into the daemon bundle is +434,628 B raw / +118,002 B gzip on top
of the then-current daemon. That gzip delta **alone exceeds the gzip ceiling**. Not the blocking
reason, but not free.

Load smoke (200 parallel GET `/health`; not a formal benchmark):

| Server | ok/fail | wall ms | rps | p50 | p95 | p99 | max |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| effect | 200/0 | 170 | 1174 | 111.11 | 168.94 | 169.33 | 169.47 |
| control | 200/0 | 107 | 1876 | 70.38 | 104.94 | 105.04 | 106.22 |

Fiber-per-request (BunHttpServer forks a fiber per fetch, interrupt on abort) is visible as
extra p95 (1,174 vs 1,876 rps). Overhead is not the blocking reason to keep-custom.

---

## 6. Shutdown

Source (`node_modules/@effect/platform-bun/src/BunHttpServer.ts`):

```ts
const shutdown = yield* Effect.promise(() => server.stop()).pipe(Effect.cached)
const preemptiveShutdown = options.disablePreemptiveShutdown ? Effect.void : Effect.timeoutOrElse(shutdown, {
  duration: options.gracefulShutdownTimeout ?? Duration.seconds(20),
  orElse: () => Effect.void
})
yield* Scope.addFinalizer(scope, shutdown) // awaits the SAME cached graceful stop — no stop(true)
```

Fleet Deck freeze: initiate `stop(false)` **once**, race the budget, then `stop(true)` if
graceful loses; never await two serially; never close/terminate native `ServerWebSocket` before
`stop(true)` on bun 1.3.14. The adapter cannot express that state machine.

Extra probes wait for `HOLD_STARTED` before SIGINT. Control `/api/watch` is abortable on
`req.signal` (first-pass hang was a non-abortable `Bun.sleep` plus no handshake — do not read
that as "the daemon pattern fails").

| Probe | HOLD_STARTED | exit | ms | What actually happened |
| --- | --- | --- | ---: | --- |
| effectDefaultGraceHeld | yes | HUNG | 4001 (probe budget; first-pass 11988) | Default 20s `timeoutOrElse(server.stop())` waits on graceful drain. In-flight `Effect.sleep` is not aborted. Process still alive when we SIGKILL. |
| effectGrace500Held | yes | 130 | 513 | `timeoutOrElse` fires at 500ms, then `BunRuntime.runMain` `process.exit(130)`. Client: connection reset. **Not** `stop(true)`. |
| effectDisablePreemptiveHeld | yes | 130 | 12 | No preemptive wait. runMain exits immediately. In-flight got `503` empty (fiber interrupt) then process death. Still no `stop(true)`. |
| effectGrace500NoHold | no | 130 | 11 | Empty server: `stop()` resolves, process.exit 130. |
| controlDeadline500Held | yes | 0 | 4505 | `stop(false)` issued; race=deadline at 501ms; `stop(true)` then **hung 4s** (force-hung wrapper) on the in-flight fetch; `process.exit(0)` anyway. State machine **is** expressible on raw Bun.serve; `stop(true)` linger on held HTTP is a Bun 1.3.14 fact the daemon already races around. |
| controlDeadline500NoHold | no | 0 | 3 | `CONTROL_STOP graceful after 0ms`. |
| effectWsHeld (grace=500) | n/a | 130 | 517 | WS opened; process.exit 130; client close **1011** (adapter `onExit` failure close). |
| controlWsHeld (deadline=500) | n/a | 0 | 504 | `stop(false)` → deadline 500ms → `stop(true)=forced` immediately; client close **1006** "Connection ended". This is the daemon WS force-path. |

`process.exit(130)` after a grace timeout is **not** a substitute for `stop(true)`. It abandons
sockets to the kernel and cannot implement the freeze's "never close/terminate native
ServerWebSocket before `stop(true)`" rule.

---

## 7. Archival spike (scratch)

```
spike/p6-7/
  server.ts               Effect HttpRouter + BunHttpServer
  control-server.ts       raw Bun.serve (json helper + stop(false)/stop(true) race)
  bundle-entry.ts         scratch imports for esbuild
  control-bundle-entry.ts
  run.ts                  first-pass matrix + bundle + load
  probe-extra.ts          HOLD_STARTED shutdown, keep-alive-idle, WS-held
  REPORT.md               canonical write-up
  out/evidence.json       raw first-pass + extra merged under .extra
  out/extra-evidence.json extra probe data
```

Branch `fd/p6-7-spike` @ `3cf7aa29` is the archival commit. The worktree path
`/tmp/fd-wt-p6-7` is scratch and may already be gone; do not treat it as a durable input.
Daemon source was not modified. Nothing was pushed. The user fleetdeck on :4711 was not
touched.
